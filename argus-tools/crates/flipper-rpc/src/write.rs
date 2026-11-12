//! Operations that change the device: write a file, make a folder, remove, rename, reboot,
//! factory reset. They follow `StorageWriteOperation`, `StorageMkdirOperation`,
//! `StorageRemoveOperation`, `StorageRenameOperation`, `SystemRebootOperation` and
//! `SystemFactoryResetOperation` of the C++ app, request for request. Nothing calls them
//! unless a user asked for that change (argus-cli wants `--yes`).
//!
//! Differences from the C++ app, on purpose:
//! - Paths are checked before anything is sent ([`check_path`], plus ASCII only: the firmware
//!   refuses other names for writes, folders and new names, so refusing here keeps a
//!   half-done multi-file job from starting).
//! - A file write sends its chunks up to each status ping and waits for that ping's answer
//!   before going on (the C++ app writes the whole file at once and reads the answers as they
//!   come; the frames on the wire are the same). That bounds what is in flight, so a device that
//!   fails early is not sent the rest of a large file.

use crate::ops::check_path;
use crate::proto::pb::main::Content;
use crate::proto::{pb_storage, pb_system};
use crate::session::{RpcError, Session};
use crate::transport::Transport;

/// Bytes per write frame (`StorageWriteOperation::CHUNK_SIZE`).
pub const CHUNK: usize = 1024;
/// Files of at least this many bytes get status pings, about one per 1 % of the file.
const PING_FROM: usize = CHUNK * 100;
/// At most this many chunks between two pings (`CHUNKS_PER_PING_CAP`).
const PING_CAP: usize = 1000;
/// The largest file one write sends (the firmware's own storage is far smaller than this).
pub const MAX_WRITE: usize = 256 << 20;

pub use crate::proto::pb_system::reboot_request::RebootMode;

/// A device path for a request that creates or names something: [`check_path`] and ASCII.
pub fn check_write_path(path: &str) -> Result<(), RpcError> {
    check_path(path)?;
    if !path.is_ascii() {
        return Err(RpcError::Invalid(format!("the device takes only ASCII names: {path:?}")));
    }
    Ok(())
}

/// The frames of one file write, as the C++ `StorageWriteOperation` encodes them: 1 KiB
/// chunks with `has_next` on all but the last (an empty file is one frame without a `file`),
/// and after every `chunks_per_ping` chunks, unless it was the last, a ping. `None` marks a ping.
pub fn write_frames(path: &str, data: &[u8]) -> Vec<Option<(Content, bool)>> {
    let chunk = |bytes: &[u8], has_next: bool| {
        let file = (!bytes.is_empty()).then(|| pb_storage::File { data: bytes.to_vec(), ..Default::default() });
        Some((Content::StorageWriteRequest(pb_storage::WriteRequest { path: path.to_string(), file }), has_next))
    };
    if data.is_empty() {
        return vec![chunk(&[], false)];
    }
    let per_ping = if data.len() >= PING_FROM { (data.len() / PING_FROM).min(PING_CAP) } else { 0 };
    let n = data.len().div_ceil(CHUNK);
    let mut out = Vec::with_capacity(n + n / per_ping.max(1));
    for (i, c) in data.chunks(CHUNK).enumerate() {
        let k = i + 1;
        out.push(chunk(c, k < n));
        if per_ping > 0 && k % per_ping == 0 && k < n {
            out.push(None);
        }
    }
    out
}

fn ping() -> Content {
    Content::SystemPingRequest(pb_system::PingRequest { data: Vec::new() })
}

fn empty_answer(what: &'static str) -> impl FnMut(crate::proto::pb::Main) -> Result<(), RpcError> {
    move |m| match m.content {
        Some(Content::Empty(_)) | None => Ok(()),
        Some(other) => Err(RpcError::Unexpected(format!(
            "expected an empty answer to {what}, got {}",
            format!("{other:?}").split('(').next().unwrap_or("?")
        ))),
    }
}

impl<T: Transport> Session<T> {
    /// `PB_Storage.MkdirRequest`.
    pub fn storage_mkdir(&mut self, path: &str) -> Result<(), RpcError> {
        check_write_path(path)?;
        self.request(Content::StorageMkdirRequest(pb_storage::MkdirRequest { path: path.to_string() }), empty_answer("mkdir"))
    }

    /// `PB_Storage.DeleteRequest`. Removing something that does not exist succeeds (the
    /// firmware's rule); a folder that holds anything needs `recursive`.
    pub fn storage_remove(&mut self, path: &str, recursive: bool) -> Result<(), RpcError> {
        check_path(path)?;
        self.request(Content::StorageDeleteRequest(pb_storage::DeleteRequest { path: path.to_string(), recursive }), empty_answer("remove"))
    }

