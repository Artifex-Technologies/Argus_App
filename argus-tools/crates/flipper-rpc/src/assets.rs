//! The resources ("assets", the databases) a firmware install puts on the SD card, as the C++ app's
//! `AssetsDownloadOperation` installs them after a repair or an update: the bundle (`resources.tgz`, its
//! `resources/Manifest`) is compared with the device's `/ext/Manifest`, the files that went away or changed
//! are removed, and what is new or changed is written, the manifest last. Same requests in the same order
//! as the C++ app (`compare-repair.sh`).
//!
//! The manifest (`AssetManifest`): lines `V:version`, `T:timestamp`, `D:folder` and `F:md5:size:path`; a
//! folder must be listed before what is in it. Fixed in both versions: a name that could lead outside `/ext`
//! (an empty part, `.`, `..`, a backslash, a control character) or a parent that is a file made a manifest
//! valid, so a device's own `/ext/Manifest` could make the app remove `/ext/../int/...`; now such a manifest is
//! invalid (a device's is then treated as missing: a fresh install). And a file the bundle's manifest names
//! but the bundle lacks, or holds with another size or MD5, was written empty (or as it was) under the
//! manifest's claim; now the bundle is refused before anything is sent.

use std::cmp::Ordering;
use std::collections::{BTreeMap, VecDeque};
use std::io::Read;

use md5::{Digest, Md5};

use crate::proto::pb::CommandStatus;
use crate::restore::{gunzip, read_tar, ArchiveError, BackupArchive, RestoreReport};
use crate::session::{RpcError, Session};
use crate::transport::Transport;
use crate::write::RebootMode;

/// The device's manifest.
pub const DEVICE_MANIFEST: &str = "/ext/Manifest";
/// The largest device manifest read (Flipper's is about 50 KiB).
pub const MAX_DEVICE_MANIFEST: u64 = 4 << 20;

#[derive(Debug, Clone, Copy)]
pub struct BundleLimits {
    /// Bytes of tar once decompressed.
    pub max_tar: u64,
    pub max_entries: usize,
}

impl Default for BundleLimits {
    fn default() -> BundleLimits {
        BundleLimits { max_tar: 256 << 20, max_entries: 20_000 }
    }
}

/// A name as `QString` orders it (`QMap<QString, ...>` keys): by UTF-16 code units.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Key(String);

impl Ord for Key {
    fn cmp(&self, other: &Key) -> Ordering {
        self.0.encode_utf16().cmp(other.0.encode_utf16())
    }
}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Key) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Info {
    Dir,
    File { md5: String, size: i64 },
}

#[derive(Debug, Clone)]
struct Node {
    path: String,
    info: Info,
    children: BTreeMap<Key, Node>,
}

impl Node {
    fn root() -> Node {
        Node { path: String::new(), info: Info::Dir, children: BTreeMap::new() }
    }

    fn is_dir(&self) -> bool {
        self.info == Info::Dir
    }

    /// `FileNode::find`: "" is the node itself.
    fn find(&self, path: &str) -> Option<&Node> {
        if path.is_empty() {
            return Some(self);
        }
        path.split('/').try_fold(self, |n, part| n.children.get(&Key(part.to_string())))
    }

    fn find_mut(&mut self, parts: &[&str]) -> Option<&mut Node> {
        parts.iter().try_fold(self, |n, part| n.children.get_mut(&Key(part.to_string())))
    }

    /// `FileNode::toPreOrderList` (breadth first, children in name order), this node first.
    fn list(&self) -> Vec<Item> {
        let mut out = Vec::new();
        let mut queue = VecDeque::from([self]);
        while let Some(n) = queue.pop_front() {
            queue.extend(n.children.values());
            out.push(Item { path: n.path.clone(), dir: n.is_dir() });
        }
        out
    }

    /// `FileNode::difference`: the nodes of `other` that this tree lacks (or holds as the other type),
    /// each with everything below it.
    fn difference(&self, other: &Node) -> Vec<Item> {
        let mut out = Vec::new();
        let mut queue = VecDeque::from([other]);
        while let Some(n) = queue.pop_front() {
            match self.find(&n.path) {
                Some(c) if c.is_dir() == n.is_dir() => queue.extend(n.children.values()),
                _ => out.extend(n.list()),
            }
        }
        out
    }

