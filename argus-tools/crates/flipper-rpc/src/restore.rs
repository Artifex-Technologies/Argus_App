//! Restoring a backup of `/int` (upstream's `qFlipper-cli restore`, the C++
//! `UserRestoreOperation`), with the archive checked whole before anything is written.
//!
//! [`read_backup`] reads a `.tar.gz` into memory and refuses it unless every entry is safe
//! and complete. The rules are the C++ app's since 2026-11-12 (`TarArchive::readTarFile` for
//! any archive, `UserRestoreOperation` for backups), so both apps refuse the same archives:
//!
//! - one gzip member, complete (its CRC and length checked), at most [`ArchiveLimits::max_tar`]
//!   bytes of tar inside; anything after the member is ignored;
//! - 512-byte headers with a matching checksum, a name ended by NUL within its 100 bytes, no
//!   ustar prefix, a size in octal digits; only regular files (`0`) and folders (`5`, no data);
//!   every entry's data complete; the end is two zero blocks in a row, or the end of the data
//!   right after an entry (archives from older builds had no end blocks);
//! - names: UTF-8, relative, every part not empty, not `.` or `..`, no `\` and no control
//!   character; each folder listed before what is in it; no name twice;
//! - for a backup: everything under one `int/` folder, names in ASCII (the firmware refuses
//!   others), at most [`ArchiveLimits`] files, bytes and entries.
//!
//! [`Session::restore`] then lists `/int` on the device, refuses a backup whose files are
//! folders there (or the other way round) before writing anything, makes the folders that are
//! missing and writes every file (replacing what is there). Files on the device that are not in
//! the backup stay. Entries go out folder by folder (breadth first), names in byte order, the
//! order the C++ app's `FileNode` tree gives.
//!
//! Upstream's restore did this differently and could not work on current firmware: it first
//! removed every path in the backup without `recursive`, `/int` itself first (refused while it
//! holds anything), then made `/int` (always "already exists", since `/int` is an alias of
//! `/ext/.int`). It also unpacked the archive to the local disk with no name checks.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;

use crate::ops::FileKind;
use crate::session::{RpcError, Session};
use crate::transfer::Limits;
use crate::transport::Transport;

#[derive(Debug, Clone, Copy)]
pub struct ArchiveLimits {
    /// Bytes of tar after decompression.
    pub max_tar: u64,
    /// One file.
    pub max_file: u64,
    /// All files together.
    pub max_total: u64,
    /// Entries, folders included.
    pub max_entries: usize,
}

impl Default for ArchiveLimits {
    /// For `/int`, 256 KiB on a Flipper Zero: generous, and the same in the C++ app.
    fn default() -> ArchiveLimits {
        ArchiveLimits { max_tar: 80 << 20, max_file: 16 << 20, max_total: 64 << 20, max_entries: 10_000 }
    }
}

/// An archive refused, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveError(pub String);

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "backup refused: {}", self.0)
    }
}

impl std::error::Error for ArchiveError {}

fn bad(msg: impl Into<String>) -> ArchiveError {
    ArchiveError(msg.into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupEntry {
    /// The device path, `/int/...`.
    pub path: String,
    pub dir: bool,
    pub data: Vec<u8>,
}

/// A checked backup: every entry below `/int` (not `/int` itself), parents first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupArchive {
    pub entries: Vec<BackupEntry>,
}

impl BackupArchive {
    pub fn files(&self) -> usize {
        self.entries.iter().filter(|e| !e.dir).count()
    }
    pub fn dirs(&self) -> usize {
        self.entries.iter().filter(|e| e.dir).count()
    }
    pub fn bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.data.len() as u64).sum()
    }
}

const BLOCK: usize = 512;

/// Decompresses one gzip member, refusing more than `max` bytes of output or a cut stream.
pub fn gunzip(input: impl Read, max: u64) -> Result<Vec<u8>, ArchiveError> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(input)
        .take(max + 1)
        .read_to_end(&mut out)
        .map_err(|e| bad(format!("not a complete gzip stream ({e})")))?;
    if out.len() as u64 > max {
        return Err(bad(format!("more than {max} bytes once decompressed")));
    }
    Ok(out)
}

