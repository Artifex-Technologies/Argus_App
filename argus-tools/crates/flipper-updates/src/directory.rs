//! `directory.json`, Flipper's firmware directory, read as the C++ app reads it (`UpdateRegistry::fillFromJson`,
//! `Flipper::Updates::ChannelInfo`/`VersionInfo`/`FileInfo`), with Qt's JSON rules (`qjson`).
//!
//! The rules both apps follow (the C++ ones since 2026-11-12, see docs/RUST_PORT.md):
//! - a channel needs `id`, `title`, `description` and an array `versions`; a version `version`, `changelog`,
//!   `timestamp` and an array `files`; a file `target`, `type`, `url` and `sha256`. A value of another type
//!   reads as Qt's `toString()` does (empty), except:
//! - `sha256` must be 64 hex digits (it is kept in lower case), and `url` must start with `https://`: a file
//!   whose hash could not be checked, or that would be read from anywhere but an HTTPS server, makes the
//!   directory invalid (the C++ app skipped the check for an empty hash and followed any URL Qt can open);
//! - one malformed entry makes the whole directory invalid (the C++ app kept the channels before it);
//! - channels are kept by `id` in Qt's key order (a repeated id: the later one); a channel's versions are
//!   sorted newest first by [`version_key`], stably;
//! - a channel may have no versions: it then has no latest version (the C++ app read past an empty list).

use std::collections::BTreeMap;

use qjson::{QString, Value};

/// The largest directory read (Flipper's is well under 1 MiB).
pub const MAX_DIRECTORY: u64 = 16 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileInfo {
    pub target: String,
    pub kind: String,
    pub url: String,
    /// 64 lower-case hex digits.
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub number: String,
    pub changelog: String,
    /// Seconds since 1970 as the server gives them (0 when not a number).
    pub timestamp: i64,
    pub files: Vec<FileInfo>,
}