    /// `PB_Storage.RenameRequest`.
    pub fn storage_rename(&mut self, old: &str, new: &str) -> Result<(), RpcError> {
        check_path(old)?;
        check_write_path(new)?;
        let req = pb_storage::RenameRequest { old_path: old.to_string(), new_path: new.to_string() };
        self.request(Content::StorageRenameRequest(req), empty_answer("rename"))
    }

    /// `PB_Storage.WriteRequest`: creates or replaces the file at `path` with `data`.
    pub fn storage_write(&mut self, path: &str, data: &[u8]) -> Result<(), RpcError> {
        check_write_path(path)?;
        if data.len() > MAX_WRITE {
            return Err(RpcError::Invalid(format!("{path}: {} bytes is more than the {MAX_WRITE} one write sends", data.len())));
        }
        let id = self.new_id()?;
        let mut segment = Vec::new();
        for frame in write_frames(path, data) {
            match frame {
                Some(f) => segment.push(f),
                None => {
                    // the chunks so far and the ping, then wait for the ping's answer
                    segment.push((ping(), false));
                    self.send_frames(id, std::mem::take(&mut segment))?;
                    self.receive(id, |m| match m.content {
                        Some(Content::SystemPingResponse(_)) => Ok(true),
                        other => Err(RpcError::Unexpected(format!(
                            "{path}: expected a ping answer during the write, got {}",
                            other
                                .map(|c| format!("{c:?}").split('(').next().unwrap_or("?").to_string())
                                .unwrap_or_else(|| "no content".into())
                        ))),
                    })?;
                }
            }
        }
        self.send_frames(id, segment)?;
        self.receive(id, |m| match m.content {
            Some(Content::Empty(_)) | None => Ok(true),
            Some(other) => Err(RpcError::Unexpected(format!(
                "{path}: expected the write's empty answer, got {}",
                format!("{other:?}").split('(').next().unwrap_or("?")
            ))),
        })
    }

    /// `PB_System.RebootRequest`. The firmware reboots without answering, so this returns once
    /// the request is sent; the session is over (the device leaves USB:
    /// [`Session::wait_until_gone`]).
    pub fn system_reboot(&mut self, mode: RebootMode) -> Result<(), RpcError> {
        let id = self.new_id()?;
        self.send_frames(id, vec![(Content::SystemRebootRequest(pb_system::RebootRequest { mode: mode as i32 }), false)])
    }

    /// `PB_System.FactoryResetRequest`: the device sets the flag that formats its internal
    /// storage and reboots, without answering. Returns once the request is sent.
    pub fn factory_reset(&mut self) -> Result<(), RpcError> {
        let id = self.new_id()?;
        self.send_frames(id, vec![(Content::SystemFactoryResetRequest(pb_system::FactoryResetRequest {}), false)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(frames: &[Option<(Content, bool)>]) -> Vec<String> {
        frames
            .iter()
            .map(|f| match f {
                None => "ping".to_string(),
                Some((Content::StorageWriteRequest(w), next)) => {
                    format!("{}{}", w.file.as_ref().map(|f| f.data.len()).unwrap_or(0), if *next { "+" } else { "" })
                }
                Some(_) => "?".into(),
            })
            .collect()
    }

    #[test]
    fn chunks_and_pings_follow_the_cpp_operation() {
        assert_eq!(shape(&write_frames("/int/a", b"")), ["0"]);
        assert_eq!(shape(&write_frames("/int/a", &[1; 1024])), ["1024"]);
        assert_eq!(shape(&write_frames("/int/a", &[1; 1025])), ["1024+", "1"]);
        // 102399 bytes: 100 chunks, no pings
        assert!(!shape(&write_frames("/int/a", &vec![0; PING_FROM - 1])).contains(&"ping".to_string()));
        // 150 KiB: one ping per chunk (150 KiB / 100 KiB = 1), none after the last
        let s = shape(&write_frames("/int/a", &vec![0; 150 * 1024]));
        assert_eq!(s.len(), 150 + 149);
        assert_eq!(s[1], "ping");
        assert_eq!(s.last().unwrap(), "1024");
        // 300 KiB: a ping every 3 chunks
        let s = shape(&write_frames("/int/a", &vec![0; 300 * 1024]));
        assert_eq!(s.iter().filter(|x| *x == "ping").count(), 99);
        assert_eq!(&s[..4], ["1024+", "1024+", "1024+", "ping"]);
        assert!(write_frames("/int/a", b"x").iter().all(|f| f.is_some()));
    }

    #[test]
    fn write_paths() {
        assert!(check_write_path("/int/a.txt").is_ok());
        assert!(check_write_path("/int/caf\u{e9}").is_err());
        assert!(check_write_path("/int/../x").is_err());
    }
}