    /// `FileNode::changed`: the nodes of `other` this tree holds with other contents (size or MD5).
    fn changed(&self, other: &Node) -> Vec<Item> {
        let mut out = Vec::new();
        let mut queue = VecDeque::from([other]);
        while let Some(n) = queue.pop_front() {
            match self.find(&n.path) {
                Some(c) if c.is_dir() == n.is_dir() => {
                    if c.info != n.info {
                        out.push(Item { path: n.path.clone(), dir: n.is_dir() });
                    } else {
                        queue.extend(n.children.values());
                    }
                }
                _ => {}
            }
        }
        out
    }
}

/// One entry of a manifest, by its path under `/ext`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub path: String,
    pub dir: bool,
}

#[derive(Debug, Clone)]
pub struct Manifest {
    pub version: i32,
    pub timestamp: i64,
    root: Node,
}

/// A part of a manifest path that stays inside `/ext`.
fn safe_part(p: &str) -> bool {
    !p.is_empty() && p != "." && p != ".." && !p.chars().any(|c| c == '\\' || c.is_control())
}

impl Manifest {
    /// `AssetManifest(text)`: every line must be one of the four kinds; version and timestamp must be set.
    pub fn parse(text: &[u8]) -> Result<Manifest, String> {
        let text = std::str::from_utf8(text).map_err(|_| "the manifest is not UTF-8".to_string())?;
        let mut m = Manifest { version: -1, timestamp: 0, root: Node::root() };
        let body = text.strip_suffix('\n').unwrap_or(text);
        if !text.is_empty() {
            for (i, line) in body.split('\n').enumerate() {
                if !m.parse_line(line.trim()) {
                    return Err(format!("syntax error on line {}", i + 1));
                }
            }
        }
        if m.version == -1 || m.timestamp == 0 {
            return Err("incomplete manifest".into());
        }
        Ok(m)
    }

    fn parse_line(&mut self, line: &str) -> bool {
        let t: Vec<&str> = line.split(':').collect();
        match (t[0], t.len()) {
            ("F", 4..) => match t[2].trim().parse::<i64>() {
                Ok(size) => self.add(t[3], Info::File { md5: t[1].to_string(), size }),
                Err(_) => false,
            },
            ("D", 2..) => self.add(t[1].strip_suffix('/').unwrap_or(t[1]), Info::Dir),
            ("V", 2..) => t[1].trim().parse::<i32>().map(|v| self.version = v).is_ok(),
            ("T", 2..) => t[1].trim().parse::<i64>().map(|v| self.timestamp = v).is_ok(),
            _ => false,
        }
    }

    /// `FileNode::addFile`/`addDirectory`: the parent must be there (and, since the fix, a folder); a name
    /// given twice replaces the first.
    fn add(&mut self, path: &str, info: Info) -> bool {
        let parts: Vec<&str> = path.split('/').collect();
        if !parts.iter().all(|p| safe_part(p)) {
            return false;
        }
        let (name, parents) = parts.split_last().expect("split gives one part at least");
        match self.root.find_mut(parents) {
            Some(parent) if parent.is_dir() => {
                parent.children.insert(Key(name.to_string()), Node { path: path.to_string(), info, children: BTreeMap::new() });
                true
            }
            _ => false,
        }
    }

    /// Every entry, breadth first (the root left out).
    pub fn items(&self) -> Vec<Item> {
        self.root.list().split_off(1)
    }

    /// Size and MD5 of a file entry.
    pub fn file(&self, path: &str) -> Option<(i64, &str)> {
        match &self.root.find(path)?.info {
            Info::File { md5, size } => Some((*size, md5)),
            Info::Dir => None,
        }
    }
}

/// The bundle: its manifest and the files it holds (paths under `resources/`).
#[derive(Debug, Clone)]
pub struct Bundle {
    pub manifest: Manifest,
    pub files: BTreeMap<String, Vec<u8>>,
}

fn bad(m: impl Into<String>) -> ArchiveError {
    ArchiveError(m.into())
}

