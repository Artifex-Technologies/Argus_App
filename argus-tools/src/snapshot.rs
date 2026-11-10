//! Versioned SD card snapshots on top of [`crate::store`].
//!
//! `<root>/snapshots/<id>.json` lists a snapshot's files (device path, size, SHA-256, MD5); the
//! bytes live once in the object store. Taking a snapshot is a three-step dance with the app,
//! which owns the device link:
//!
//! 1. The app lists the card (`Listing`: paths and sizes) and asks for a [`plan`]: which files
//!    can be confirmed unchanged by asking the device for their MD5 (same path and size as in the
//!    previous snapshot), and which must be downloaded.
//! 2. The app asks the device for those MD5s, writes them into the listing, and downloads the rest
//!    into a staging folder that mirrors the card (`<staging>/ext/...`).
//! 3. [`create`] stores what was downloaded and reuses what the device's MD5 confirmed.
//!
//! [`diff`] compares a snapshot with another or with the card now; [`restore_plan`] says, file by
//! file, what a restore would create or overwrite. Nothing here deletes from the card.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::paths;
use crate::store::{self, Store};
use crate::{Error, Result};

pub const FORMAT: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRec {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub md5: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkipRec {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub format: u32,
    pub id: String,
    pub created_unix: u64,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub device: serde_json::Value,
    pub root: String,
    pub files: Vec<FileRec>,
    #[serde(default)]
    pub dirs: Vec<String>,
    #[serde(default)]
    pub skipped: Vec<SkipRec>,
}

/// What the app read from the card: paths and sizes, and the device's MD5 where it asked for it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Listing {
    #[serde(default = "default_root")]
    pub root: String,
    pub files: Vec<ListedFile>,
    #[serde(default)]
    pub dirs: Vec<String>,
}

