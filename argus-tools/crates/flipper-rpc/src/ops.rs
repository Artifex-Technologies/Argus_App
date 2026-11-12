//! Read-only operations, one per RPC request, as in `backend/flipperzero/rpc/` of the C++ app.
//! Nothing here writes to the device, changes its settings, its clock or its state.

use crate::proto::pb::main::Content;
use crate::proto::pb::CommandStatus;
use crate::proto::{pb_property, pb_storage, pb_system};
use crate::session::{RpcError, Session};
use crate::transport::Transport;

/// The firmware's limit for storage paths (nanopb option `PB_Storage.*.path max_length:255`).
pub const MAX_PATH_LEN: usize = 255;
/// Key/value pairs accepted from one device-info or property request.
const MAX_PAIRS: usize = 4096;
/// Entries accepted from one directory listing.
pub const MAX_LIST_ENTRIES: usize = 65_536;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
}

impl FileKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileKind::File => "file",
            FileKind::Dir => "dir",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: FileKind,
    pub size: u32,
}

/// The device clock, as `PB_System.DateTime` carries it (no time zone).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: u32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 1 = Monday ... 7 = Sunday, as the firmware counts.
    pub weekday: u32,
}

impl DateTime {
    /// `YYYY-MM-DDTHH:MM:SS`.
    pub fn iso(&self) -> String {
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", self.year, self.month, self.day, self.hour, self.minute, self.second)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageSpace {
    pub total: u64,
    pub free: u64,
}

/// Where the device information came from: the `devinfo` property (protobuf 0.14 and later,
/// dotted keys such as `hardware.name`) or the older device-info request (`hardware_name`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoSource {
    Property,
    Legacy,
}

/// Checks a device path before it is sent: absolute, at most 255 bytes, no NUL, no `.`/`..`
/// parts. The device would refuse most of these itself; refusing here keeps them off the wire.
pub fn check_path(path: &str) -> Result<(), RpcError> {
    if !path.starts_with('/') {
        return Err(RpcError::Invalid(format!("device path must start with '/': {path:?}")));
    }
    if path.len() > MAX_PATH_LEN {
        return Err(RpcError::Invalid(format!("device path is longer than {MAX_PATH_LEN} bytes")));
    }
    if path.contains('\0') {
        return Err(RpcError::Invalid("device path contains NUL".into()));
    }
    if path.split('/').any(|p| p == "." || p == "..") {
        return Err(RpcError::Invalid(format!("device path has '.' or '..' parts: {path:?}")));
    }
    Ok(())
}

fn unexpected(what: &str, got: &Option<Content>) -> RpcError {
    let name = match got {
        None => "no content".to_string(),
        Some(c) => format!("{c:?}").split('(').next().unwrap_or("?").to_string(),
    };
    RpcError::Unexpected(format!("expected {what}, got {name}"))
}

impl<T: Transport> Session<T> {
    /// `PB_System.ProtobufVersionRequest`: the protocol version the firmware speaks.
    pub fn protobuf_version(&mut self) -> Result<(u32, u32), RpcError> {
        let mut out = None;
        self.request(Content::SystemProtobufVersionRequest(pb_system::ProtobufVersionRequest {}), |m| match m.content {
            Some(Content::SystemProtobufVersionResponse(r)) => {
                out = Some((r.major, r.minor));
                Ok(())
            }
            other => Err(unexpected("a protobuf version response", &other)),
        })?;
        out.ok_or_else(|| RpcError::Unexpected("no protobuf version in the response".into()))
    }

    /// `PB_Property.GetRequest`: every key/value pair under `key` (e.g. `devinfo`).
    pub fn property_get(&mut self, key: &str) -> Result<Vec<(String, String)>, RpcError> {
        let mut out = Vec::new();
        self.request(Content::PropertyGetRequest(pb_property::GetRequest { key: key.to_string() }), |m| match m.content {
            Some(Content::PropertyGetResponse(r)) => push_pair(&mut out, r.key, r.value),
            // The last frame of a property stream may carry no pair.
            Some(Content::Empty(_)) | None => Ok(()),
            other => Err(unexpected("a property response", &other)),
        })?;
        Ok(out)
    }

    /// `PB_System.DeviceInfoRequest`: the older key/value device information.
    pub fn device_info_legacy(&mut self) -> Result<Vec<(String, String)>, RpcError> {
        let mut out = Vec::new();
        self.request(Content::SystemDeviceInfoRequest(pb_system::DeviceInfoRequest {}), |m| match m.content {
            Some(Content::SystemDeviceInfoResponse(r)) => push_pair(&mut out, r.key, r.value),
            Some(Content::Empty(_)) | None => Ok(()),
            other => Err(unexpected("a device info response", &other)),
        })?;
        Ok(out)
    }