impl Version {
    /// `VersionInfo::fileInfo`: the first file of this type and target.
    pub fn file(&self, kind: &str, target: &str) -> Option<&FileInfo> {
        self.files.iter().find(|f| f.kind == kind && f.target == target)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Channel {
    pub id: String,
    pub title: String,
    pub description: String,
    /// Newest first.
    pub versions: Vec<Version>,
}

impl Channel {
    pub fn latest(&self) -> Option<&Version> {
        self.versions.first()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Directory {
    /// By id, in Qt's order (UTF-16 code units).
    channels: BTreeMap<QString, Channel>,
}

impl Directory {
    /// `UpdateRegistry::channelNames`: the ids in key order, a first one starting with `dev` moved to the end.
    pub fn channel_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.channels.values().map(|c| c.id.as_str()).collect();
        if names.first().is_some_and(|n| n.starts_with("dev")) {
            let first = names.remove(0);
            names.push(first);
        }
        names
    }

    pub fn channel(&self, id: &str) -> Option<&Channel> {
        self.channels.get(&qjson::qs(id))
    }

    /// The channels in [`channel_names`](Self::channel_names) order.
    pub fn channels(&self) -> Vec<&Channel> {
        self.channel_names().into_iter().filter_map(|n| self.channel(n)).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn bad(m: impl Into<String>) -> Error {
    Error(m.into())
}

/// `VersionInfo::toNumericValue`: the part before the first `-`, split at `.`, each part 1 to 9 ASCII digits,
/// read as base-1000 digits. `None` for anything else (a git hash on the dev channel) or an overflow; `None`
/// sorts below every number. (The C++ app read the parts with `toInt` into an `int`, which overflowed, and
/// compared by subtracting, which overflowed for a version that is not a number.)
pub fn version_key(version: &str) -> Option<i64> {
    let head = version.split('-').next().unwrap_or("");
    let mut ret: i64 = 0;
    for token in head.split('.') {
        if token.is_empty() || token.len() > 9 || !token.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let val: i64 = token.parse().ok()?;
        ret = ret.checked_mul(1000)?.checked_add(val)?;
    }
    Some(ret)
}

/// `VersionInfo::compare`: which version is newer (the C++ app's sign).
pub fn compare(a: &str, b: &str) -> std::cmp::Ordering {
    version_key(a).cmp(&version_key(b))
}

fn text(v: Option<&Value>) -> String {
    v.map(Value::to_text).unwrap_or_default()
}

fn has_all(o: &qjson::Object, keys: &[&str]) -> bool {
    keys.iter().all(|k| o.contains(k))
}

/// A SHA-256 as the directory gives it: 64 hex digits, returned in lower case.
pub fn checked_sha256(s: &str) -> Option<String> {
    (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}

fn file(v: &Value) -> Result<FileInfo, Error> {
    let o = v.as_object().ok_or_else(|| bad("Expected FileInfo to be an object"))?;
    if !has_all(o, &["target", "type", "url", "sha256"]) {
        return Err(bad("Malformed FileInfo"));
    }
    let url = text(o.get("url"));
    if !url.starts_with("https://") {
        return Err(bad(format!("FileInfo: not an https URL: {url}")));
    }
    let sha = text(o.get("sha256"));
    let sha256 = checked_sha256(&sha).ok_or_else(|| bad(format!("FileInfo: not a SHA-256: {sha}")))?;
    Ok(FileInfo { target: text(o.get("target")), kind: text(o.get("type")), url, sha256 })
}

fn timestamp(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Int(n)) => *n,
        Some(Value::Double(d)) if d.is_finite() => *d as i64,
        _ => 0,
    }
}

fn version(v: &Value) -> Result<Version, Error> {
    let o = v.as_object().ok_or_else(|| bad("Expected VersionInfo to be an object"))?;
    if !has_all(o, &["version", "changelog", "timestamp", "files"]) {
        return Err(bad("Malformed VersionInfo"));
    }
    let Some(Value::Array(files)) = o.get("files") else {
        return Err(bad("VersionInfo: Expected an array of files"));
    };
    Ok(Version {
        number: text(o.get("version")),
        changelog: text(o.get("changelog")),
        timestamp: timestamp(o.get("timestamp")),
        files: files.iter().map(file).collect::<Result<_, _>>()?,
    })
}

fn channel(v: &Value) -> Result<(QString, Channel), Error> {
    let o = v.as_object().ok_or_else(|| bad("Expected ChannelInfo to be an object"))?;
    if !has_all(o, &["id", "title", "description", "versions"]) {
        return Err(bad("Malformed ChannelInfo"));
    }
    let Some(Value::Array(versions)) = o.get("versions") else {
        return Err(bad("ChannelInfo: Expected an array of versions"));
    };
    let mut versions: Vec<Version> = versions.iter().map(version).collect::<Result<_, _>>()?;
    // newest first; equal keys keep the directory's order
    versions.sort_by_key(|v| std::cmp::Reverse(version_key(&v.number)));
    let id = o.get("id").map(Value::to_qstring).unwrap_or_default();
    let ch = Channel { id: String::from_utf16_lossy(&id), title: text(o.get("title")), description: text(o.get("description")), versions };
    Ok((id, ch))
}

/// `UpdateRegistry::fillFromJson`: the directory, or why it is not one. A directory without channels is an
/// error too (the C++ registry's state is then `ErrorOccured`).
pub fn parse(text: &[u8]) -> Result<Directory, Error> {
    let doc = qjson::from_json(text).map_err(|e| bad(format!("Failed to parse the document: {}", e.text())))?;
    let obj = doc.as_object().ok_or_else(|| bad("Json document is not an object"))?;
    let Some(channels) = obj.get("channels") else {
        return Err(bad("No channels data in json file"));
    };
    let Value::Array(list) = channels else {
        return Err(bad("Expected to get an array of channels"));
    };
    let mut map = BTreeMap::new();
    for v in list {
        let (id, ch) = channel(v).map_err(|e| bad(format!("Failed to parse update information: {e}")))?;
        map.insert(id, ch);
    }
    if map.is_empty() {
        return Err(bad("The directory has no channels"));
    }
    Ok(Directory { channels: map })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_keys() {
        assert_eq!(version_key("0.98.3"), Some(98_003));
        assert_eq!(version_key("1.0.0-rc"), Some(1_000_000));
        assert_eq!(version_key("c6d5ae6e"), None);
        assert_eq!(version_key(""), None);
        assert_eq!(version_key("1..2"), None);
        assert_eq!(version_key("+1.2"), None);
        assert_eq!(version_key("100.0.0.0"), Some(100_000_000_000));
        assert_eq!(version_key("999999999.999999999.999999999"), Some(1_001_000_998_998_999));
        assert_eq!(version_key("9.9.9.9.9.9.9.9"), None);
        assert_eq!(compare("0.98.3", "0.99.1-rc"), std::cmp::Ordering::Less);
        assert_eq!(compare("abc", "0.1"), std::cmp::Ordering::Less);
    }

    const SHA: &str = "ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    fn dir(files: &str, versions_extra: &str) -> String {
        format!(
            r#"{{"channels":[{{"id":"release","title":"Release","description":"d","versions":[{{"version":"0.1.0","changelog":"","timestamp":1,"files":[{files}]}}{versions_extra}]}},{{"id":"development","title":"Dev","description":"","versions":[]}}]}}"#
        )
    }

    #[test]
    fn reads_a_directory() {
        let f = format!(r#"{{"target":"f7","type":"full_dfu","url":"https://x/a.dfu","sha256":"{SHA}"}}"#);
        let d = parse(dir(&f, r#",{"version":"0.2.0","changelog":"","timestamp":2,"files":[]}"#).as_bytes()).unwrap();
        assert_eq!(d.channel_names(), ["release", "development"]);
        let r = d.channel("release").unwrap();
        assert_eq!(r.latest().unwrap().number, "0.2.0");
        let f = r.versions[1].file("full_dfu", "f7").unwrap();
        assert_eq!(f.sha256, SHA.to_ascii_lowercase());
        assert!(d.channel("development").unwrap().latest().is_none());
    }

    #[test]
    fn refuses_unchecked_files() {
        for f in [
            r#"{"target":"f7","type":"full_dfu","url":"https://x/a","sha256":""}"#.to_string(),
            r#"{"target":"f7","type":"full_dfu","url":"https://x/a","sha256":5}"#.to_string(),
            format!(r#"{{"target":"f7","type":"full_dfu","url":"http://x/a","sha256":"{SHA}"}}"#),
            format!(r#"{{"target":"f7","type":"full_dfu","url":"file:///etc/passwd","sha256":"{SHA}"}}"#),
            format!(r#"{{"target":"f7","type":"full_dfu","url":"https://x/a","sha256":"{SHA}0"}}"#),
            format!(r#"{{"target":"f7","type":"full_dfu","sha256":"{SHA}"}}"#),
        ] {
            assert!(parse(dir(&f, "").as_bytes()).is_err(), "{f}");
        }
        assert!(parse(br#"{"channels":[]}"#).is_err());
        assert!(parse(br#"{"channels":{}}"#).is_err());
    }
}
