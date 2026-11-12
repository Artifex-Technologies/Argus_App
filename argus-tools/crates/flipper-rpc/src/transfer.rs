//! Multi-request reads that write to the local disk: the file tree of a device folder, pulling
//! a file or folder, and the backup of `/int` (upstream's `qFlipper-cli backup`). They follow
//! `GetFileTreeOperation`, `DirectoryDownloadOperation` and `UserBackupOperation` in the C++
//! app, with these differences, all on purpose:
//!
//! - Every name the device lists is checked before it is used locally (the rule of the C++
//!   capture pull's `stagedPath`, plus control characters): empty, `.`, `..`, or containing `/`,
//!   `\`, NUL or a control character is refused and reported, never joined onto a local path.
//!   The C++ backup and directory download had no such check.
//! - Nothing local is overwritten or deleted. The C++ directory download removes an existing
//!   target folder recursively; here the destination must not exist. Folders are created 0700
//!   and files 0600 with `create_new`, so a planted symlink is never followed. The backup holds
//!   the device's keys and settings; it is written as `<file>.part` and linked into place only
//!   when complete.
//! - Sizes, entry counts and depth are capped ([`Limits`]).
//! - The backup's tar entries are written in the order the device listed them, with the
//!   format the C++ restore reads (section "backup format" below), without the C++ writer's
//!   two quirks: it truncated names over 99 bytes (refused here) and added a zero block after
//!   files whose size is a multiple of 512.

use std::collections::VecDeque;
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::ops::{check_path, FileKind, MAX_PATH_LEN};
use crate::proto::pb::CommandStatus;
use crate::session::{RpcError, Session};
use crate::transport::Transport;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Largest single file.
    pub max_file: u64,
    /// All files together.
    pub max_total: u64,
    /// Entries in one tree.
    pub max_entries: usize,
    /// Folder levels below the start.
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { max_file: 256 << 20, max_total: 1 << 30, max_entries: 100_000, max_depth: 32 }
    }
}

impl Limits {
    /// For `/int`, which is 256 KiB on a Flipper Zero.
    pub fn backup() -> Limits {
        Limits { max_file: 16 << 20, max_total: 64 << 20, max_entries: 10_000, max_depth: 16 }
    }
}

/// One entry of a device tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// Absolute device path.
    pub path: String,
    /// Path below the walked folder, `/`-separated, each part checked by [`safe_name`].
    pub relative: String,
    pub kind: FileKind,
    pub size: u32,
}

/// A device tree, in the order the C++ `GetFileTreeOperation` produces it (each folder's
/// entries as listed, folders queued breadth-first), plus the names that were refused.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tree {
    pub entries: Vec<TreeEntry>,
    /// Device paths of refused entries, with the name escaped (`{:?}`).
    pub refused: Vec<String>,
}

/// True when a name from a device listing is safe to use as one local path component.
pub fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= MAX_PATH_LEN
        && !name.chars().any(|c| c == '/' || c == '\\' || c.is_control())
}