/// An octal header field: optional leading spaces, at least one digit, then only NULs or
/// spaces up to the end of the field. A binary (base-256) field is refused.
fn octal(field: &[u8], what: &str) -> Result<u64, ArchiveError> {
    let start = field.iter().position(|&b| b != b' ').unwrap_or(field.len());
    let digits = field[start..].iter().take_while(|b| (b'0'..=b'7').contains(b)).count();
    let rest = &field[start + digits..];
    if digits == 0 || !rest.iter().all(|&b| b == 0 || b == b' ') {
        return Err(bad(format!("the {what} field is not an octal number")));
    }
    field[start..start + digits]
        .iter()
        .try_fold(0u64, |v, &d| v.checked_mul(8).map(|v| v + u64::from(d - b'0')))
        .ok_or_else(|| bad(format!("the {what} field is too large")))
}

/// One entry of a tar stream: its name parts, whether it is a folder, and its data.
#[derive(Debug)]
struct TarEntry {
    parts: Vec<String>,
    dir: bool,
    data: std::ops::Range<usize>,
}

/// The rules every archive must meet (the C++ `TarArchive` reader's): see the module text.
fn read_tar(t: &[u8], max_entries: usize) -> Result<Vec<TarEntry>, ArchiveError> {
    let mut out: Vec<TarEntry> = Vec::new();
    let mut seen: HashMap<Vec<String>, bool> = HashMap::new();
    let mut pos = 0usize;
    let mut zeros = 0;
    while pos < t.len() {
        if t.len() - pos < BLOCK {
            return Err(bad("the archive is truncated (a cut header)"));
        }
        let h = &t[pos..pos + BLOCK];
        pos += BLOCK;
        if h.iter().all(|&b| b == 0) {
            zeros += 1;
            if zeros == 2 {
                break;
            }
            continue;
        }
        zeros = 0;
        let stored = octal(&h[148..156], "checksum")?;
        let sum: u64 = h[..148].iter().chain(&h[156..]).map(|&b| u64::from(b)).sum::<u64>() + 256;
        if stored != sum {
            return Err(bad("a header checksum does not match"));
        }
        let Some(name_len) = h[..100].iter().position(|&b| b == 0) else {
            return Err(bad("a name fills its whole field"));
        };
        if h[345..500].iter().any(|&b| b != 0) {
            return Err(bad("a name has a ustar prefix"));
        }
        let size = octal(&h[124..136], "size")?;
        let dir = match h[156] {
            b'0' => false,
            b'5' => true,
            _ => return Err(bad("only regular files and folders may be in a backup (no links)")),
        };
        if dir && size != 0 {
            return Err(bad("a folder entry carries data"));
        }
        let name = std::str::from_utf8(&h[..name_len]).map_err(|_| bad("a name is not UTF-8"))?;
        let body = if dir { name.strip_suffix('/').ok_or_else(|| bad(format!("folder {name:?} does not end in '/'")))? } else { name };
        if !dir && name.ends_with('/') {
            return Err(bad(format!("file {name:?} ends in '/'")));
        }
        let parts: Vec<String> = body.split('/').map(str::to_string).collect();
        for p in &parts {
            if p.is_empty() || p == "." || p == ".." || p.contains('\\') || p.chars().any(char::is_control) {
                return Err(bad(format!("unsafe name {name:?}")));
            }
        }
        if parts.len() > 1 && seen.get(&parts[..parts.len() - 1]) != Some(&true) {
            return Err(bad(format!("{name:?} comes before its folder (or the folder is missing)")));
        }
        if seen.insert(parts.clone(), dir).is_some() {
            return Err(bad(format!("{name:?} is in the archive twice")));
        }
        let start = pos;
        let pad = (BLOCK as u64 - size % BLOCK as u64) % BLOCK as u64;
        let end = (start as u64)
            .checked_add(size)
            .filter(|&e| e + pad <= t.len() as u64)
            .ok_or_else(|| bad("the archive is truncated (cut data)"))?;
        pos = (end + pad) as usize;
        out.push(TarEntry { parts, dir, data: start..end as usize });
        if out.len() > max_entries {
            return Err(bad(format!("more than {max_entries} entries")));
        }
    }
    Ok(out)
}

