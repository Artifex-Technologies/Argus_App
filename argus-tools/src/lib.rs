//! argus-tools: the non-UI logic of Argus_App (a qFlipper fork).
//!
//! The Qt app runs the `argus-tools` binary through QProcess and reads its JSON. Everything here
//! treats its input as hostile data: firmware packages, FAP files, captures and the output of
//! Limbo guests are parsed with explicit bounds and never executed.
//!
//! Modules:
//! - [`elf`], [`fap`]: a bounded ELF32 reader and the FAP manifest (`.fapmeta`).
//! - [`api`]: the firmware loader's compatibility rule and the exported-symbol tables.
//! - [`heatshrink`], [`package`]: opening firmware packages (.tgz, .zip, .tar, resources.ths).
//! - [`store`], [`snapshot`]: the content-addressed SD card snapshots.
//! - [`flipperfile`], [`capture`]: Flipper key/value files and the capture library.
//! - [`limbo`]: rebuilding an app with ufbt inside a disposable Limbo guest.

pub mod api;
pub mod capture;
pub mod checkup;
pub mod elf;
pub mod fap;
pub mod flipperfile;
pub mod heatshrink;
pub mod limbo;
pub mod package;
pub mod paths;
pub mod snapshot;
pub mod store;

use std::fmt;

/// One error type for the crate: a stable machine-readable code plus a sentence for people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}

impl Error {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Error { code, message: message.into() }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::new("io", e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::new("json", e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Hex SHA-256 of a byte slice.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

/// Hex MD5 of a byte slice (the Flipper's storage RPC reports MD5, so snapshots keep it too).
pub fn md5_hex(data: &[u8]) -> String {
    use md5::{Digest, Md5};
    hex::encode(Md5::digest(data))
}

/// Text that came from a hostile source, made safe to show: control characters replaced, length capped.
pub fn display_text(raw: &[u8], max_chars: usize) -> String {
    let s = String::from_utf8_lossy(raw);
    let mut out = String::new();
    for (n, c) in s.chars().enumerate() {
        if n >= max_chars {
            out.push('…');
            break;
        }
        out.push(if c.is_control() { '\u{FFFD}' } else { c });
    }
    out
}

/// Like [`display_text`], but keeps line breaks and tabs, and keeps the *last* `max_chars`
/// characters (for a guest's build log, where the end matters).
pub fn display_log(raw: &[u8], max_chars: usize) -> String {
    let s = String::from_utf8_lossy(raw);
    let skip = s.chars().count().saturating_sub(max_chars);
    s.chars().skip(skip).map(|c| if c.is_control() && c != '\n' && c != '\t' { '\u{FFFD}' } else { c }).collect()
}