/// Reads and checks `resources.tgz` before anything is sent: the archive by the rules every archive the
/// app reads meets, a valid `resources/Manifest`, and every file it names present with its size and MD5.
pub fn read_bundle(input: impl Read, limits: &BundleLimits) -> Result<Bundle, ArchiveError> {
    let t = gunzip(input, limits.max_tar)?;
    let mut files = BTreeMap::new();
    for e in read_tar(&t, limits.max_entries)? {
        if !e.dir && e.parts.len() > 1 && e.parts[0] == "resources" {
            files.insert(e.parts[1..].join("/"), t[e.data.clone()].to_vec());
        }
    }
    // the manifest stays among the files: it is written to the device last
    let text = files.get("Manifest").ok_or_else(|| bad("no resources/Manifest"))?;
    let manifest = Manifest::parse(text).map_err(|e| bad(format!("resources/Manifest: {e}")))?;
    for item in manifest.items().iter().filter(|i| !i.dir) {
        let (size, md5) = manifest.file(&item.path).expect("a file item");
        let data = files.get(&item.path).ok_or_else(|| bad(format!("the manifest names {}, which the bundle lacks", item.path)))?;
        let digest: String = Md5::digest(data).iter().map(|b| format!("{b:02x}")).collect();
        if data.len() as i64 != size || !digest.eq_ignore_ascii_case(md5) {
            return Err(bad(format!("{} differs from the manifest (size or MD5)", item.path)));
        }
    }
    Ok(Bundle { manifest, files })
}

/// What the device's `/ext/Manifest` turned out to be.
#[derive(Debug, Clone)]
pub enum DeviceManifest {
    /// Not there (or not a file): a fresh install.
    Missing,
    /// There but invalid: a fresh install too.
    Invalid,
    /// There, but the read failed: compared as an empty manifest (as the C++ app does).
    Unread,
    Read(Manifest),
}

/// What to remove (in this order) and what to write (folders made, files written), as
/// `AssetsDownloadOperation::buildFileLists` decides it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub remove: Vec<Item>,
    pub write: Vec<Item>,
}

pub fn plan(local: &Manifest, device: &DeviceManifest) -> Plan {
    let manifest = Item { path: "Manifest".into(), dir: false };
    let (deleted, added, changed) = match device {
        DeviceManifest::Missing | DeviceManifest::Invalid => {
            let added = local.items();
            let deleted: Vec<Item> = added.iter().filter(|i| !i.dir).cloned().collect();
            (deleted, added, vec![manifest])
        }
        DeviceManifest::Unread | DeviceManifest::Read(_) => {
            let empty = Node::root();
            let dev = match device {
                DeviceManifest::Read(m) => &m.root,
                _ => &empty,
            };
            let deleted: Vec<Item> = local.root.difference(dev).into_iter().filter(|i| !i.dir).collect();
            let added = dev.difference(&local.root);
            let mut changed = dev.changed(&local.root);
            if !deleted.is_empty() || !added.is_empty() || !changed.is_empty() {
                changed.insert(0, manifest);
            }
            (deleted, added, changed)
        }
    };
    let mut remove: Vec<Item> = deleted.into_iter().chain(changed.iter().cloned()).collect();
    remove.reverse();
    Plan { remove, write: added.into_iter().chain(changed).collect() }
}

/// What [`Session::install_assets`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetsReport {
    /// False when the device has no SD card (nothing was done, as in the C++ app).
    pub card: bool,
    pub device_manifest: &'static str,
    pub removed: usize,
    pub dirs_made: usize,
    pub files_written: usize,
    pub bytes: u64,
}

fn ext(path: &str) -> String {
    format!("/ext/{path}")
}

