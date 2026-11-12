//! Pulling captures off the device: the port of `CaptureSync` and `Bridge::startPull` /
//! `recordCapture` (backend/chymaera). Only the capture folders leave the device (secrets such
//! as `/ext/u2f`, `/int/.bt.keys` or `apps_data/totp` are never read), `assets` and `.cache`
//! below them are skipped, every file is hashed, recorded once per content and device path,
//! and the steps are logged to the datastore's event table as the C++ app logs them.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use flipper_rpc::ops::FileKind;
use flipper_rpc::transfer::Limits;
use flipper_rpc::transport::Transport;
use flipper_rpc::{RpcError, Session};

use crate::meta;
use crate::store::{Capture, Severity, Source, Store, StoreError};

/// The capture folders (`CaptureSync::defaultRoots`).
pub const CAPTURE_ROOTS: &[&str] = &[
    // stock apps' saved captures
    "/ext/nfc",
    "/ext/subghz",
    "/ext/lfrfid",
    "/ext/infrared",
    "/ext/ibutton",
    // Wi-Fi devboard (Marauder companion app)
    "/ext/apps_data/marauder/pcaps",
    "/ext/apps_data/marauder/logs",
    // Argus firmware apps
    "/ext/rf_monitor",
    "/ext/wifi_scan",
    "/ext/ld2450",
];

/// Folders skipped wherever they appear: key dictionaries, keeloq manufacturer keys, caches.
const EXCLUDED: &[&str] = &["assets", ".cache"];

/// `QDir::cleanPath` for an absolute path with no `..`: empty and `.` parts dropped.
fn clean(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    format!("/{}", parts.join("/"))
}

/// `CaptureSync::isCaptureRoot`: a capture folder or a path inside one.
pub fn is_capture_root(path: &str) -> bool {
    if !path.starts_with('/') || path.split('/').any(|p| p == "..") || path.contains('\0') {
        return false;
    }
    let c = clean(path);
    CAPTURE_ROOTS.iter().any(|root| c == *root || c.starts_with(&format!("{root}/")))
}

/// `CaptureSync::isExcluded`.
pub fn is_excluded(path: &str) -> bool {
    path.split('/').any(|seg| EXCLUDED.contains(&seg))
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PullReport {
    pub new: u64,
    pub duplicates: u64,
    pub failed: Vec<(String, String)>,
    pub missing_roots: Vec<String>,
    /// Bytes of the files read, by their listed sizes (as the C++ app counts).
    pub bytes: u64,
    pub staging: PathBuf,
    /// Summaries of the new captures, as logged.
    pub recorded: Vec<String>,
}

#[derive(Debug)]
pub enum PullError {
    /// Not a capture folder; nothing was asked of the device.
    NotAllowed(String),
    Store(StoreError),
    Local(String),
    /// The session broke (a frame error, a timeout): the pull stopped there.
    Rpc(RpcError),
}

impl std::fmt::Display for PullError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PullError::NotAllowed(p) => write!(f, "only the capture folders may be pulled, not {p}: {}", CAPTURE_ROOTS.join(", ")),
            PullError::Store(e) => write!(f, "{e}"),
            PullError::Local(m) => write!(f, "{m}"),
            PullError::Rpc(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PullError {}

impl From<StoreError> for PullError {
    fn from(e: StoreError) -> PullError {
        PullError::Store(e)
    }
}

/// UTC `yyyyMMdd-hhmmss` (the C++ app names the folder in local time).
fn stamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // days since 1970-01-01 to a civil date (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem / 60 % 60, rem % 60)
}

fn mkdirs(dir: &Path) -> Result<(), PullError> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| PullError::Local(format!("{}: {e}", dir.display())))
}

