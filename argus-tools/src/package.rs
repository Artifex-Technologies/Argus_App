//! Open a firmware package as data and find the apps in it.
//!
//! Accepted: update packages (.tgz: a gzip'd tar holding `resources.ths`, a heatshrink'd tar),
//! .zip, plain or gzip'd resource tarballs, `resources.ths` itself, nested combinations of these
//! up to a depth limit, or a single .fap. The format is decided by magic bytes, not names.
//!
//! What keeps a hostile package harmless:
//! - Nothing in the package is executed, and nothing from it is ever written under a name it
//!   chose. Every app found is stored as `<out>/<sha256>.fap`; package paths are kept only as
//!   display text. Symlinks, hard links, devices and directories are skipped, never created.
//! - Every decompressing layer draws from one byte budget (`total_bytes`), so a zip bomb or a
//!   gzip quine stops at the budget. Each app is capped (`app_bytes`), a nested archive spooled to
//!   disk is capped (`nested_bytes`), and so are entries and nesting depth.
//! - A malformed entry or nested archive is reported and skipped; only an unreadable outer file
//!   is an error.

use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::Serialize;

use crate::api::{self, Compat, FirmwareApi, SymbolTable};
use crate::fap::{self, Manifest};
use crate::heatshrink;
use crate::paths::{self, EntryName};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Decompressed bytes produced across all layers, in total.
    pub total_bytes: u64,
    /// A nested archive that has to be spooled to disk (a .zip inside something).
    pub nested_bytes: u64,
    /// One app file.
    pub app_bytes: u64,
    /// Archive entries looked at, in total.
    pub entries: u64,
    /// Archive / compression layers inside one another.
    pub depth: u32,
    /// Apps recorded.
    pub apps: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { total_bytes: 2 << 30, nested_bytes: 512 << 20, app_bytes: 8 << 20, entries: 200_000, depth: 6, apps: 10_000 }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorInfo {
    pub code: &'static str,
    pub message: String,
}

impl From<&Error> for ErrorInfo {
    fn from(e: &Error) -> Self {
        ErrorInfo { code: e.code, message: e.message.clone() }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AppEntry {
    /// Where it was inside the package (display text; may name an outer and inner archive).
    pub package_path: String,
    /// The name was absolute or had ".." parts: listed, but its folders are ignored.
    pub path_suspicious: bool,
    pub category: String,
    /// File name to use on the device.
    pub file_name: String,
    pub size: u64,
    pub sha256: String,
    pub md5: String,
    /// The stored copy: <out>/<sha256>.fap
    pub stored: PathBuf,
    pub manifest: Option<Manifest>,
    pub imports: usize,
    pub error: Option<ErrorInfo>,
    pub compat: Option<Compat>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Skipped {
    pub package_path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub package: String,
    pub package_sha256: String,
    pub format: String,
    pub firmware_api: Option<String>,
    pub symbols_table: bool,
    pub apps: Vec<AppEntry>,
    pub skipped: Vec<Skipped>,
    pub warnings: Vec<String>,
    /// A limit was hit: the listing is partial.
    pub incomplete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Gzip,
    Zip,
    Tar,
    Heatshrink,
    Elf,
    Unknown,
}

fn sniff(h: &[u8]) -> Kind {
    if h.starts_with(&[0x1f, 0x8b]) {
        Kind::Gzip
    } else if h.starts_with(b"PK\x03\x04") || h.starts_with(b"PK\x05\x06") {
        Kind::Zip
    } else if h.starts_with(b"\x7fELF") {
        Kind::Elf
    } else if heatshrink::parse_header(h).is_some() {
        Kind::Heatshrink
    } else if h.len() >= 263 && &h[257..262] == b"ustar" {
        Kind::Tar
    } else {
        Kind::Unknown
    }
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Gzip => "gzip",
        Kind::Zip => "zip",
        Kind::Tar => "tar",
        Kind::Heatshrink => "heatshrink",
        Kind::Elf => "elf",
        Kind::Unknown => "unknown",
    }
}

/// Bytes left to decompress, shared by every layer.
#[derive(Clone)]
struct Budget(Rc<Cell<u64>>);

const BUDGET_MSG: &str = "argus: decompression budget exhausted";

/// Counts what passes through against the budget; fails once it is spent.
struct Metered<R> {
    inner: R,
    budget: Budget,
}

impl<R: Read> Read for Metered<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.budget.0.get();
        if left == 0 {
            return Err(io::Error::other(BUDGET_MSG));
        }
        let want = buf.len().min(left.min(usize::MAX as u64) as usize);
        let n = self.inner.read(&mut buf[..want])?;
        self.budget.0.set(left - n as u64);
        Ok(n)
    }
}

