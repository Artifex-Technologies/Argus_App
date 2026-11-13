//! The Argus tools window: the port of the Qt app's `ArgusToolsWindow.qml` and `ArgusController`
//! (`backend/argus/`): apps from a firmware package (their compatibility with the device's firmware API, installed
//! to `/ext/apps`), the check-up of the apps on the card, a rebuild in Limbo, SD card snapshots (take, compare,
//! restore, delete) and the capture library (read the card's capture folders, search, explain, tag, duplicates).
//!
//! The Qt app runs `argus-tools` as a process and talks to the card through `DeviceStorage`; here the same library
//! (`argus_tools`) runs in process, and the card is read and written through the app's device worker (`Cmd::Run`), one
//! request at a time, between the UI's own. Every step that changes the card (an install, a restore) runs only after
//! its confirmation dialog in `app.rs`; deleting a snapshot (local data) asks too. Files are always written to new
//! staging folders (0700) or the tools' own store; names from the device go through `argus_tools::paths`.

use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use argus_tools::api::{FirmwareApi, Status};
use argus_tools::capture::{self, Builder, Capture, DupeGroup, Library};
use argus_tools::package::{self, AppEntry, Limits as PackageLimits, Report};
use argus_tools::snapshot::{self, Diff, ListedFile, Listing, RestorePlan, Snapshots, Summary};
use argus_tools::{limbo, paths};
use flipper_rpc::ops::FileKind;
use flipper_rpc::transfer::Limits;
use flipper_rpc::transport::Transport;
use flipper_rpc::{RpcError, Session};
use serde_json::{json, Value};

pub type Sess = Session<Box<dyn Transport>>;

/// The firmware API the Qt app assumes until the device says (Argus firmware, Flipper Zero f7).
pub const DEFAULT_API: &str = "88.2/7";
/// The largest app and capture file read from the card (the Qt controller's limits).
const MAX_APP_BYTES: u64 = 8 << 20;
const MAX_CAPTURE_BYTES: u64 = 4 << 20;
/// The largest local file written to the card.
const MAX_WRITE: u64 = 64 << 20;
/// The capture folders the library reads from the card, with their files' suffix (`capture::FOLDERS`).
const CAPTURE_FOLDERS: [(&str, &str); 5] =
    [("/ext/subghz", ".sub"), ("/ext/infrared", ".ir"), ("/ext/nfc", ".nfc"), ("/ext/lfrfid", ".rfid"), ("/ext/ibutton", ".ibtn")];

/// Where the tools keep their data: `$XDG_DATA_HOME/argus` (`ArgusTools::dataDir`), the console's pulls, Limbo.
#[derive(Clone, Debug)]
pub struct Dirs {
    pub data: PathBuf,
    /// The console's capture pulls (`Argus/chymaera/pulls`), indexed with the library.
    pub pulls: PathBuf,
    pub limbo_socket: PathBuf,
}

fn private_dir(p: &Path) -> Result<(), String> {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(p).map_err(|e| format!("{}: {e}", p.display()))
}

impl Dirs {
    pub fn snapshots(&self) -> PathBuf {
        self.data.join("snapshots")
    }
    pub fn library(&self) -> PathBuf {
        self.data.join("captures")
    }
    pub fn apps(&self) -> PathBuf {
        self.data.join("apps")
    }
    /// The private mirror of the card's capture folders, replaced on every read.
    pub fn card_mirror(&self) -> PathBuf {
        self.data.join("captures-card")
    }
    /// A new staging folder (`ArgusTools::newStagingDir`), 0700.
    pub fn staging(&self, tag: &str) -> Result<PathBuf, String> {
        let base = self.data.join("staging");
        private_dir(&base)?;
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        for n in 0..100u32 {
            let dir = base.join(format!("{tag}-{stamp}-{n}"));
            if std::fs::DirBuilder::new().mode(0o700).create(&dir).is_ok() {
                return Ok(dir);
            }
        }
        Err(format!("cannot make a staging folder in {}", base.display()))
    }
}

/// The device's firmware API from its `devinfo` (`firmware.api.major/minor`, `hardware.target`).
pub fn firmware_api(pairs: &[(String, String)]) -> Option<FirmwareApi> {
    let get = |k: &str| pairs.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    FirmwareApi::parse(&format!("{}.{}/{}", get("firmware.api.major")?, get("firmware.api.minor")?, get("hardware.target")?))
}