/// Reads and checks a backup of `/int`. Nothing is written anywhere.
pub fn read_backup(input: impl Read, limits: &ArchiveLimits) -> Result<BackupArchive, ArchiveError> {
    let t = gunzip(input, limits.max_tar)?;
    let entries = read_tar(&t, limits.max_entries)?;
    let mut tree: BTreeMap<Vec<String>, (bool, Vec<u8>)> = BTreeMap::new();
    let mut total = 0u64;
    for e in &entries {
        if e.parts[0] != "int" || (e.parts.len() == 1 && !e.dir) {
            return Err(bad("everything in a backup must be in its int/ folder"));
        }
        if e.parts.iter().any(|p| !p.is_ascii()) {
            return Err(bad(format!("{:?}: the device takes only ASCII names", e.parts.join("/"))));
        }
        let len = e.data.len() as u64;
        if len > limits.max_file {
            return Err(bad(format!("{} is {len} bytes, more than {}", e.parts.join("/"), limits.max_file)));
        }
        total += len;
        if total > limits.max_total {
            return Err(bad(format!("more than {} bytes of files", limits.max_total)));
        }
        tree.insert(e.parts.clone(), (e.dir, t[e.data.clone()].to_vec()));
    }
    if !tree.contains_key(&vec!["int".to_string()]) {
        return Err(bad("no int/ folder in the archive"));
    }
    // breadth first from int/, each folder's entries in byte order (BTreeMap order)
    let mut out = BackupArchive::default();
    let mut level = vec![vec!["int".to_string()]];
    while !level.is_empty() {
        let mut next = Vec::new();
        for parent in &level {
            for (parts, (dir, data)) in tree.range(parent.clone()..).skip(1) {
                if !parts.starts_with(parent) {
                    break;
                }
                if parts.len() != parent.len() + 1 {
                    continue;
                }
                out.entries.push(BackupEntry { path: format!("/{}", parts.join("/")), dir: *dir, data: data.clone() });
                if *dir {
                    next.push(parts.clone());
                }
            }
        }
        level = next;
    }
    Ok(out)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreReport {
    /// Folders made on the device.
    pub dirs_made: u64,
    /// Folders that were there already.
    pub dirs_kept: u64,
    pub files: u64,
    pub bytes: u64,
}

impl<T: Transport> Session<T> {
    /// Writes a checked backup to the device's `/int` (see the module text). Does not reboot;
    /// the C++ app reboots after a restore, and so does `argus-cli restore`.
    pub fn restore(&mut self, archive: &BackupArchive) -> Result<RestoreReport, RpcError> {
        let tree = self.walk("/int", &Limits::default())?;
        let on_device: HashMap<&str, &FileKind> = tree.entries.iter().map(|e| (e.path.as_str(), &e.kind)).collect();
        for e in &archive.entries {
            let kind = |dir: bool| if dir { "folder" } else { "file" };
            if let Some(k) = on_device.get(e.path.as_str()) {
                let device_dir = **k == FileKind::Dir;
                if device_dir != e.dir {
                    return Err(RpcError::Invalid(format!(
                        "{} is a {} on the device and a {} in the backup; nothing was written",
                        e.path,
                        kind(device_dir),
                        kind(e.dir)
                    )));
                }
            }
        }
        let mut report = RestoreReport::default();
        for e in &archive.entries {
            if e.dir {
                if on_device.contains_key(e.path.as_str()) {
                    report.dirs_kept += 1;
                } else {
                    self.storage_mkdir(&e.path)?;
                    report.dirs_made += 1;
                }
            } else {
                self.storage_write(&e.path, &e.data)?;
                report.files += 1;
                report.bytes += e.data.len() as u64;
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn octal_fields() {
        assert_eq!(octal(b"00000000017\0", "size").unwrap(), 15);
        assert_eq!(octal(b"     17 \0\0\0\0", "size").unwrap(), 15);
        assert!(octal(b"\0\0\0\0\0\0\0\0\0\0\0\0", "size").is_err());
        assert!(octal(b"0000000001x\0", "size").is_err());
        assert!(octal(&[0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], "size").is_err());
        assert!(octal(b"77777777777777777777777", "size").is_err());
    }
}