fn is_budget(e: &io::Error) -> bool {
    e.to_string().contains(BUDGET_MSG)
}

struct Ctx<'a> {
    limits: Limits,
    budget: Budget,
    out_dir: &'a Path,
    work: tempfile::TempDir,
    fw: Option<FirmwareApi>,
    table: Option<SymbolTable>,
    entries: u64,
    report: Report,
}

impl Ctx<'_> {
    fn stop(&mut self, why: String) {
        if !self.report.incomplete {
            self.report.incomplete = true;
            self.report.warnings.push(why);
        }
    }

    fn skip(&mut self, path: &str, reason: impl Into<String>) {
        if self.report.skipped.len() < 5000 {
            self.report.skipped.push(Skipped { package_path: path.into(), reason: reason.into() });
        }
    }

    fn io_problem(&mut self, path: &str, e: io::Error) {
        if is_budget(&e) {
            self.stop(format!(
                "stopped: the package decompresses to more than {} bytes (a zip bomb, or simply too big)",
                self.limits.total_bytes
            ));
        } else {
            self.skip(path, format!("unreadable: {e}"));
        }
    }

    /// Read one app's bytes (capped) and record it.
    fn take_app(&mut self, name: &EntryName, outer: &str, r: &mut dyn Read) {
        let shown = join_display(outer, &name.display);
        if self.report.apps.len() >= self.limits.apps {
            self.stop(format!("stopped after {} apps", self.limits.apps));
            return;
        }
        let mut data = Vec::new();
        let cap = self.limits.app_bytes;
        if let Err(e) = r.take(cap + 1).read_to_end(&mut data) {
            self.io_problem(&shown, e);
            return;
        }
        if data.len() as u64 > cap {
            self.skip(&shown, format!("larger than {cap} bytes; no Flipper app is that big"));
            return;
        }
        match self.record(name, &shown, &data) {
            Ok(entry) => self.report.apps.push(entry),
            Err(e) => self.skip(&shown, format!("could not store: {e}")),
        }
    }

    fn record(&self, name: &EntryName, shown: &str, data: &[u8]) -> Result<AppEntry> {
        let sha = crate::sha256_hex(data);
        let stored = self.out_dir.join(format!("{sha}.fap"));
        write_new(&stored, data)?;
        let (manifest, imports, error, compat) = match fap::inspect(data) {
            Ok(info) => {
                let compat = self.fw.map(|fw| api::check(&info, &fw, self.table.as_ref()));
                (Some(info.manifest.clone()), info.imports.len(), None, compat)
            }
            Err(e) => {
                let compat = self.fw.map(|fw| api::not_a_fap(&fw, &e.message));
                (None, 0, Some(ErrorInfo::from(&e)), compat)
            }
        };
        Ok(AppEntry {
            package_path: shown.into(),
            path_suspicious: name.traversal,
            category: paths::category_for(name),
            file_name: paths::device_file_name(name, &sha),
            size: data.len() as u64,
            md5: crate::md5_hex(data),
            sha256: sha,
            stored,
            manifest,
            imports,
            error,
            compat,
        })
    }
}

fn join_display(outer: &str, inner: &str) -> String {
    if outer.is_empty() {
        inner.to_string()
    } else {
        format!("{outer} > {inner}")
    }
}