/// What a job sends back. Every job ends with one reply that is not [`Reply::Status`].
pub enum Reply {
    Status(String),
    Package(Result<Report, String>),
    Plan(Result<Vec<PlanItem>, String>),
    Checkup(Result<Value, String>),
    Rebuild(Result<limbo::BuildReport, String>),
    Snapshots(Result<Vec<Summary>, String>),
    Diff(Result<Diff, String>),
    RestorePlan(Result<RestorePlan, String>),
    Captures(Result<(Vec<Capture>, usize), String>),
    Explain(Result<(Capture, String), String>),
    Dupes(Result<Vec<DupeGroup>, String>),
    /// The end of anything else (an install, a snapshot, a restore, a delete, a read, an index): what to say.
    Done(Result<String, String>),
}

// ------------------------------------------------------------------------------------------------ the card

/// One entry of the card.
#[derive(Clone, Debug)]
pub struct Remote {
    pub path: String,
    pub dir: bool,
    pub size: u64,
}

fn rpc(e: RpcError) -> String {
    e.to_string()
}

/// Every entry below `root` (`DeviceStorage::listTree`); no limit on the total, as the Qt app's.
pub fn list_tree(s: &mut Sess, root: &str) -> Result<Vec<Remote>, String> {
    let limits = Limits { max_file: u64::MAX, max_total: u64::MAX, max_entries: 100_000, max_depth: 32 };
    let tree = s.walk(root, &limits).map_err(rpc)?;
    Ok(tree.entries.into_iter().map(|e| Remote { path: e.path, dir: e.kind == FileKind::Dir, size: u64::from(e.size) }).collect())
}