fn default_root() -> String {
    "/ext".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListedFile {
    pub path: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub id: String,
    pub created_unix: u64,
    pub created: String,
    pub label: String,
    pub root: String,
    pub file_count: usize,
    pub total_bytes: u64,
    pub skipped: usize,
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn under_root(root: &str, p: &str) -> bool {
    p == root || p.starts_with(&format!("{root}/"))
}

/// UTC "YYYY-MM-DD HH:MM:SS" without a date crate (civil-from-days, Howard Hinnant's algorithm).
pub fn utc_text(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let secs = unix % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

pub struct Snapshots {
    pub store: Store,
}

impl Snapshots {
    pub fn open(root: &Path) -> Result<Snapshots> {
        let store = Store::open(root)?;
        store::mkdir_private(&root.join("snapshots"))?;
        Ok(Snapshots { store })
    }

    fn manifest_path(&self, id: &str) -> Result<std::path::PathBuf> {
        if !valid_id(id) {
            return Err(Error::new("bad_id", "a snapshot id is lowercase letters, digits and hyphens"));
        }
        Ok(self.store.root().join("snapshots").join(format!("{id}.json")))
    }

    pub fn load(&self, id: &str) -> Result<Snapshot> {
        let p = self.manifest_path(id)?;
        let raw = fs::read(&p).map_err(|_| Error::new("no_snapshot", format!("there is no snapshot {id}")))?;
        let s: Snapshot = serde_json::from_slice(&raw)?;
        if s.format != FORMAT {
            return Err(Error::new("bad_format", format!("snapshot {id} has format {}, expected {FORMAT}", s.format)));
        }
        Ok(s)
    }

    /// All snapshots, oldest first.
    pub fn list(&self) -> Result<Vec<Snapshot>> {
        let mut out = Vec::new();
        for e in fs::read_dir(self.store.root().join("snapshots"))? {
            let name = e?.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_suffix(".json") {
                if let Ok(s) = self.load(id) {
                    out.push(s);
                }
            }
        }
        out.sort_by(|a, b| (a.created_unix, &a.id).cmp(&(b.created_unix, &b.id)));
        Ok(out)
    }

    pub fn latest(&self) -> Result<Option<Snapshot>> {
        Ok(self.list()?.pop())
    }

    fn new_id(&self, now: u64) -> String {
        let t = utc_text(now).replace(['-', ':'], "").replace(' ', "-");
        let base = format!("snap-{t}");
        let mut id = base.clone();
        let mut n = 1;
        while self.manifest_path(&id).map(|p| p.exists()).unwrap_or(true) {
            n += 1;
            id = format!("{base}-{n}");
        }
        id
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        fs::remove_file(self.manifest_path(id)?).map_err(|_| Error::new("no_snapshot", format!("there is no snapshot {id}")))
    }

    /// Remove objects no snapshot refers to. Returns (objects removed, bytes freed).
    pub fn gc(&self) -> Result<(usize, u64)> {
        let mut live = BTreeSet::new();
        for s in self.list()? {
            live.extend(s.files.into_iter().map(|f| f.sha256));
        }
        let (mut n, mut bytes) = (0, 0);
        for sha in self.store.all_objects()? {
            if !live.contains(&sha) {
                if let Some(p) = self.store.object_path(&sha) {
                    bytes += fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                }
                self.store.remove(&sha)?;
                n += 1;
            }
        }
        Ok((n, bytes))
    }
}

pub fn summary(s: &Snapshot) -> Summary {
    Summary {
        id: s.id.clone(),
        created_unix: s.created_unix,
        created: utc_text(s.created_unix),
        label: s.label.clone(),
        root: s.root.clone(),
        file_count: s.files.len(),
        total_bytes: s.files.iter().map(|f| f.size).sum(),
        skipped: s.skipped.len(),
    }
}

/// A file the device can confirm unchanged: if its MD5 on the card equals `md5` (the previous
/// snapshot's), it need not be downloaded.
#[derive(Debug, Clone, Serialize)]
pub struct Verify {
    pub path: String,
    pub md5: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub base: Option<String>,
    /// Ask the device for these files' MD5; a match means no download.
    pub verify_md5: Vec<Verify>,
    /// Download these.
    pub download: Vec<String>,
    /// Listed paths Argus will not use (not under the root, or not a valid device path).
    pub refused: Vec<String>,
}

fn base_snapshot(snaps: &Snapshots, base: Option<&str>) -> Result<Option<Snapshot>> {
    match base {
        Some(id) => Ok(Some(snaps.load(id)?)),
        None => snaps.latest(),
    }
}

pub fn plan(snaps: &Snapshots, listing: &Listing, base: Option<&str>) -> Result<Plan> {
    let base = base_snapshot(snaps, base)?;
    let known: BTreeMap<&str, &FileRec> =
        base.as_ref().map(|b| b.files.iter().map(|f| (f.path.as_str(), f)).collect()).unwrap_or_default();
    let mut p = Plan { base: base.as_ref().map(|b| b.id.clone()), verify_md5: vec![], download: vec![], refused: vec![] };
    for f in &listing.files {
        if !paths::valid_device_path(&f.path) || !under_root(&listing.root, &f.path) {
            p.refused.push(crate::display_text(f.path.as_bytes(), 300));
            continue;
        }
        match known.get(f.path.as_str()) {
            Some(rec) if rec.size == f.size && snaps.store.has(&rec.sha256) => {
                p.verify_md5.push(Verify { path: f.path.clone(), md5: rec.md5.clone() })
            }
            _ => p.download.push(f.path.clone()),
        }
    }
    Ok(p)
}

/// Make a snapshot from a listing and a staging mirror. Files whose listed `md5` equals the base
/// snapshot's record (same path and size) are reused; everything else must be in `staging`.
pub fn create(
    snaps: &Snapshots,
    listing: &Listing,
    staging: Option<&Path>,
    base: Option<&str>,
    label: &str,
    device: serde_json::Value,
    now: u64,
) -> Result<Snapshot> {
    if !paths::valid_device_path(&listing.root) {
        return Err(Error::new("bad_root", "the listing's root is not a device path"));
    }
    let base = base_snapshot(snaps, base)?;
    let known: BTreeMap<&str, &FileRec> =
        base.as_ref().map(|b| b.files.iter().map(|f| (f.path.as_str(), f)).collect()).unwrap_or_default();
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    let mut seen = BTreeSet::new();
    for f in &listing.files {
        let shown = crate::display_text(f.path.as_bytes(), 300);
        if !paths::valid_device_path(&f.path) || !under_root(&listing.root, &f.path) {
            skipped.push(SkipRec { path: shown, reason: "not a valid path under the snapshot root".into() });
            continue;
        }
        if !seen.insert(f.path.clone()) {
            continue;
        }
        if let (Some(md5), Some(rec)) = (&f.md5, known.get(f.path.as_str())) {
            if rec.size == f.size && rec.md5.eq_ignore_ascii_case(md5) && snaps.store.has(&rec.sha256) {
                files.push((*rec).clone());
                continue;
            }
        }
        let Some(local) = staging.and_then(|s| paths::mirror_path(s, &f.path)) else {
            skipped.push(SkipRec { path: shown, reason: "not downloaded".into() });
            continue;
        };
        match snaps.store.put_file(&local) {
            Ok(info) => files.push(FileRec { path: f.path.clone(), size: info.size, sha256: info.sha256, md5: info.md5 }),
            Err(e) => skipped.push(SkipRec { path: shown, reason: e.message }),
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut dirs: Vec<String> =
        listing.dirs.iter().filter(|d| paths::valid_device_path(d) && under_root(&listing.root, d)).cloned().collect();
    dirs.sort();
    dirs.dedup();
    let snap = Snapshot {
        format: FORMAT,
        id: snaps.new_id(now),
        created_unix: now,
        label: crate::display_text(label.as_bytes(), 120),
        device,
        root: listing.root.clone(),
        files,
        dirs,
        skipped,
    };
    let path = snaps.manifest_path(&snap.id)?;
    store::write_atomic(&path, &serde_json::to_vec_pretty(&snap)?)?;
    Ok(snap)
}

#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub path: String,
    pub old_size: Option<u64>,
    pub new_size: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Diff {
    pub from: String,
    pub to: String,
    pub added: Vec<Change>,
    pub removed: Vec<Change>,
    pub changed: Vec<Change>,
    /// Same size, but no hash to compare (the card's MD5 was not asked for).
    pub unknown: Vec<Change>,
    pub unchanged: usize,
}

/// One side of a comparison: path -> (size, md5 if known).
fn side_of_snapshot(s: &Snapshot) -> BTreeMap<String, (u64, Option<String>)> {
    s.files.iter().map(|f| (f.path.clone(), (f.size, Some(f.md5.clone())))).collect()
}

fn side_of_listing(l: &Listing) -> BTreeMap<String, (u64, Option<String>)> {
    l.files
        .iter()
        .filter(|f| paths::valid_device_path(&f.path))
        .map(|f| (f.path.clone(), (f.size, f.md5.clone())))
        .collect()
}

fn compare(from: &str, to: &str, a: BTreeMap<String, (u64, Option<String>)>, b: BTreeMap<String, (u64, Option<String>)>) -> Diff {
    let mut d = Diff { from: from.into(), to: to.into(), ..Default::default() };
    for (p, (sa, ma)) in &a {
        match b.get(p) {
            None => d.removed.push(Change { path: p.clone(), old_size: Some(*sa), new_size: None }),
            Some((sb, mb)) => {
                let c = Change { path: p.clone(), old_size: Some(*sa), new_size: Some(*sb) };
                if sa != sb {
                    d.changed.push(c);
                } else {
                    match (ma, mb) {
                        (Some(x), Some(y)) if x.eq_ignore_ascii_case(y) => d.unchanged += 1,
                        (Some(_), Some(_)) => d.changed.push(c),
                        _ => d.unknown.push(c),
                    }
                }
            }
        }
    }
    for (p, (sb, _)) in &b {
        if !a.contains_key(p) {
            d.added.push(Change { path: p.clone(), old_size: None, new_size: Some(*sb) });
        }
    }
    d
}

pub fn diff_snapshots(a: &Snapshot, b: &Snapshot) -> Diff {
    compare(&a.id, &b.id, side_of_snapshot(a), side_of_snapshot(b))
}

/// Compare a snapshot with the card as listed now. Only paths under the snapshot's root count.
pub fn diff_listing(a: &Snapshot, now: &Listing) -> Diff {
    let mut b = side_of_listing(now);
    b.retain(|p, _| under_root(&a.root, p));
    compare(&a.id, "card", side_of_snapshot(a), b)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Not on the card: written.
    Create,
    /// On the card with other content: replaced (needs the user's confirmation).
    Overwrite,
    /// On the card with the same size; its content was not compared: replaced (confirm).
    OverwriteUnverified,
    /// On the card with the same MD5: left alone.
    Same,
}

#[derive(Debug, Clone, Serialize)]
pub struct RestoreItem {
    pub remote: String,
    pub local: String,
    pub size: u64,
    pub sha256: String,
    pub md5: String,
    pub action: Action,
}

#[derive(Debug, Clone, Serialize)]
pub struct RestorePlan {
    pub snapshot: String,
    pub selector: Option<String>,
    pub items: Vec<RestoreItem>,
    /// Folders to create first (parents of the files written).
    pub mkdirs: Vec<String>,
    pub overwrites: usize,
    pub creates: usize,
    pub same: usize,
    /// Records whose object is missing or corrupt in the store: not restorable.
    pub missing_objects: Vec<String>,
}

/// What restoring `selector` (one file, a folder, or everything when None) would do to the card
/// as `device` lists it now (None: assume every file exists with unknown content).
pub fn restore_plan(snaps: &Snapshots, snap: &Snapshot, selector: Option<&str>, device: Option<&Listing>) -> Result<RestorePlan> {
    if let Some(sel) = selector {
        if !paths::valid_device_path(sel) {
            return Err(Error::new("bad_path", "the path to restore is not a device path"));
        }
    }
    let now: BTreeMap<String, (u64, Option<String>)> = device.map(side_of_listing).unwrap_or_default();
    let mut plan = RestorePlan {
        snapshot: snap.id.clone(),
        selector: selector.map(String::from),
        items: vec![],
        mkdirs: vec![],
        overwrites: 0,
        creates: 0,
        same: 0,
        missing_objects: vec![],
    };
    let mut dirs = BTreeSet::new();
    for f in &snap.files {
        if let Some(sel) = selector {
            if !under_root(sel, &f.path) {
                continue;
            }
        }
        if !paths::valid_device_path(&f.path) {
            continue;
        }
        let Some(local) = snaps.store.object_path(&f.sha256).filter(|p| p.is_file()) else {
            plan.missing_objects.push(f.path.clone());
            continue;
        };
        let action = match (device, now.get(&f.path)) {
            (Some(_), None) => Action::Create,
            (None, _) => Action::OverwriteUnverified,
            (Some(_), Some((size, md5))) => {
                if *size != f.size {
                    Action::Overwrite
                } else {
                    match md5 {
                        Some(m) if m.eq_ignore_ascii_case(&f.md5) => Action::Same,
                        Some(_) => Action::Overwrite,
                        None => Action::OverwriteUnverified,
                    }
                }
            }
        };
        match action {
            Action::Create => plan.creates += 1,
            Action::Same => plan.same += 1,
            _ => plan.overwrites += 1,
        }
        let mut parent = f.path.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default();
        while parent.len() > 4 && paths::valid_device_path(&parent) {
            dirs.insert(parent.clone());
            parent = parent.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default();
        }
        plan.items.push(RestoreItem {
            remote: f.path.clone(),
            local: local.to_string_lossy().into_owned(),
            size: f.size,
            sha256: f.sha256.clone(),
            md5: f.md5.clone(),
            action,
        });
    }
    if let Some(dev) = device {
        let existing: BTreeSet<&str> = dev.dirs.iter().map(String::as_str).collect();
        dirs.retain(|d| !existing.contains(d.as_str()));
    }
    plan.mkdirs = dirs.into_iter().collect(); // BTreeSet order: parents before children
    Ok(plan)
}