/// Create `path` with `data`, never following or replacing anything already there (an existing
/// file of the same name has the same content: names are hashes).
fn write_new(path: &Path, data: &[u8]) -> Result<()> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc_nofollow());
    }
    match opts.open(path) {
        Ok(mut f) => {
            f.write_all(data)?;
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let meta = std::fs::symlink_metadata(path)?;
            if meta.file_type().is_file() && meta.len() == data.len() as u64 {
                Ok(())
            } else {
                Err(Error::new("store_conflict", format!("{} exists and is not this app", path.display())))
            }
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(unix)]
fn libc_nofollow() -> i32 {
    // O_NOFOLLOW: Linux 0o400000; macOS and the BSDs 0x100.
    if cfg!(target_os = "linux") {
        0o400000
    } else {
        0x100
    }
}

/// Fill `head` from `r` as far as it goes (up to its length).
fn read_head(r: &mut dyn Read, head: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < head.len() {
        let m = r.read(&mut head[n..])?;
        if m == 0 {
            break;
        }
        n += m;
    }
    Ok(n)
}

fn looks_nested(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [".ths", ".tar", ".tgz", ".gz", ".zip"].iter().any(|s| n.ends_with(s))
}

/// Process a stream of unknown kind (the content of an entry or a decompressed layer).
fn stream(ctx: &mut Ctx, r: &mut dyn Read, depth: u32, outer: &str, top_name: &EntryName) -> Result<()> {
    if depth > ctx.limits.depth {
        ctx.skip(outer, "nested too deep");
        return Ok(());
    }
    let mut head = [0u8; 512];
    let n = match read_head(r, &mut head) {
        Ok(n) => n,
        Err(e) => {
            ctx.io_problem(outer, e);
            return Ok(());
        }
    };
    let kind = sniff(&head[..n]);
    if depth == 0 {
        ctx.report.format = kind_name(kind).into();
    }
    match kind {
        Kind::Gzip => {
            let chained = (&head[..n]).chain(r);
            let mut dec = Metered { inner: flate2::read::MultiGzDecoder::new(chained), budget: ctx.budget.clone() };
            stream(ctx, &mut dec, depth + 1, outer, top_name)
        }
        Kind::Heatshrink => {
            let p = heatshrink::parse_header(&head[..n]).expect("sniffed");
            let rest = (&head[heatshrink::HEADER_LEN..n]).chain(r);
            let mut dec = Metered { inner: heatshrink::Decoder::new(rest, p), budget: ctx.budget.clone() };
            stream(ctx, &mut dec, depth + 1, outer, top_name)
        }
        Kind::Tar => tar_entries(ctx, &mut (&head[..n]).chain(r), depth, outer),
        Kind::Zip => {
            let spooled = spool(ctx, &mut (&head[..n]).chain(r), outer)?;
            match spooled {
                Some(f) => zip_entries(ctx, f, depth, outer),
                None => Ok(()),
            }
        }
        Kind::Elf => {
            ctx.take_app(top_name, "", &mut (&head[..n]).chain(r));
            Ok(())
        }
        Kind::Unknown if depth == 0 => {
            Err(Error::new("unknown_format", "not a firmware package Argus knows (expected .tgz, .zip, .tar, resources.ths or a .fap)"))
        }
        Kind::Unknown => {
            ctx.skip(outer, "not an archive Argus knows; skipped");
            Ok(())
        }
    }
}

/// Copy a nested archive that needs seeking (zip) into the work folder, within its cap.
fn spool(ctx: &mut Ctx, r: &mut dyn Read, outer: &str) -> Result<Option<File>> {
    let mut f = tempfile::tempfile_in(ctx.work.path())?;
    let cap = ctx.limits.nested_bytes;
    match io::copy(&mut r.take(cap + 1), &mut f) {
        Ok(n) if n > cap => {
            ctx.skip(outer, format!("nested archive larger than {cap} bytes"));
            Ok(None)
        }
        Ok(_) => {
            f.rewind()?;
            Ok(Some(f))
        }
        Err(e) => {
            ctx.io_problem(outer, e);
            Ok(None)
        }
    }
}

fn tar_entries(ctx: &mut Ctx, r: &mut dyn Read, depth: u32, outer: &str) -> Result<()> {
    let mut archive = tar::Archive::new(r);
    let entries = match archive.entries() {
        Ok(e) => e,
        Err(e) => {
            ctx.io_problem(outer, e);
            return Ok(());
        }
    };
    for entry in entries {
        if ctx.report.incomplete {
            break;
        }
        ctx.entries += 1;
        if ctx.entries > ctx.limits.entries {
            ctx.stop(format!("stopped after {} archive entries", ctx.limits.entries));
            break;
        }
        let mut entry = match entry {
            Ok(e) => e,
            Err(e) => {
                // a broken header ends what can be read of this tar
                ctx.io_problem(outer, e);
                break;
            }
        };
        let name = paths::entry_name(&entry.path_bytes());
        let shown = join_display(outer, &name.display);
        let ty = entry.header().entry_type();
        if ty.is_dir() || ty.is_pax_global_extensions() {
            continue;
        }
        if ty.is_symlink() || ty.is_hard_link() {
            if is_app_name(&name) {
                ctx.skip(&shown, "a link, not a file: skipped");
            }
            continue;
        }
        if !(ty.is_file() || ty.is_contiguous()) {
            if is_app_name(&name) {
                ctx.skip(&shown, "not a regular file: skipped");
            }
            continue;
        }
        if is_app_name(&name) {
            ctx.take_app(&name, outer, &mut entry);
        } else if looks_nested(&name.display) {
            let mut metered = Metered { inner: &mut entry, budget: ctx.budget.clone() };
            stream(ctx, &mut metered, depth + 1, &shown, &name)?;
        }
    }
    Ok(())
}

fn is_app_name(n: &EntryName) -> bool {
    n.parts.last().is_some_and(|b| b.to_ascii_lowercase().ends_with(".fap"))
}

fn zip_entries(ctx: &mut Ctx, f: File, depth: u32, outer: &str) -> Result<()> {
    let mut zip = match zip::ZipArchive::new(f) {
        Ok(z) => z,
        Err(e) => {
            ctx.skip(if outer.is_empty() { "(package)" } else { outer }, format!("not a readable zip: {e}"));
            return Ok(());
        }
    };
    for i in 0..zip.len() {
        if ctx.report.incomplete {
            break;
        }
        ctx.entries += 1;
        if ctx.entries > ctx.limits.entries {
            ctx.stop(format!("stopped after {} archive entries", ctx.limits.entries));
            break;
        }
        let mut file = match zip.by_index(i) {
            Ok(f) => f,
            Err(e) => {
                ctx.skip(outer, format!("entry {i}: {e}"));
                continue;
            }
        };
        let name = paths::entry_name(file.name_raw());
        let shown = join_display(outer, &name.display);
        if file.is_dir() {
            continue;
        }
        let is_link = file.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000);
        if is_link || file.encrypted() {
            if is_app_name(&name) {
                ctx.skip(&shown, if is_link { "a link, not a file: skipped" } else { "encrypted: skipped" });
            }
            continue;
        }
        if is_app_name(&name) {
            let mut metered = Metered { inner: &mut file, budget: ctx.budget.clone() };
            ctx.take_app(&name, outer, &mut metered);
        } else if looks_nested(&name.display) {
            let mut metered = Metered { inner: &mut file, budget: ctx.budget.clone() };
            stream(ctx, &mut metered, depth + 1, &shown, &name)?;
        }
    }
    Ok(())
}