impl<T: Transport> Session<T> {
    /// `AssetsDownloadOperation`: the bundle onto `/ext`.
    pub fn install_assets(&mut self, bundle: &Bundle) -> Result<AssetsReport, RpcError> {
        let mut r = AssetsReport { card: false, device_manifest: "missing", removed: 0, dirs_made: 0, files_written: 0, bytes: 0 };
        if self.storage_info("/ext")?.is_none() {
            return Ok(r);
        }
        r.card = true;
        let device = match self.storage_stat(DEVICE_MANIFEST)? {
            Some(e) if e.kind == crate::ops::FileKind::File => {
                let mut text = Vec::new();
                match self.storage_read(DEVICE_MANIFEST, &mut text, MAX_DEVICE_MANIFEST) {
                    Ok(_) => match Manifest::parse(&text) {
                        Ok(m) => DeviceManifest::Read(m),
                        Err(_) => DeviceManifest::Invalid,
                    },
                    // the device refused the read: the stream is where it was
                    Err(RpcError::Device(_)) => DeviceManifest::Unread,
                    Err(e) => return Err(e),
                }
            }
            _ => DeviceManifest::Missing,
        };
        r.device_manifest = match device {
            DeviceManifest::Missing => "missing",
            DeviceManifest::Invalid => "invalid",
            DeviceManifest::Unread => "unread",
            DeviceManifest::Read(_) => "read",
        };
        let p = plan(&bundle.manifest, &device);
        for item in &p.remove {
            self.storage_remove(&ext(&item.path), false)?;
            r.removed += 1;
        }
        for item in &p.write {
            let path = ext(&item.path);
            if item.dir {
                // a folder that is there already is fine (the C++ mkdir takes it as done)
                match self.storage_mkdir(&path) {
                    Ok(()) | Err(RpcError::Device(CommandStatus::ErrorStorageExist)) => r.dirs_made += 1,
                    Err(e) => return Err(e),
                }
            } else {
                let data = bundle.files.get(&item.path).ok_or_else(|| RpcError::Invalid(format!("{} is not in the bundle", item.path)))?;
                self.storage_write(&path, data)?;
                r.files_written += 1;
                r.bytes += data.len() as u64;
            }
        }
        Ok(r)
    }

    /// The steps of the C++ full repair (and update) once the firmware runs: the resources, the region
    /// data, a restart. The development region (0) needs no region data (the C++ app skips the step); any other
    /// needs `region_data`, the bytes for `/int/.region_data` made from Flipper's region bundle
    /// (`flipper_updates::region`), and without them this refuses before anything is sent. Returns the assets
    /// report; the device then restarts.
    ///
    /// `restore`: the legacy update's backup of `/int`, written back before the region step, so an older
    /// `/int/.region_data` in the backup never replaces the fresh one (both apps since 2026-11-12; the C++
    /// `LegacyUpdateOperation` used to restore after it).
    pub fn finish_install(
        &mut self,
        bundle: &Bundle,
        region: u32,
        region_data: Option<&[u8]>,
        restore: Option<&BackupArchive>,
    ) -> Result<(AssetsReport, Option<RestoreReport>), RpcError> {
        let region_data = check_region(region, region_data)?;
        let r = self.install_assets(bundle)?;
        let restored = match restore {
            Some(archive) => Some(self.restore(archive)?),
            None => None,
        };
        if let Some(data) = region_data {
            self.storage_write(REGION_DATA_PATH, data)?;
        }
        self.system_reboot(RebootMode::Os)?;
        Ok((r, restored))
    }
}

/// Where the firmware reads its region data (`RegionProvisioningOperation`).
pub const REGION_DATA_PATH: &str = "/int/.region_data";

/// The region data to write for a device of this region: none for the development region (0), else the
/// bytes given, which must be there (and not empty: an empty file is "no region data" to the firmware).
pub fn check_region(region: u32, region_data: Option<&[u8]>) -> Result<Option<&[u8]>, RpcError> {
    match (region, region_data) {
        (0, _) => Ok(None),
        (_, Some(d)) if !d.is_empty() => Ok(Some(d)),
        _ => Err(RpcError::Invalid(format!("the device's region ({region}) needs region data from Flipper's region bundle"))),
    }
}

/// The device's built-in region from its `devinfo` (`hardware.region.builtin`; 0 is the development region).
pub fn builtin_region(devinfo: &[(String, String)]) -> Option<u32> {
    devinfo.iter().find(|(k, _)| k == "hardware.region.builtin").and_then(|(_, v)| v.parse().ok())
}
