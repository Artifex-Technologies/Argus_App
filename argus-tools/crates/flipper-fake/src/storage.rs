//! The fake's storage once a client may change it: every file's bytes, and the rules the
//! firmware applies to writes (`rpc_storage.c` and `storage_processing.c` in Argus_Firmware).
//!
//! - `/int` is an alias of `/ext/.int` on current firmware and always exists: `mkdir /int` is
//!   "already exists", removing it removes only what is in it (recursive) or fails while it holds
//!   something; `/any` is `/ext`. Any other top-level name is an invalid name.
//! - Write, mkdir and the new name of a rename must be ASCII (`path_contains_only_ascii`).
//! - A write opens its file with `FSOM_CREATE_ALWAYS` on the first frame of a command id
//!   (truncating it), appends every frame's data, and answers once, after the frame without
//!   `has_next`, or at the first error. Any other storage request in between ends the write and
//!   answers its id with "continuous command interrupted".
//! - Removing something that does not exist is OK; a folder that still holds ASCII-named entries
//!   needs `recursive`.

use std::collections::BTreeMap;

use flipper_rpc::proto::pb::CommandStatus;

use crate::Node;

#[derive(Debug, Clone, Default)]
pub struct Storage {
    /// Every folder and file, as in [`crate::FakeFlipper::nodes`]; sizes follow the contents.
    pub nodes: Vec<Node>,
    /// The bytes of every file.
    pub contents: BTreeMap<String, Vec<u8>>,
    /// The command id and path of a write in progress.
    writing: Option<(u32, String)>,
    /// Every change is refused with "access denied", as a write-protected or failing card.
    pub read_only: bool,
}

/// What a storage request did: answer this id with this status (OK answers carry no content).
pub type Answer = (u32, CommandStatus);

fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &path[..i],
    }
}

impl Storage {
    pub fn new(nodes: &[Node], data: impl Fn(&Node) -> Vec<u8>) -> Storage {
        let mut s = Storage { nodes: nodes.to_vec(), ..Default::default() };
        for n in nodes.iter().filter(|n| !n.dir) {
            s.contents.insert(n.path.clone(), data(n));
        }
        for root in ["/int", "/ext"] {
            if !s.is_dir(root) {
                s.nodes.push(Node { path: root.into(), dir: true, size: 0 });
            }
        }
        s
    }