/// Scan a package file. Apps are stored in `out_dir` (created if missing) as `<sha256>.fap`.
/// With `fw`, each app gets a compatibility verdict for that firmware API.
pub fn scan(package: &Path, out_dir: &Path, limits: Limits, fw: Option<FirmwareApi>) -> Result<Report> {
    let meta = std::fs::metadata(package)?;
    if !meta.is_file() {
        return Err(Error::new("not_a_file", "the package is not a regular file"));
    }
    std::fs::create_dir_all(out_dir)?;
    let package_sha256 = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        io::copy(&mut File::open(package)?, &mut h)?;
        hex::encode(h.finalize())
    };
    let table = fw.as_ref().and_then(api::builtin_table);
    let display = crate::display_text(package.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().as_bytes(), 200);
    let mut ctx = Ctx {
        limits,
        budget: Budget(Rc::new(Cell::new(limits.total_bytes))),
        out_dir,
        work: tempfile::Builder::new().prefix("argus-pkg-").tempdir_in(out_dir)?,
        fw,
        table,
        entries: 0,
        report: Report {
            package: display.clone(),
            package_sha256,
            format: String::new(),
            firmware_api: fw.map(|f| f.text()),
            symbols_table: false,
            apps: Vec::new(),
            skipped: Vec::new(),
            warnings: Vec::new(),
            incomplete: false,
        },
    };
    ctx.report.symbols_table = ctx.table.is_some();
    let top = paths::entry_name(display.as_bytes());
    let mut f = File::open(package)?;
    let head_kind = {
        let mut head = [0u8; 512];
        let n = read_head(&mut f, &mut head)?;
        f.rewind()?;
        sniff(&head[..n])
    };
    if head_kind == Kind::Zip {
        ctx.report.format = "zip".into();
        zip_entries(&mut ctx, f, 0, "")?;
    } else {
        stream(&mut ctx, &mut f, 0, "", &top)?;
    }
    let report = ctx.report;
    Ok(report)
}