/// Reads a card file into a new local file (made 0600; removed again if the read fails).
fn read_file(s: &mut Sess, remote: &str, local: &Path, max: u64) -> Result<(), String> {
    if let Some(parent) = local.parent() {
        private_dir(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(local)
        .map_err(|e| format!("{}: {e}", local.display()))?;
    let r = s.storage_read(remote, &mut f, max);
    if r.is_err() {
        let _ = std::fs::remove_file(local);
    }
    r.map(|_| ()).map_err(rpc)
}

/// Reads `paths` into the mirror `root` (`root/ext/...`): how many failed. A name that would lead outside the mirror
/// counts as failed.
fn download_all(s: &mut Sess, list: &[String], root: &Path, status: &mut dyn FnMut(String)) -> usize {
    let mut failed = 0;
    for (i, remote) in list.iter().enumerate() {
        status(format!("Reading {remote} ({} of {})", i + 1, list.len()));
        let ok = match paths::mirror_path(root, remote) {
            Some(local) => read_file(s, remote, &local, MAX_APP_BYTES.max(MAX_CAPTURE_BYTES).max(256 << 20)).is_ok(),
            None => false,
        };
        failed += usize::from(!ok);
    }
    failed
}

/// Makes `dir` and its parents on the card (`DeviceStorage::mkpath`); a file in the way is an error.
fn mkpath(s: &mut Sess, dir: &str) -> Result<(), String> {
    if !paths::valid_device_path(dir) {
        return Err(format!("not a path on the card: {dir}"));
    }
    let mut at = String::new();
    for (i, part) in dir.split('/').skip(1).enumerate() {
        at.push('/');
        at.push_str(part);
        if i == 0 {
            continue; // /ext itself
        }
        match s.storage_stat(&at).map_err(rpc)? {
            Some(e) if e.kind == FileKind::Dir => {}
            Some(_) => return Err(format!("{at} is a file")),
            None => s.storage_mkdir(&at).map_err(rpc)?,
        }
    }
    Ok(())
}

/// A file to write: the local copy, where it goes, and its MD5 to check on the card afterwards (empty: no check).
#[derive(Clone, Debug)]
pub struct WriteItem {
    pub local: PathBuf,
    pub remote: String,
    pub md5: String,
}

/// `ArgusController::writeAll`: each file's folder, the file, then the card's MD5 compared with the local one.
fn write_all(s: &mut Sess, items: &[WriteItem], status: &mut dyn FnMut(String)) -> (usize, Vec<String>) {
    let mut written = 0;
    let mut errors = Vec::new();
    for (i, it) in items.iter().enumerate() {
        status(format!("Writing {} ({} of {})", it.remote, i + 1, items.len()));
        let r = (|| {
            if !paths::valid_device_path(&it.remote) {
                return Err("not writable".to_string());
            }
            let data = crate::device::read_local(&it.local, MAX_WRITE)?;
            let parent = it.remote.rsplit_once('/').map(|(p, _)| p).unwrap_or("/ext");
            mkpath(s, parent).map_err(|e| format!("cannot create the folder ({e})"))?;
            s.storage_write(&it.remote, &data).map_err(rpc)?;
            if !it.md5.is_empty() && !s.storage_md5(&it.remote).map_err(rpc)?.eq_ignore_ascii_case(&it.md5) {
                return Err("the card's copy does not match (MD5)".into());
            }
            Ok(())
        })();
        match r {
            Ok(()) => written += 1,
            Err(e) => errors.push(format!("{}: {e}", it.remote)),
        }
    }
    (written, errors)
}

/// The card under `/ext` as argus-tools' listing (`listingOf`), with the MD5s asked for.
fn listing_of(entries: &[Remote], md5s: &std::collections::HashMap<String, String>) -> Listing {
    let mut l = Listing { root: "/ext".into(), files: Vec::new(), dirs: Vec::new() };
    for e in entries.iter().filter(|e| paths::valid_device_path(&e.path)) {
        if e.dir {
            l.dirs.push(e.path.clone());
        } else {
            l.files.push(ListedFile { path: e.path.clone(), size: e.size, md5: md5s.get(&e.path).cloned() });
        }
    }
    l
}

fn err(e: argus_tools::Error) -> String {
    e.message
}

// ------------------------------------------------------------------------------------------------ apps

/// `scanPackage`: the apps of a package, stored under the apps folder, each judged against `fw`.
pub fn scan_package(dirs: &Dirs, package: &Path, fw: FirmwareApi) -> Result<Report, String> {
    private_dir(&dirs.data)?;
    package::scan(package, &dirs.apps(), PackageLimits::default(), Some(fw)).map_err(err)
}

/// One app of an install plan (`prepareInstall`).
#[derive(Clone, Debug, PartialEq)]
pub struct PlanItem {
    pub local: PathBuf,
    pub remote: String,
    pub name: String,
    pub md5: String,
    pub exists: bool,
    /// Ticked by the user for an app already on the card.
    pub overwrite: bool,
    pub refused: bool,
    pub reason: String,
}

/// `prepareInstall`: which apps can go where, then (on the card) which are already there.
pub fn plan_install(s: &mut Sess, apps: &[AppEntry], force: bool, fw: &str) -> Vec<PlanItem> {
    let mut plan = Vec::new();
    for a in apps {
        let remote = format!("/ext/apps/{}/{}", a.category, a.file_name);
        let compat = a.compat.as_ref();
        let mut item = PlanItem {
            local: a.stored.clone(),
            remote: remote.clone(),
            name: a.manifest.as_ref().map(|m| m.name.clone()).unwrap_or_else(|| a.file_name.clone()),
            md5: a.md5.clone(),
            exists: false,
            overwrite: false,
            refused: false,
            reason: compat.map(|c| c.reason.clone()).unwrap_or_default(),
        };
        let refusal = if compat.is_some_and(|c| c.blocked) {
            Some(format!("refused: {}", item.reason))
        } else if compat.map(|c| c.status) != Some(Status::Compatible) && !force {
            Some(format!("not compatible with API {fw} ({}); tick Force to install anyway", compat.map(|c| c.code).unwrap_or("unknown")))
        } else if !paths::valid_device_path(&remote) || !remote.to_ascii_lowercase().ends_with(".fap") {
            Some("refused: not a path under /ext/apps".into())
        } else if !a.stored.is_file() {
            Some("refused: the extracted copy is missing".into())
        } else {
            None
        };
        if let Some(r) = refusal {
            item.refused = true;
            item.reason = r;
        } else {
            match s.storage_stat(&remote) {
                Ok(Some(e)) => {
                    item.exists = true;
                    if e.kind == FileKind::Dir {
                        item.refused = true;
                        item.reason = "refused: a folder has that name on the card".into();
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    item.refused = true;
                    item.reason = format!("cannot look at the card: {e}");
                }
            }
        }
        plan.push(item);
    }
    plan
}

/// `runInstall`: the plan's apps that are not refused, and of those on the card only the ticked ones.
pub fn install(s: &mut Sess, plan: &[PlanItem], status: &mut dyn FnMut(String)) -> Result<String, String> {
    let (mut kept, mut refused) = (0, 0);
    let mut todo = Vec::new();
    for p in plan {
        if p.refused {
            refused += 1;
        } else if p.exists && !p.overwrite {
            kept += 1;
        } else {
            todo.push(WriteItem { local: p.local.clone(), remote: p.remote.clone(), md5: p.md5.clone() });
        }
    }
    let (written, errors) = write_all(s, &todo, status);
    let mut msg = format!("Installed {written}, kept {kept} existing, refused {refused}, failed {}", errors.len());
    if !errors.is_empty() {
        msg.push_str(": ");
        msg.push_str(&errors.iter().take(5).cloned().collect::<Vec<_>>().join("; "));
        return Err(msg);
    }
    Ok(msg)
}

/// `runCheckup`: the card's `.fap` files read into a staging mirror, then `argus_tools::checkup`.
pub fn checkup(s: &mut Sess, dirs: &Dirs, fw: FirmwareApi, packages: &[String], status: &mut dyn FnMut(String)) -> Result<Value, String> {
    let staging = dirs.staging("checkup")?;
    // a card without /ext/apps has no apps
    let faps: Vec<String> = list_tree(s, "/ext/apps")
        .unwrap_or_default()
        .into_iter()
        .filter(|e| !e.dir && e.path.to_ascii_lowercase().ends_with(".fap") && e.size <= MAX_APP_BYTES && paths::valid_device_path(&e.path))
        .map(|e| e.path)
        .collect();
    let failed = download_all(s, &faps, &staging, status);
    status(format!("Reading {} app manifests…", faps.len()));
    let r = argus_tools::checkup::run(&staging, fw, packages, Some(&dirs.apps()), &mut |m| status(m.to_string())).map_err(err);
    let _ = std::fs::remove_dir_all(&staging);
    let mut v = r?;
    v["unread"] = json!(failed);
    Ok(v)
}

/// `rebuild`: a local folder or an https git URL built with ufbt in Limbo (its own sealed runs).
pub fn rebuild(
    dirs: &Dirs,
    source: &str,
    git_ref: &str,
    subdir: &str,
    fw: FirmwareApi,
    status: &mut dyn FnMut(String),
) -> Result<limbo::BuildReport, String> {
    let out = dirs.apps().join("built");
    let o = limbo::BuildOptions {
        socket: &dirs.limbo_socket,
        build_profile: limbo::BUILD_PROFILE,
        fetch_profile: limbo::FETCH_PROFILE,
        fw,
        out_dir: &out,
        timeout: 900,
        poll: std::time::Duration::from_secs(2),
    };
    let src = if source.starts_with("https://") {
        limbo::Source::Git { url: source, git_ref: (!git_ref.is_empty()).then_some(git_ref), subdir }
    } else {
        limbo::Source::Folder(Path::new(source))
    };
    if !dirs.limbo_socket.exists() {
        return Err("Limbo is not running here (no agent socket); nothing was sent".into());
    }
    limbo::build(src, &o, &mut |m| status(m.to_string())).map_err(err)
}

// ------------------------------------------------------------------------------------------------ snapshots

pub fn snapshots(dirs: &Dirs) -> Result<Vec<Summary>, String> {
    if !dirs.snapshots().join("snapshots").is_dir() {
        return Ok(Vec::new());
    }
    let snaps = Snapshots::open(&dirs.snapshots()).map_err(err)?;
    let mut list: Vec<Summary> = snaps.list().map_err(err)?.iter().map(snapshot::summary).collect();
    list.sort_by(|a, b| b.created_unix.cmp(&a.created_unix).then(b.id.cmp(&a.id)));
    Ok(list)
}

fn open_store(dirs: &Dirs) -> Result<Snapshots, String> {
    private_dir(&dirs.data)?;
    Snapshots::open(&dirs.snapshots()).map_err(err)
}

/// MD5s of the card's files whose path and size match `sizes` (only the card's MD5 can tell them apart).
fn md5s_of(
    s: &mut Sess,
    entries: &[Remote],
    same: impl Fn(&Remote) -> bool,
    status: &mut dyn FnMut(String),
) -> std::collections::HashMap<String, String> {
    let todo: Vec<&Remote> = entries.iter().filter(|e| !e.dir && same(e)).collect();
    let mut out = std::collections::HashMap::new();
    for (i, e) in todo.iter().enumerate() {
        status(format!("Comparing {} ({} of {})", e.path, i + 1, todo.len()));
        if let Ok(m) = s.storage_md5(&e.path) {
            out.insert(e.path.clone(), m);
        }
    }
    out
}

/// `takeSnapshot`: the card listed, unchanged files confirmed by their MD5, the rest read, then stored.
pub fn take_snapshot(s: &mut Sess, dirs: &Dirs, label: &str, fw: &str, status: &mut dyn FnMut(String)) -> Result<String, String> {
    let snaps = open_store(dirs)?;
    let staging = dirs.staging("snapshot")?;
    let r = (|| {
        status("Listing the card…".into());
        let entries = list_tree(s, "/ext").map_err(|e| format!("cannot list the card: {e}"))?;
        let listing = listing_of(&entries, &Default::default());
        let plan = snapshot::plan(&snaps, &listing, None).map_err(err)?;
        let mut download = plan.download.clone();
        let mut md5s = std::collections::HashMap::new();
        for (i, v) in plan.verify_md5.iter().enumerate() {
            status(format!("Checking unchanged files ({} of {})", i + 1, plan.verify_md5.len()));
            match s.storage_md5(&v.path) {
                Ok(m) => {
                    if !m.eq_ignore_ascii_case(&v.md5) {
                        download.push(v.path.clone());
                    }
                    md5s.insert(v.path.clone(), m);
                }
                Err(_) => download.push(v.path.clone()),
            }
        }
        let mirror = staging.join("card");
        let failed = download_all(s, &download, &mirror, status);
        status("Storing the snapshot…".into());
        let listing = listing_of(&entries, &md5s);
        let device = json!({"card": "Flipper", "firmware_api": fw});
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let snap = snapshot::create(&snaps, &listing, Some(&mirror), None, label, device, now).map_err(err)?;
        let unchanged = plan.verify_md5.len() - (download.len() - plan.download.len());
        Ok(format!(
            "Snapshot {}: {} files, {} downloaded, {unchanged} confirmed unchanged by MD5, {} skipped{}",
            snap.id,
            snap.files.len(),
            download.len() - failed,
            snap.skipped.len(),
            if failed > 0 { format!(" ({failed} could not be read)") } else { String::new() }
        ))
    })();
    let _ = std::fs::remove_dir_all(&staging);
    r
}

pub fn diff_snapshots(dirs: &Dirs, from: &str, to: &str) -> Result<Diff, String> {
    let snaps = open_store(dirs)?;
    Ok(snapshot::diff_snapshots(&snaps.load(from).map_err(err)?, &snaps.load(to).map_err(err)?))
}

/// The card now, with the MD5s of the files a snapshot's sizes cannot tell apart.
fn card_listing(s: &mut Sess, sizes: &std::collections::HashMap<String, u64>, status: &mut dyn FnMut(String)) -> Result<Listing, String> {
    status("Listing the card…".into());
    let entries = list_tree(s, "/ext").map_err(|e| format!("cannot list the card: {e}"))?;
    let md5s = md5s_of(s, &entries, |e| sizes.get(&e.path) == Some(&e.size), status);
    Ok(listing_of(&entries, &md5s))
}

/// `diffSnapshot` with the card.
pub fn diff_with_card(s: &mut Sess, dirs: &Dirs, id: &str, status: &mut dyn FnMut(String)) -> Result<Diff, String> {
    let snaps = open_store(dirs)?;
    let snap = snaps.load(id).map_err(err)?;
    let sizes = snap.files.iter().map(|f| (f.path.clone(), f.size)).collect();
    let listing = card_listing(s, &sizes, status)?;
    Ok(snapshot::diff_listing(&snap, &listing))
}

/// `planRestore`: what restoring `path` (or everything) from snapshot `id` would do to the card as it is now.
pub fn plan_restore(s: &mut Sess, dirs: &Dirs, id: &str, path: &str, status: &mut dyn FnMut(String)) -> Result<RestorePlan, String> {
    if !path.is_empty() && !paths::valid_device_path(path) {
        return Err(format!("not a path on the card: {path}"));
    }
    let snaps = open_store(dirs)?;
    let snap = snaps.load(id).map_err(err)?;
    let under = |p: &str| path.is_empty() || p == path || p.starts_with(&format!("{path}/"));
    let sizes = snap.files.iter().filter(|f| under(&f.path)).map(|f| (f.path.clone(), f.size)).collect();
    let listing = card_listing(s, &sizes, status)?;
    snapshot::restore_plan(&snaps, &snap, (!path.is_empty()).then_some(path), Some(&listing)).map_err(err)
}

/// `runRestore`: the plan's folders, then every file that is not already the same.
pub fn restore(s: &mut Sess, plan: &RestorePlan, status: &mut dyn FnMut(String)) -> Result<String, String> {
    for d in &plan.mkdirs {
        let _ = mkpath(s, d);
    }
    let items: Vec<WriteItem> = plan
        .items
        .iter()
        .filter(|i| i.action != snapshot::Action::Same)
        .map(|i| WriteItem { local: PathBuf::from(&i.local), remote: i.remote.clone(), md5: i.md5.clone() })
        .collect();
    let (written, errors) = write_all(s, &items, status);
    let mut msg = format!("Restored {written} file(s) from {}, {} failed", plan.snapshot, errors.len());
    if !errors.is_empty() {
        msg.push_str(": ");
        msg.push_str(&errors.iter().take(5).cloned().collect::<Vec<_>>().join("; "));
        return Err(msg);
    }
    Ok(msg)
}

pub fn delete_snapshot(dirs: &Dirs, id: &str) -> Result<String, String> {
    let snaps = open_store(dirs)?;
    snaps.delete(id).map_err(err)?;
    let (_, freed) = snaps.gc().map_err(err)?;
    Ok(format!("Deleted {id}; freed {freed} bytes"))
}

// ------------------------------------------------------------------------------------------------ captures

/// `readCapturesFromCard`: the capture folders' files (no `assets`, no `.cache`) into the private mirror.
pub fn read_captures(s: &mut Sess, dirs: &Dirs, status: &mut dyn FnMut(String)) -> Result<String, String> {
    let mirror = dirs.card_mirror();
    let _ = std::fs::remove_dir_all(&mirror);
    private_dir(&mirror)?;
    let mut list = Vec::new();
    for (folder, suffix) in CAPTURE_FOLDERS {
        status(format!("Listing {folder}…"));
        for e in list_tree(s, folder).unwrap_or_default() {
            let parts: Vec<&str> = e.path.split('/').collect();
            if !e.dir
                && e.size <= MAX_CAPTURE_BYTES
                && e.path.to_ascii_lowercase().ends_with(suffix)
                && !parts.contains(&"assets")
                && !parts.contains(&".cache")
            {
                list.push(e.path);
            }
        }
    }
    let failed = download_all(s, &list, &mirror, status);
    let read = format!(
        "Read {} capture file(s) from the card{}",
        list.len() - failed,
        if failed > 0 { format!(", {failed} failed") } else { String::new() }
    );
    let indexed = index(dirs)?;
    Ok(format!("{read}; {indexed}"))
}

/// `buildCaptureLibrary`: the snapshots, the card's mirror and the console's pulls.
pub fn index(dirs: &Dirs) -> Result<String, String> {
    private_dir(&dirs.data)?;
    let lib = Library::open(&dirs.library()).map_err(err)?;
    let mut b = Builder::default();
    if dirs.snapshots().join("snapshots").is_dir() {
        b.add_snapshots(&Snapshots::open(&dirs.snapshots()).map_err(err)?).map_err(err)?;
    }
    for t in [dirs.card_mirror(), dirs.pulls.clone()] {
        if t.is_dir() {
            b.add_tree(&t, "pull").map_err(err)?;
        }
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let idx = b.finish(now);
    lib.save_index(&idx).map_err(err)?;
    Ok(format!("The library holds {} capture(s)", idx.captures.len()))
}

fn library(dirs: &Dirs) -> Result<(Library, capture::Index), String> {
    let lib = Library::open(&dirs.library()).map_err(err)?;
    let idx = lib.load().map_err(err)?;
    Ok((lib, idx))
}

pub fn search(dirs: &Dirs, query: &str) -> Result<(Vec<Capture>, usize), String> {
    if !dirs.library().is_dir() {
        return Ok((Vec::new(), 0));
    }
    let (_, idx) = library(dirs)?;
    Ok((capture::search(&idx, query, 500).into_iter().cloned().collect(), idx.captures.len()))
}

pub fn explain(dirs: &Dirs, id: &str) -> Result<(Capture, String), String> {
    let (lib, idx) = library(dirs)?;
    let c = lib.find(&idx, id).map_err(err)?;
    Ok((c.clone(), capture::explain(c)))
}

pub fn tag(dirs: &Dirs, id: &str, add: &[String], remove: &[String]) -> Result<String, String> {
    let (lib, idx) = library(dirs)?;
    let sha = lib.find(&idx, id).map_err(err)?.sha256.clone();
    let tags = lib.tag(&sha, add, remove).map_err(err)?;
    Ok(format!("Tags: {}", if tags.is_empty() { "none".to_string() } else { tags.join(", ") }))
}

pub fn dupes(dirs: &Dirs) -> Result<Vec<DupeGroup>, String> {
    let (_, idx) = library(dirs)?;
    Ok(capture::duplicates(&idx))
}

/// Package files are read through the same checks as everything else the app reads (a regular file).
pub fn check_package(path: &Path) -> Result<(), String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !f.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let mut first = [0u8; 1];
    f.read(&mut first).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(())
}

// ------------------------------------------------------------------------------------------------ the window's state

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Apps,
    Checkup,
    Rebuild,
    Snapshots,
    Captures,
}

/// What the tools window shows; the jobs' replies land here.
pub struct State {
    pub tab: Tab,
    /// The running job's status line; None when idle.
    pub busy: Option<String>,
    pub message: Option<Result<String, String>>,
    pub package: String,
    pub report: Option<Report>,
    pub picked: Vec<bool>,
    pub force: bool,
    pub plan: Vec<PlanItem>,
    pub checkup_packages: String,
    pub checkup: Option<Value>,
    pub source: String,
    pub git_ref: String,
    pub subdir: String,
    pub built: Option<limbo::BuildReport>,
    pub snapshots: Vec<Summary>,
    pub selected: Option<String>,
    pub other: Option<String>,
    pub label: String,
    pub diff: Option<Diff>,
    pub restore_path: String,
    pub restore: Option<RestorePlan>,
    pub query: String,
    pub captures: Vec<Capture>,
    pub total: usize,
    pub capture: Option<String>,
    pub detail: Option<(Capture, String)>,
    pub tags: String,
    pub dupes: Option<Vec<DupeGroup>>,
    /// The snapshots and the library were read once the window opened.
    pub loaded: bool,
}

impl Default for State {
    fn default() -> State {
        State {
            tab: Tab::Apps,
            busy: None,
            message: None,
            package: String::new(),
            report: None,
            picked: Vec::new(),
            force: false,
            plan: Vec::new(),
            checkup_packages: String::new(),
            checkup: None,
            source: String::new(),
            git_ref: String::new(),
            subdir: String::new(),
            built: None,
            snapshots: Vec::new(),
            selected: None,
            other: None,
            label: String::new(),
            diff: None,
            restore_path: String::new(),
            restore: None,
            query: String::new(),
            captures: Vec::new(),
            total: 0,
            capture: None,
            detail: None,
            tags: String::new(),
            dupes: None,
            loaded: false,
        }
    }
}

impl State {
    /// Takes in a job's reply.
    pub fn apply(&mut self, r: Reply) {
        let msg = |r: &Result<String, String>| Some(r.clone());
        match r {
            Reply::Status(s) => {
                self.busy = Some(s);
                return;
            }
            Reply::Package(r) => {
                self.plan.clear();
                match r {
                    Ok(rep) => {
                        let compatible =
                            rep.apps.iter().filter(|a| a.compat.as_ref().is_some_and(|c| c.status == Status::Compatible)).count();
                        self.message = Some(Ok(format!(
                            "{} apps in the package, {compatible} compatible with API {}{}",
                            rep.apps.len(),
                            rep.firmware_api.clone().unwrap_or_default(),
                            if rep.incomplete { " (incomplete: a limit was hit)" } else { "" }
                        )));
                        self.picked = rep.apps.iter().map(|a| a.compat.as_ref().is_some_and(|c| c.status == Status::Compatible)).collect();
                        self.report = Some(rep);
                    }
                    Err(e) => self.message = Some(Err(e)),
                }
            }
            Reply::Plan(r) => match r {
                Ok(p) => {
                    let refused = p.iter().filter(|i| i.refused).count();
                    let existing = p.iter().filter(|i| !i.refused && i.exists).count();
                    self.message = Some(Ok(format!(
                        "{} new, {existing} already on the card (asks before overwriting), {refused} refused",
                        p.len() - refused - existing
                    )));
                    self.plan = p;
                }
                Err(e) => self.message = Some(Err(e)),
            },
            Reply::Checkup(r) => match r {
                Ok(v) => {
                    self.message = Some(Ok(format!(
                        "{} apps on the card: {} will not load on API {}, {} unknown",
                        v["apps"].as_array().map_or(0, Vec::len),
                        v["incompatible"],
                        v["firmware_api"].as_str().unwrap_or_default(),
                        v["unknown"]
                    )));
                    self.checkup = Some(v);
                }
                Err(e) => self.message = Some(Err(e)),
            },
            Reply::Rebuild(r) => match r {
                Ok(b) => {
                    self.message = Some(Ok(format!("Built {} app(s) in Limbo", b.faps.len())));
                    self.built = Some(b);
                }
                Err(e) => self.message = Some(Err(e)),
            },
            Reply::Snapshots(r) => match r {
                Ok(list) => {
                    if self.selected.as_ref().is_some_and(|s| !list.iter().any(|x| &x.id == s)) {
                        self.selected = None;
                    }
                    self.snapshots = list;
                }
                Err(e) => self.message = Some(Err(e)),
            },
            Reply::Diff(r) => match r {
                Ok(d) => {
                    self.message = Some(Ok(format!(
                        "Since {}: {} added, {} removed, {} changed, {} unchanged",
                        d.from,
                        d.added.len(),
                        d.removed.len(),
                        d.changed.len(),
                        d.unchanged
                    )));
                    self.diff = Some(d);
                }
                Err(e) => self.message = Some(Err(e)),
            },
            Reply::RestorePlan(r) => match r {
                Ok(p) => {
                    self.message = Some(Ok(format!(
                        "Restoring would create {} and overwrite {} file(s); {} already match",
                        p.creates, p.overwrites, p.same
                    )));
                    self.restore = Some(p);
                }
                Err(e) => self.message = Some(Err(e)),
            },
            Reply::Captures(r) => match r {
                Ok((list, total)) => {
                    self.total = total;
                    self.captures = list;
                }
                Err(e) => self.message = Some(Err(e)),
            },
            Reply::Explain(r) => match r {
                Ok(d) => self.detail = Some(d),
                Err(e) => self.message = Some(Err(e)),
            },
            Reply::Dupes(r) => match r {
                Ok(g) => {
                    self.message = Some(Ok(format!("{} group(s) of duplicates", g.len())));
                    self.dupes = Some(g);
                }
                Err(e) => self.message = Some(Err(e)),
            },
            Reply::Done(r) => self.message = msg(&r),
        }
        self.busy = None;
    }
}

/// `true` for a string of the tags field: tags separated by commas or spaces, each a valid tag.
pub fn parse_tags(text: &str) -> Result<Vec<String>, String> {
    let tags: Vec<String> = text.split([',', ' ']).map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).collect();
    if let Some(bad) = tags.iter().find(|t| !capture::valid_tag(t)) {
        return Err(format!("not a tag: {bad}"));
    }
    Ok(tags)
}