    pub fn find(&self, path: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.path == path)
    }

    pub fn is_dir(&self, path: &str) -> bool {
        self.find(path).is_some_and(|n| n.dir)
    }

    pub fn data(&self, path: &str) -> Vec<u8> {
        self.contents.get(path).cloned().unwrap_or_default()
    }

    /// Children names (ASCII or not) of a folder.
    fn children(&self, path: &str) -> Vec<String> {
        let prefix = if path == "/" { "/".to_string() } else { format!("{path}/") };
        self.nodes.iter().filter_map(|n| n.path.strip_prefix(&prefix).filter(|r| !r.contains('/')).map(str::to_string)).collect()
    }

    /// `storage_get_data`: `/any` becomes `/ext`; only `/int`, `/ext` and what is below them
    /// are storage paths.
    fn resolve(path: &str) -> Result<String, CommandStatus> {
        let path = match path.strip_prefix("/any") {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("/ext{rest}"),
            _ => path.to_string(),
        };
        let ok = ["/int", "/ext"].iter().any(|r| path == *r || path.starts_with(&format!("{r}/")));
        if ok && !path.ends_with('/') {
            Ok(path)
        } else {
            Err(CommandStatus::ErrorStorageInvalidName)
        }
    }

    /// Ends a write in progress, as `rpc_system_storage_reset_state(.., send_error)`.
    pub fn reset(&mut self, send_error: bool) -> Option<Answer> {
        let (id, _) = self.writing.take()?;
        send_error.then_some((id, CommandStatus::ErrorContinuousCommandInterrupted))
    }

    /// Ends a write at the end of an RPC session (the firmware closes the file).
    pub fn end_session(&mut self) {
        self.writing = None;
    }

    pub fn mkdir(&mut self, id: u32, path: &str) -> Vec<Answer> {
        let mut out: Vec<Answer> = self.reset(true).into_iter().collect();
        let status = if !path.is_ascii() {
            CommandStatus::ErrorStorageInvalidName
        } else {
            match Self::resolve(path) {
                Err(s) => s,
                Ok(p) if self.find(&p).is_some() || p == "/int" || p == "/ext" => CommandStatus::ErrorStorageExist,
                Ok(_) if self.read_only => CommandStatus::ErrorStorageDenied,
                Ok(p) if !self.is_dir(parent(&p)) => CommandStatus::ErrorStorageNotExist,
                Ok(p) => {
                    self.nodes.push(Node { path: p, dir: true, size: 0 });
                    CommandStatus::Ok
                }
            }
        };
        out.push((id, status));
        out
    }

    pub fn remove(&mut self, id: u32, path: &str, recursive: bool) -> Vec<Answer> {
        let mut out: Vec<Answer> = self.reset(true).into_iter().collect();
        let status = match Self::resolve(path) {
            Err(s) => s,
            Ok(p) if p == "/ext" => CommandStatus::ErrorStorageInvalidName,
            Ok(p) if self.read_only && self.find(&p).is_some() => CommandStatus::ErrorStorageDenied,
            Ok(p) => match self.find(&p).cloned() {
                None if p == "/int" => CommandStatus::Ok,
                None => CommandStatus::Ok,
                Some(n) if !n.dir => {
                    self.drop_subtree(&p, true);
                    CommandStatus::Ok
                }
                Some(_) => {
                    let names = self.children(&p);
                    if names.is_empty() {
                        // /int is only emptied: the alias makes it again at once.
                        self.drop_subtree(&p, p != "/int");
                        CommandStatus::Ok
                    } else if !names.iter().any(|n| n.is_ascii()) {
                        // FatFS denies it, and the firmware's emptiness check skips names that
                        // are not ASCII, so it reports the denial itself.
                        CommandStatus::ErrorStorageDenied
                    } else if recursive {
                        self.drop_subtree(&p, p != "/int");
                        CommandStatus::Ok
                    } else {
                        CommandStatus::ErrorStorageDirNotEmpty
                    }
                }
            },
        };
        out.push((id, status));
        out
    }

    fn drop_subtree(&mut self, path: &str, with_root: bool) {
        let prefix = format!("{path}/");
        self.nodes.retain(|n| !(n.path.starts_with(&prefix) || (with_root && n.path == path)));
        self.contents.retain(|p, _| !(p.starts_with(&prefix) || (with_root && p == path)));
    }

    pub fn rename(&mut self, id: u32, old: &str, new: &str) -> Vec<Answer> {
        let mut out: Vec<Answer> = self.reset(true).into_iter().collect();
        let status = if !new.is_ascii() {
            CommandStatus::ErrorStorageInvalidName
        } else {
            match (Self::resolve(old), Self::resolve(new)) {
                (Err(s), _) | (_, Err(s)) => s,
                (Ok(o), Ok(n)) => {
                    if self.find(&o).is_none() || o == "/int" || o == "/ext" {
                        CommandStatus::ErrorStorageNotExist
                    } else if self.find(&n).is_some() || n == "/int" || n == "/ext" {
                        CommandStatus::ErrorStorageExist
                    } else if !self.is_dir(parent(&n)) {
                        CommandStatus::ErrorStorageNotExist
                    } else if self.read_only {
                        CommandStatus::ErrorStorageDenied
                    } else if n.starts_with(&format!("{o}/")) {
                        // A folder into itself; the fake refuses it as an invalid name.
                        CommandStatus::ErrorStorageInvalidName
                    } else {
                        let moved = |p: &str| -> Option<String> {
                            if p == o {
                                Some(n.clone())
                            } else {
                                p.strip_prefix(&format!("{o}/")).map(|rest| format!("{n}/{rest}"))
                            }
                        };
                        for node in &mut self.nodes {
                            if let Some(p) = moved(&node.path) {
                                node.path = p;
                            }
                        }
                        let contents = std::mem::take(&mut self.contents);
                        self.contents = contents.into_iter().map(|(p, d)| (moved(&p).unwrap_or(p), d)).collect();
                        CommandStatus::Ok
                    }
                }
            }
        };
        out.push((id, status));
        out
    }

    /// One frame of a write. Returns the answers to send (none while the write goes on).
    pub fn write(&mut self, id: u32, path: &str, data: &[u8], has_next: bool) -> Vec<Answer> {
        let mut out = Vec::new();
        if !path.is_ascii() {
            self.writing = None;
            out.push((id, CommandStatus::ErrorStorageInvalidName));
            return out;
        }
        if self.writing.as_ref().is_some_and(|(w, _)| *w != id) {
            out.extend(self.reset(true));
        }
        if self.writing.is_none() {
            match self.open(path) {
                Ok(p) => self.writing = Some((id, p)),
                Err(status) => {
                    out.push((id, status));
                    return out;
                }
            }
        }
        let (wid, p) = self.writing.clone().expect("opened above");
        let file = self.contents.entry(p.clone()).or_default();
        file.extend_from_slice(data);
        let size = file.len() as u32;
        if let Some(n) = self.nodes.iter_mut().find(|n| n.path == p) {
            n.size = size;
        }
        if !has_next {
            self.writing = None;
            out.push((wid, CommandStatus::Ok));
        }
        out
    }

    /// `storage_file_open(.., FSAM_WRITE, FSOM_CREATE_ALWAYS)`: the parent must be a folder,
    /// the path must not be one; an existing file is truncated.
    fn open(&mut self, path: &str) -> Result<String, CommandStatus> {
        let p = Self::resolve(path)?;
        if p == "/int" || p == "/ext" || self.is_dir(&p) || self.read_only {
            return Err(CommandStatus::ErrorStorageDenied);
        }
        if !self.is_dir(parent(&p)) {
            return Err(CommandStatus::ErrorStorageNotExist);
        }
        if self.find(&p).is_none() {
            self.nodes.push(Node { path: p.clone(), dir: false, size: 0 });
        }
        self.contents.insert(p.clone(), Vec::new());
        if let Some(n) = self.nodes.iter_mut().find(|n| n.path == p) {
            n.size = 0;
        }
        Ok(p)
    }

    /// A factory reset formats the internal storage on the next boot: `/int` is emptied.
    pub fn factory_reset(&mut self) {
        self.writing = None;
        self.drop_subtree("/int", false);
    }

    /// One line per folder and file, sorted: `d PATH` or `f PATH SIZE MD5`. Two runs whose
    /// dumps are equal left the same bytes on the fake.
    pub fn dump(&self) -> String {
        use md5::Digest;
        let mut lines: Vec<String> = self
            .nodes
            .iter()
            .map(|n| {
                if n.dir {
                    format!("d {}", n.path.escape_debug())
                } else {
                    let data = self.data(&n.path);
                    let sum: String = md5::Md5::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
                    format!("f {} {} {sum}", n.path.escape_debug(), data.len())
                }
            })
            .collect();
        lines.sort();
        lines.join("\n") + "\n"
    }
}