fn join(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

impl<T: Transport> Session<T> {
    /// Lists `root` and every folder below it.
    pub fn walk(&mut self, root: &str, limits: &Limits) -> Result<Tree, RpcError> {
        check_path(root)?;
        let mut tree = Tree::default();
        let mut queue = VecDeque::from([(root.to_string(), String::new(), 0usize)]);
        while let Some((dir, rel, depth)) = queue.pop_front() {
            for e in self.storage_list(&dir)? {
                let path = join(&dir, &e.name);
                if !safe_name(&e.name) || path.len() > MAX_PATH_LEN {
                    tree.refused.push(format!("{dir}/{:?}", e.name));
                    continue;
                }
                let relative = if rel.is_empty() { e.name.clone() } else { format!("{rel}/{}", e.name) };
                if e.kind == FileKind::Dir {
                    if depth + 1 > limits.max_depth {
                        return Err(RpcError::Limit(format!("{path} is more than {} levels deep", limits.max_depth)));
                    }
                    queue.push_back((path.clone(), relative.clone(), depth + 1));
                }
                tree.entries.push(TreeEntry { path, relative, kind: e.kind, size: e.size });
                if tree.entries.len() > limits.max_entries {
                    return Err(RpcError::Limit(format!("{root} has more than {} entries", limits.max_entries)));
                }
            }
        }
        let total: u64 = tree.entries.iter().filter(|e| e.kind == FileKind::File).map(|e| u64::from(e.size)).sum();
        if total > limits.max_total {
            return Err(RpcError::Limit(format!("{root} holds {total} bytes, more than the {} allowed", limits.max_total)));
        }
        Ok(tree)
    }

    /// Copies a device file or folder to `dest`, which must not exist yet. A folder becomes
    /// `dest` with the tree below it.
    pub fn pull(&mut self, remote: &str, dest: &Path, limits: &Limits) -> Result<PullReport, RpcError> {
        check_path(remote)?;
        ensure_absent(dest)?;
        let Some(top) = self.storage_stat(remote)? else {
            return Err(RpcError::Device(CommandStatus::ErrorStorageNotExist));
        };
        let mut report = PullReport::default();
        if top.kind == FileKind::File {
            report.bytes = self.read_to_new_file(remote, dest, limits.max_file)?;
            report.files = 1;
            return Ok(report);
        }
        let tree = self.walk(remote, limits)?;
        make_dir(dest)?;
        report.dirs = 1;
        for e in &tree.entries {
            let local = local_path(dest, &e.relative);
            match e.kind {
                FileKind::Dir => {
                    make_dir(&local)?;
                    report.dirs += 1;
                }
                FileKind::File => {
                    report.bytes += self.read_to_new_file(&e.path, &local, limits.max_file)?;
                    report.files += 1;
                    if report.bytes > limits.max_total {
                        return Err(RpcError::Limit(format!("more than {} bytes pulled", limits.max_total)));
                    }
                }
            }
        }
        report.refused = tree.refused;
        Ok(report)
    }

    fn read_to_new_file(&mut self, remote: &str, local: &Path, max: u64) -> Result<u64, RpcError> {
        let mut f = new_file(local)?;
        let r = self.storage_read(remote, &mut f, max).and_then(|n| f.sync_all().map(|_| n).map_err(|e| local_err(local, e)));
        if r.is_err() {
            drop(f);
            let _ = std::fs::remove_file(local);
        }
        r
    }

    /// Backs up `/int` into a gzip-compressed tar at `out` (which must not exist), in the
    /// layout the C++ app's restore reads: `int/` and everything below it. `mtime` (seconds
    /// since 1970) goes into every header.
    pub fn backup(&mut self, out: &Path, limits: &Limits, mtime: u64) -> Result<BackupReport, RpcError> {
        ensure_absent(out)?;
        let tree = self.walk("/int", limits)?;
        let part = PathBuf::from(format!("{}.part", out.display()));
        let file = new_file(&part)?;
        let result = (|| {
            let mut tar = TarWriter::new(flate2::write::GzEncoder::new(file, flate2::Compression::default()), mtime);
            tar.dir("int").map_err(|e| local_err(&part, e))?;
            let mut report = BackupReport { refused: tree.refused.clone(), ..Default::default() };
            let mut buf = Vec::new();
            for e in &tree.entries {
                let name = format!("int/{}", e.relative);
                match e.kind {
                    FileKind::Dir => {
                        tar.dir(&name).map_err(|err| tar_err(&part, &name, err))?;
                        report.dirs += 1;
                    }
                    FileKind::File => {
                        buf.clear();
                        self.storage_read(&e.path, &mut buf, limits.max_file)?;
                        tar.file(&name, &buf).map_err(|err| tar_err(&part, &name, err))?;
                        report.files += 1;
                        report.bytes += buf.len() as u64;
                        if report.bytes > limits.max_total {
                            return Err(RpcError::Limit(format!("more than {} bytes in the backup", limits.max_total)));
                        }
                    }
                }
            }
            let gz = tar.finish().map_err(|e| local_err(&part, e))?;
            gz.finish().and_then(|f| f.sync_all()).map_err(|e| local_err(&part, e))?;
            // A hard link fails if `out` appeared meanwhile, so nothing is ever replaced.
            std::fs::hard_link(&part, out).map_err(|e| local_err(out, e))?;
            Ok(report)
        })();
        let _ = std::fs::remove_file(&part);
        result
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullReport {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub refused: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupReport {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub refused: Vec<String>,
}

fn local_err(p: &Path, e: std::io::Error) -> RpcError {
    RpcError::Local(format!("{}: {e}", p.display()))
}

fn tar_err(p: &Path, name: &str, e: std::io::Error) -> RpcError {
    if e.kind() == std::io::ErrorKind::InvalidInput {
        RpcError::Local(format!("cannot store {name:?} in the backup: {e}"))
    } else {
        local_err(p, e)
    }
}

fn ensure_absent(p: &Path) -> Result<(), RpcError> {
    match std::fs::symlink_metadata(p) {
        Ok(_) => Err(RpcError::Local(format!("{} already exists; nothing is overwritten", p.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(local_err(p, e)),
    }
}

fn make_dir(p: &Path) -> Result<(), RpcError> {
    DirBuilder::new().mode(0o700).create(p).map_err(|e| local_err(p, e))
}

fn new_file(p: &Path) -> Result<File, RpcError> {
    OpenOptions::new().write(true).create_new(true).mode(0o600).open(p).map_err(|e| local_err(p, e))
}

fn local_path(root: &Path, relative: &str) -> PathBuf {
    // Every part was checked by safe_name, so this stays below root.
    relative.split('/').fold(root.to_path_buf(), |p, part| p.join(part))
}

// ---- backup format -------------------------------------------------------------------------
//
// The C++ app's TarArchive (backend/tararchive.cpp) reads 512-byte headers: name (100 bytes,
// NUL-terminated), mode, owner, group, size, mtime (octal text), checksum, typeflag '0' (file) or
// '5' (folder, name ending in '/'), then the data padded to 512, until two zero blocks. It
// ignores everything after the link name, so the ustar magic written here is for other tools.

const BLOCK: usize = 512;

struct TarWriter<W: Write> {
    out: W,
    mtime: u64,
}

impl<W: Write> TarWriter<W> {
    fn new(out: W, mtime: u64) -> Self {
        TarWriter { out, mtime }
    }

    fn header(&mut self, name: &str, size: u64, dir: bool) -> std::io::Result<()> {
        let stored = if dir { format!("{name}/") } else { name.to_string() };
        if stored.len() > 99 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "name longer than 99 bytes"));
        }
        let mut h = [0u8; BLOCK];
        h[..stored.len()].copy_from_slice(stored.as_bytes());
        let mut put = |off: usize, len: usize, value: u64| {
            let text = format!("{value:0width$o}", width = len - 1);
            h[off..off + len - 1].copy_from_slice(text.as_bytes());
        };
        put(100, 8, if dir { 0o755 } else { 0o664 });
        put(108, 8, 1000);
        put(116, 8, 1000);
        put(124, 12, size);
        put(136, 12, self.mtime);
        h[156] = if dir { b'5' } else { b'0' };
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        // The checksum counts its own field as eight spaces.
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        let text = format!("{sum:06o}\0 ");
        h[148..156].copy_from_slice(text.as_bytes());
        self.out.write_all(&h)
    }

    fn dir(&mut self, name: &str) -> std::io::Result<()> {
        self.header(name, 0, true)
    }

    fn file(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        if data.len() as u64 > 0o77777777777 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "file too large for a tar header"));
        }
        self.header(name, data.len() as u64, false)?;
        self.out.write_all(data)?;
        let pad = (BLOCK - data.len() % BLOCK) % BLOCK;
        self.out.write_all(&[0u8; BLOCK][..pad])
    }

    fn finish(mut self) -> std::io::Result<W> {
        self.out.write_all(&[0u8; 2 * BLOCK])?;
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["a", "badge.nfc", ".bt.settings", "snake game.fap", "café"] {
            assert!(safe_name(ok), "{ok}");
        }
        for bad in ["", ".", "..", "a/b", "a\\b", "a\0b", "a\nb", "x\u{1b}[2J", "x\u{7f}", "x\u{9b}"] {
            assert!(!safe_name(bad), "{bad:?}");
        }
        assert!(!safe_name(&"a".repeat(256)));
    }

    #[test]
    fn tar_header_checksum_matches_the_cpp_formula() {
        let mut out = Vec::new();
        let mut t = TarWriter::new(&mut out, 1_700_000_000);
        t.file("int/a", b"hello").unwrap();
        t.finish().unwrap();
        let h = &out[..BLOCK];
        // backend/tararchive.cpp: 256 + every byte except the checksum field
        let cpp: u32 = 256 + h[..148].iter().chain(&h[156..]).map(|&b| u32::from(b)).sum::<u32>();
        let stored = u32::from_str_radix(std::str::from_utf8(&h[148..154]).unwrap(), 8).unwrap();
        assert_eq!(stored, cpp);
        assert_eq!(out.len(), BLOCK + BLOCK + 2 * BLOCK);
    }

    #[test]
    fn aligned_files_get_no_extra_block_and_long_names_are_refused() {
        let mut out = Vec::new();
        let mut t = TarWriter::new(&mut out, 0);
        t.file("int/x", &[7u8; 1024]).unwrap();
        assert!(t.file(&format!("int/{}", "n".repeat(96)), b"").is_err());
        t.finish().unwrap();
        assert_eq!(out.len(), BLOCK + 1024 + 2 * BLOCK);
    }
}