    /// Device information the way the C++ app picks its source (`VCPDeviceInfoHelper`): the
    /// `devinfo` property from protobuf 0.14 on, the device-info request before. Unlike the
    /// C++ app this does not go on to set the device's clock.
    pub fn device_info(&mut self, version: (u32, u32)) -> Result<(InfoSource, Vec<(String, String)>), RpcError> {
        if version.0 > 0 || version.1 >= 14 {
            Ok((InfoSource::Property, self.property_get("devinfo")?))
        } else {
            Ok((InfoSource::Legacy, self.device_info_legacy()?))
        }
    }

    /// `PB_Storage.ListRequest`: the entries of one directory, in the device's order.
    pub fn storage_list(&mut self, path: &str) -> Result<Vec<Entry>, RpcError> {
        check_path(path)?;
        let mut out: Vec<Entry> = Vec::new();
        let req = pb_storage::ListRequest { path: path.to_string(), ..Default::default() };
        self.request(Content::StorageListRequest(req), |m| match m.content {
            Some(Content::StorageListResponse(r)) => {
                if out.len() + r.file.len() > MAX_LIST_ENTRIES {
                    return Err(RpcError::Limit(format!("directory listing longer than {MAX_LIST_ENTRIES} entries")));
                }
                for f in r.file {
                    let kind = match pb_storage::file::FileType::try_from(f.r#type) {
                        Ok(pb_storage::file::FileType::Dir) => FileKind::Dir,
                        Ok(pb_storage::file::FileType::File) => FileKind::File,
                        Err(_) => return Err(RpcError::Unexpected(format!("unknown file type {}", f.r#type))),
                    };
                    out.push(Entry { name: f.name, kind, size: f.size });
                }
                Ok(())
            }
            // An empty directory, or the C++ app's "no such path" case on old firmware.
            Some(Content::Empty(_)) | None => Ok(()),
            other => Err(unexpected("a list response", &other)),
        })?;
        Ok(out)
    }

    /// `PB_Storage.InfoRequest`: total and free space of `/int` or `/ext`. `None` when the
    /// device reports the storage missing (no SD card), which the C++ app treats the same way.
    pub fn storage_info(&mut self, path: &str) -> Result<Option<StorageSpace>, RpcError> {
        check_path(path)?;
        let mut out = None;
        let res = self.request(Content::StorageInfoRequest(pb_storage::InfoRequest { path: path.to_string() }), |m| match m.content {
            Some(Content::StorageInfoResponse(r)) => {
                out = Some(StorageSpace { total: r.total_space, free: r.free_space });
                Ok(())
            }
            other => Err(unexpected("a storage info response", &other)),
        });
        match res {
            Err(RpcError::Device(CommandStatus::ErrorStorageNotReady)) => Ok(None),
            Err(e) => Err(e),
            Ok(()) => Ok(out),
        }
    }

    /// `PB_Storage.StatRequest`: the entry at `path`, `None` if it does not exist.
    pub fn storage_stat(&mut self, path: &str) -> Result<Option<Entry>, RpcError> {
        check_path(path)?;
        let mut out = None;
        let res = self.request(Content::StorageStatRequest(pb_storage::StatRequest { path: path.to_string() }), |m| match m.content {
            Some(Content::StorageStatResponse(r)) => {
                if let Some(f) = r.file {
                    let kind = if f.r#type == pb_storage::file::FileType::Dir as i32 { FileKind::Dir } else { FileKind::File };
                    let name = path.rsplit('/').next().unwrap_or_default().to_string();
                    out = Some(Entry { name, kind, size: f.size });
                }
                Ok(())
            }
            Some(Content::Empty(_)) | None => Ok(()),
            other => Err(unexpected("a stat response", &other)),
        });
        match res {
            Err(RpcError::Device(CommandStatus::ErrorStorageNotExist)) => Ok(None),
            Err(e) => Err(e),
            Ok(()) => Ok(out),
        }
    }
}

impl<T: Transport> Session<T> {
    /// `PB_System.GetDateTimeRequest`: the device clock. Values out of range are refused.
    pub fn system_datetime(&mut self) -> Result<DateTime, RpcError> {
        let mut out = None;
        self.request(Content::SystemGetDatetimeRequest(pb_system::GetDateTimeRequest {}), |m| match m.content {
            Some(Content::SystemGetDatetimeResponse(r)) => {
                let d = r.datetime.unwrap_or_default();
                let dt = DateTime {
                    year: d.year,
                    month: d.month,
                    day: d.day,
                    hour: d.hour,
                    minute: d.minute,
                    second: d.second,
                    weekday: d.weekday,
                };
                let ok = (2000..=2099).contains(&dt.year)
                    && (1..=12).contains(&dt.month)
                    && (1..=31).contains(&dt.day)
                    && dt.hour < 24
                    && dt.minute < 60
                    && dt.second < 60
                    && (1..=7).contains(&dt.weekday);
                if !ok {
                    return Err(RpcError::Unexpected(format!("date/time out of range: {dt:?}")));
                }
                out = Some(dt);
                Ok(())
            }
            other => Err(unexpected("a date/time response", &other)),
        })?;
        out.ok_or_else(|| RpcError::Unexpected("no date/time in the response".into()))
    }

    /// `PB_Storage.Md5sumRequest`: the MD5 of a file, computed on the device, as 32 lowercase
    /// hex digits.
    pub fn storage_md5(&mut self, path: &str) -> Result<String, RpcError> {
        check_path(path)?;
        let mut out = None;
        self.request(Content::StorageMd5sumRequest(pb_storage::Md5sumRequest { path: path.to_string() }), |m| match m.content {
            Some(Content::StorageMd5sumResponse(r)) => {
                if r.md5sum.len() != 32 || !r.md5sum.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                    return Err(RpcError::Unexpected(format!("not an MD5 digest: {:?}", r.md5sum)));
                }
                out = Some(r.md5sum);
                Ok(())
            }
            other => Err(unexpected("an MD5 response", &other)),
        })?;
        out.ok_or_else(|| RpcError::Unexpected("no MD5 in the response".into()))
    }

    /// Reads a file into `out`, as the C++ `StorageReadOperation` does: a stat and a read
    /// request under one command id, the size from the stat, then the data in chunks until
    /// `has_next` is false. Refuses files larger than `max` before any data is written, and
    /// fails if the device sends more or fewer bytes than its stat said. Returns the size.
    pub fn storage_read(&mut self, path: &str, out: &mut dyn std::io::Write, max: u64) -> Result<u64, RpcError> {
        check_path(path)?;
        let p = path.to_string();
        let id = self.send(vec![
            Content::StorageStatRequest(pb_storage::StatRequest { path: p.clone() }),
            Content::StorageReadRequest(pb_storage::ReadRequest { path: p }),
        ])?;
        let mut size: Option<u64> = None;
        let mut got = 0u64;
        self.receive(id, |m| {
            let last = !m.has_next;
            match (m.content, size) {
                (Some(Content::StorageStatResponse(r)), None) => {
                    let f = r.file.ok_or_else(|| RpcError::Unexpected("stat response without a file".into()))?;
                    if u64::from(f.size) > max {
                        return Err(RpcError::Limit(format!("{path} is {} bytes, more than the {max} allowed", f.size)));
                    }
                    size = Some(u64::from(f.size));
                    Ok(false)
                }
                (Some(Content::StorageReadResponse(r)), Some(total)) => {
                    let data = r.file.map(|f| f.data).unwrap_or_default();
                    got += data.len() as u64;
                    if got > total {
                        return Err(RpcError::Unexpected(format!("{path}: more data than the {total} bytes its stat said")));
                    }
                    out.write_all(&data).map_err(|e| RpcError::Local(format!("cannot write {path}: {e}")))?;
                    Ok(last)
                }
                (other, _) => Err(unexpected(if size.is_none() { "a stat response" } else { "a read response" }, &other)),
            }
        })?;
        let total = size.unwrap_or(0);
        if got != total {
            return Err(RpcError::Unexpected(format!("{path}: {got} bytes read, its stat said {total}")));
        }
        Ok(got)
    }
}

fn push_pair(out: &mut Vec<(String, String)>, key: String, value: String) -> Result<(), RpcError> {
    if out.len() >= MAX_PAIRS {
        return Err(RpcError::Limit(format!("more than {MAX_PAIRS} key/value pairs")));
    }
    out.push((key, value));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check_path;

    #[test]
    fn paths() {
        assert!(check_path("/ext").is_ok());
        assert!(check_path("/").is_ok());
        assert!(check_path("/ext/apps/Games/snake game.fap").is_ok());
        assert!(check_path("ext").is_err());
        assert!(check_path("").is_err());
        assert!(check_path("/ext/../int").is_err());
        assert!(check_path("/ext/./x").is_err());
        assert!(check_path("/ext/a\0b").is_err());
        assert!(check_path(&format!("/{}", "a".repeat(255))).is_err());
        assert!(check_path(&format!("/{}", "a".repeat(254))).is_ok());
    }
}