/// A writer that hashes what passes through it.
struct Hashing<W: Write> {
    inner: W,
    hash: Sha256,
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Removes empty folders below and including `dir`, deepest first.
fn remove_empty_dirs(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                remove_empty_dirs(&e.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// Where a pull reports its steps: (severity, source, message, detail).
pub type Log<'a> = &'a mut dyn FnMut(Severity, Source, &str, &str) -> Result<(), StoreError>;

/// Pulls `roots` (each a capture folder or inside one) into a new staging folder next to the
/// datastore (`pulls/<UTC time>`), records new captures and logs every step to the datastore. A
/// file that cannot be read is reported and the pull goes on, as in the C++ app; a broken session
/// stops it.
pub fn pull<T: Transport>(
    session: &mut Session<T>,
    store: &Store,
    roots: &[&str],
    label: &str,
    limits: &Limits,
) -> Result<PullReport, PullError> {
    store.log_event(Severity::Notice, Source::Device, &format!("Pulling {label} from device…"), "")?;
    let mut log = |sev: Severity, src: Source, msg: &str, detail: &str| store.log_event(sev, src, msg, detail);
    pull_logged(session, store, None, roots, label, limits, &mut log)
}

/// [`pull`] without its first line ("Pulling LABEL from device…", which the caller logs when it
/// starts the pull), recording captures in `session_id` and handing every step to `log`.
pub fn pull_logged<T: Transport>(
    session: &mut Session<T>,
    store: &Store,
    session_id: Option<i64>,
    roots: &[&str],
    label: &str,
    limits: &Limits,
    log: Log,
) -> Result<PullReport, PullError> {
    if let Some(bad) = roots.iter().find(|r| !is_capture_root(r)) {
        return Err(PullError::NotAllowed((*bad).to_string()));
    }
    let base = store.path().parent().unwrap_or(Path::new(".")).join("pulls");
    mkdirs(&base)?;
    let mut staging = base.join(stamp());
    let mut n = 1;
    while DirBuilder::new().mode(0o700).create(&staging).is_err() {
        n += 1;
        if n > 100 {
            return Err(PullError::Local(format!("cannot create a staging folder in {}", base.display())));
        }
        staging = base.join(format!("{}-{n}", stamp()));
    }
    let mut report = PullReport { staging: staging.clone(), ..Default::default() };

    let result = pull_roots(session, store, session_id, roots, &staging, limits, &mut report, log);

    let ok = result.is_ok() && report.failed.is_empty();
    log(
        if ok { Severity::Notice } else { Severity::Warning },
        Source::Device,
        &format!(
            "Pulled {label}: {} new, {} already stored, {} failed ({} bytes read)",
            report.new,
            report.duplicates,
            report.failed.len(),
            report.bytes
        ),
        &staging.display().to_string(),
    )?;
    // Leave no empty staging folders behind when everything was a duplicate.
    remove_empty_dirs(&staging);
    result.map(|_| report)
}

#[allow(clippy::too_many_arguments)]
fn pull_roots<T: Transport>(
    session: &mut Session<T>,
    store: &Store,
    session_id: Option<i64>,
    roots: &[&str],
    staging: &Path,
    limits: &Limits,
    report: &mut PullReport,
    log: Log,
) -> Result<(), PullError> {
    let mut queue = Vec::new();
    for root in roots {
        let root = clean(root);
        match session.walk(&root, limits) {
            Ok(tree) => {
                for path in tree.refused {
                    let reason = "refused: the device sent a name that leads outside the staging folder".to_string();
                    log(Severity::Warning, Source::Device, &format!("Could not read {path}"), &reason)?;
                    report.failed.push((path, reason));
                }
                queue.extend(tree.entries.into_iter().filter(|e| e.kind == FileKind::File && !is_excluded(&e.path)));
            }
            Err(RpcError::Device(_)) | Err(RpcError::UnknownStatus(_)) | Err(RpcError::Invalid(_)) => {
                log(Severity::Info, Source::Device, &format!("Skipped {root}: not on the device or unreadable"), "")?;
                report.missing_roots.push(root);
            }
            Err(e) => return Err(PullError::Rpc(e)),
        }
    }

    for entry in queue {
        // Every part of the path was checked by the walk (transfer::safe_name).
        let local = entry.path.trim_start_matches('/').split('/').fold(staging.to_path_buf(), |p, part| p.join(part));
        if let Some(parent) = local.parent() {
            mkdirs(parent)?;
        }
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&local)
            .map_err(|e| PullError::Local(format!("{}: {e}", local.display())))?;
        let mut w = Hashing { inner: file, hash: Sha256::new() };
        match session.storage_read(&entry.path, &mut w, limits.max_file) {
            Ok(_) => {}
            Err(e) => {
                drop(w);
                let _ = std::fs::remove_file(&local);
                log(Severity::Warning, Source::Device, &format!("Could not read {}", entry.path), &e.to_string())?;
                let broken =
                    matches!(e, RpcError::Broken | RpcError::Io(_) | RpcError::Timeout(_) | RpcError::Closed(_) | RpcError::Frame(_));
                report.failed.push((entry.path.clone(), e.to_string()));
                if broken {
                    return Err(PullError::Rpc(e));
                }
                continue;
            }
        }
        let sha256 = hex::encode(w.hash.finalize());
        w.inner.sync_all().map_err(|e| PullError::Local(format!("{}: {e}", local.display())))?;
        drop(w.inner);
        report.bytes += u64::from(entry.size);

        if store.has_capture(&sha256, &entry.path)? {
            report.duplicates += 1;
            let _ = std::fs::remove_file(&local); // already kept from an earlier pull
            continue;
        }
        let name = entry.path.rsplit('/').next().unwrap_or_default().to_string();
        let size = std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
        let mut data = Vec::new();
        if size as usize <= meta::MAX_PARSE {
            File::open(&local)
                .and_then(|mut f| f.read_to_end(&mut data))
                .map_err(|e| PullError::Local(format!("{}: {e}", local.display())))?;
        }
        let (kind, meta_json) = if size as usize <= meta::MAX_PARSE {
            meta::describe(&name, &data)
        } else {
            (meta::raw_kind(&name).to_string(), format!(r#"{{"kind":"{}"}}"#, meta::raw_kind(&name)))
        };
        let summary = meta::summary(&name, size, &meta_json);
        store.add_capture_in(
            session_id,
            &Capture {
                kind,
                name,
                source_path: entry.path.clone(),
                size,
                meta: meta_json,
                sha256,
                local_path: local.display().to_string(),
            },
        )?;
        log(Severity::Info, Source::Capture, &format!("Recorded capture: {summary}"), &entry.path)?;
        report.recorded.push(summary);
        report.new += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_roots() {
        assert!(is_capture_root("/ext/nfc"));
        assert!(is_capture_root("/ext/nfc/"));
        assert!(is_capture_root("/ext//nfc/./sub"));
        assert!(is_capture_root("/ext/apps_data/marauder/pcaps"));
        assert!(!is_capture_root("/ext/nfc/../u2f"));
        assert!(!is_capture_root("/ext/u2f"));
        assert!(!is_capture_root("/int"));
        assert!(!is_capture_root("/ext/nfcx"));
        assert!(!is_capture_root("ext/nfc"));
        assert!(!is_capture_root("/ext/apps_data/totp"));
    }

    #[test]
    fn excluded() {
        assert!(is_excluded("/ext/nfc/assets/mf_classic_dict_user.nfc"));
        assert!(is_excluded("/ext/subghz/.cache/x"));
        assert!(!is_excluded("/ext/nfc/assets.nfc"));
    }

    #[test]
    fn stamp_shape() {
        let s = stamp();
        assert_eq!(s.len(), 15);
        assert_eq!(&s[8..9], "-");
        assert!(s.starts_with("20"));
    }
}
