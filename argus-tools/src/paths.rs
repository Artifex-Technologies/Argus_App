//! Paths from hostile places (archive entries, the device, a Limbo guest) and the rules that keep
//! them from leading anywhere they should not.

use std::path::{Path, PathBuf};

/// An archive entry name as display text plus its parts, with "." dropped. `traversal` is set
/// when the name was absolute or had a ".." part (it is still listed, but never used as a path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryName {
    pub display: String,
    pub parts: Vec<String>,
    pub traversal: bool,
}

pub fn entry_name(raw: &[u8]) -> EntryName {
    let text = String::from_utf8_lossy(raw).replace('\\', "/");
    let mut traversal = text.starts_with('/') || text.contains('\0');
    let mut parts = Vec::new();
    for p in text.split('/') {
        match p {
            "" | "." => {}
            ".." => traversal = true,
            p => parts.push(p.to_string()),
        }
    }
    // a drive letter or a Windows device prefix is absolute too
    if parts.first().is_some_and(|p| p.len() == 2 && p.ends_with(':')) {
        traversal = true;
    }
    EntryName { display: crate::display_text(text.as_bytes(), 300), parts, traversal }
}

/// A folder name that may be used on the SD card under /ext/apps/: short, printable, no
/// separators, not hidden, not "." or "..".
pub fn valid_category(c: &str) -> bool {
    !c.is_empty()
        && c.len() <= 40
        && !c.starts_with('.')
        && c.chars().all(|ch| ch.is_ascii_alphanumeric() || " _-+().,&'".contains(ch))
}

/// The category (folder under /ext/apps) for an app found at `name` inside a package: the folder
/// after the last "apps" part, else the file's own folder; "Misc" when that is not a safe name.
pub fn category_for(name: &EntryName) -> String {
    if name.traversal || name.parts.len() < 2 {
        return "Misc".into();
    }
    let dirs = &name.parts[..name.parts.len() - 1];
    let pick = match dirs.iter().rposition(|p| p.eq_ignore_ascii_case("apps")) {
        Some(i) if i + 1 < dirs.len() => &dirs[i + 1],
        Some(_) => return "Misc".into(),
        None => &dirs[dirs.len() - 1],
    };
    if valid_category(pick) { pick.clone() } else { "Misc".into() }
}

/// A FAP file name for the device: the entry's basename if it is plain, else "<sha8>.fap".
pub fn device_file_name(name: &EntryName, sha256: &str) -> String {
    let base = name.parts.last().map(String::as_str).unwrap_or("");
    let ok = base.len() <= 64
        && base.to_ascii_lowercase().ends_with(".fap")
        && !base.starts_with('.')
        && base.chars().all(|c| c.is_ascii_alphanumeric() || "_-+.".contains(c));
    if ok { base.to_string() } else { format!("{}.fap", &sha256[..8.min(sha256.len())]) }
}

/// A device path Argus accepts from a listing: absolute, under /ext or /int, no "." / "..",
/// no backslash, NUL or other control characters.
pub fn valid_device_path(p: &str) -> bool {
    if !(p == "/ext" || p.starts_with("/ext/") || p == "/int" || p.starts_with("/int/")) {
        return false;
    }
    if p.chars().any(|c| c.is_control() || c == '\\') {
        return false;
    }
    p.split('/').skip(1).all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Where device path `p` lives inside a local mirror rooted at `root` (root/ext/...). None if the
/// path is not valid.
pub fn mirror_path(root: &Path, p: &str) -> Option<PathBuf> {
    if !valid_device_path(p) {
        return None;
    }
    let mut out = root.to_path_buf();
    for part in p.split('/').skip(1) {
        out.push(part);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_is_flagged() {
        for n in ["../x.fap", "a/../../x.fap", "/etc/x.fap", "C:/x.fap", "a\\..\\x.fap"] {
            assert!(entry_name(n.as_bytes()).traversal, "{n}");
        }
        assert!(!entry_name(b"./apps/Games/snake.fap").traversal);
    }

    #[test]
    fn categories() {
        assert_eq!(category_for(&entry_name(b"f7-update-1.0/apps/Games/snake.fap")), "Games");
        assert_eq!(category_for(&entry_name(b"apps/Sub-GHz/x/y.fap")), "Sub-GHz");
        assert_eq!(category_for(&entry_name(b"Tools/x.fap")), "Tools");
        assert_eq!(category_for(&entry_name(b"x.fap")), "Misc");
        assert_eq!(category_for(&entry_name(b"apps/../int/x.fap")), "Misc");
        assert_eq!(category_for(&entry_name(b"apps/.hidden/x.fap")), "Misc");
        assert_eq!(category_for(&entry_name("apps/bad\u{7}/x.fap".as_bytes())), "Misc");
        assert_eq!(category_for(&entry_name(b"apps/x.fap")), "Misc");
    }

    #[test]
    fn device_paths() {
        assert!(valid_device_path("/ext/apps/Games/a.fap"));
        for p in ["/ext/../int", "ext/a", "/ext//a", "/ext/a\\b", "/ext/./a", "/sys/x", "/ext/a\nb"] {
            assert!(!valid_device_path(p), "{p}");
        }
        assert_eq!(mirror_path(Path::new("/m"), "/ext/a/b").unwrap(), PathBuf::from("/m/ext/a/b"));
    }

    #[test]
    fn file_names() {
        let s = "0123456789abcdef";
        assert_eq!(device_file_name(&entry_name(b"apps/Games/snake_game.fap"), s), "snake_game.fap");
        assert_eq!(device_file_name(&entry_name(b"apps/Games/sn ake.fap"), s), "01234567.fap");
        assert_eq!(device_file_name(&entry_name(b"apps/Games/.fap"), s), "01234567.fap");
    }
}
