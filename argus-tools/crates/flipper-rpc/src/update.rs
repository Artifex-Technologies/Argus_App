//! A firmware update through the device's own updater, at the protocol level: the update
//! package goes to `/ext/update/<folder>/` and `PB_System.UpdateRequest` arms the updater, then
//! the device reboots into it. The C++ app's `FullUpdateOperation` with a local `.tgz`
//! (`PathCreateOperation`, `ChecksumVerifyOperation`, `FilesUploadOperation`,
//! `StartUpdaterOperation`), request for request. No DFU here: the updater on the device does
//! the flashing, and nothing in this module talks USB DFU.
//!
//! Differences from the C++ app, on purpose:
//! - The package is checked whole before the port is opened (the tar rules of
//!   [`crate::restore`]): one folder of plain files with an `update.fuf`, ASCII names. The C++
//!   app unpacks it to a temporary folder, takes the first folder by name and its files (it
//!   skips hidden files and subfolders); a package with anything else is refused here.
//! - No region provisioning (it needs Flipper's server; phase 7). The C++ app skips it for
//!   development-region devices, which the fake is.
//! - After the reboot it waits for the device to leave, not for the update to finish.

use std::io::Read;

use crate::ops::FileKind;
use crate::proto::pb::main::Content;
use crate::proto::pb::CommandStatus;
use crate::proto::pb_system;
use crate::restore::{bad, gunzip, read_tar, ArchiveError};
use crate::session::{RpcError, Session};
use crate::transport::Transport;
use crate::write::RebootMode;

/// Where packages go on the device (`REMOTE_DIR` in fullupdateoperation.cpp).
pub const REMOTE_DIR: &str = "/ext/update";
/// The manifest the updater reads.
pub const MANIFEST: &str = "update.fuf";

#[derive(Debug, Clone, Copy)]
pub struct PackageLimits {
    pub max_tar: u64,
    pub max_file: u64,
    pub max_files: usize,
}

impl Default for PackageLimits {
    /// A Flipper Zero update package is a few MiB.
    fn default() -> PackageLimits {
        PackageLimits { max_tar: 256 << 20, max_file: 128 << 20, max_files: 256 }
    }
}

/// A checked update package: its folder name and its files, in the C++ app's order
/// (`QDir::entryList`: names compared without case).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePackage {
    pub dir: String,
    pub files: Vec<(String, Vec<u8>)>,
}

impl UpdatePackage {
    pub fn remote_dir(&self) -> String {
        format!("{REMOTE_DIR}/{}", self.dir)
    }
    pub fn manifest(&self) -> String {
        format!("{}/{MANIFEST}", self.remote_dir())
    }
}

/// Reads and checks an update package (`.tgz`). Nothing is written anywhere.
pub fn read_update_package(input: impl Read, limits: &PackageLimits) -> Result<UpdatePackage, ArchiveError> {
    let t = gunzip(input, limits.max_tar)?;
    let entries = read_tar(&t, limits.max_files + 1)?;
    let Some(top) = entries.first().filter(|e| e.dir && e.parts.len() == 1) else {
        return Err(bad("an update package starts with its folder"));
    };
    let dir = top.parts[0].clone();
    if !dir.is_ascii() || dir.starts_with('.') {
        return Err(bad(format!("the package folder {dir:?} is not a plain ASCII name")));
    }
    let mut files = Vec::new();
    for e in &entries[1..] {
        if e.parts[0] != dir || e.parts.len() != 2 || e.dir {
            return Err(bad(format!("{:?}: an update package is one folder of files", e.parts.join("/"))));
        }
        let name = &e.parts[1];
        if !name.is_ascii() || name.starts_with('.') {
            return Err(bad(format!("{name:?} is not a plain ASCII name")));
        }
        if e.data.len() as u64 > limits.max_file {
            return Err(bad(format!("{name} is larger than {} bytes", limits.max_file)));
        }
        files.push((name.clone(), t[e.data.clone()].to_vec()));
    }
    if files.len() > limits.max_files {
        return Err(bad(format!("more than {} files", limits.max_files)));
    }
    if !files.iter().any(|(n, _)| n == MANIFEST) {
        return Err(bad(format!("no {MANIFEST} in the package")));
    }
    // QDir's default order: names compared case-insensitively
    files.sort_by(|(a, _), (b, _)| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()).then(a.cmp(b)));
    Ok(UpdatePackage { dir, files })
}

/// The updater's answers to `UpdateRequest` (`UpdatePrepareResult` in the firmware).
pub fn update_result_text(code: i32) -> &'static str {
    match code {
        0 => "OK",
        1 => "manifest path invalid",
        2 => "manifest folder not found",
        3 => "manifest invalid",
        4 => "stage missing",
        5 => "stage integrity error",
        6 => "manifest pointer create error",
        7 => "manifest pointer check error",
        8 => "target mismatch",
        9 => "outdated manifest version",
        10 => "internal storage full",
        _ => "unspecified error",
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateReport {
    /// Folders made under `/ext/update`.
    pub dirs_made: u64,
    /// Files sent (the ones missing on the device or with another MD5).
    pub files_sent: u64,
    /// Files already there with the same MD5.
    pub files_kept: u64,
    pub bytes: u64,
}

impl<T: Transport> Session<T> {
    /// `PB_System.UpdateRequest`: arms the updater with a manifest on the SD card. Returns the
    /// updater's result code (0 = OK); a refusal comes as `ErrorInvalidParameters` with the code.
    pub fn system_update(&mut self, manifest: &str) -> Result<i32, RpcError> {
        crate::write::check_write_path(manifest)?;
        let m =
            self.request_any_status(Content::SystemUpdateRequest(pb_system::UpdateRequest { update_manifest: manifest.to_string() }))?;
        match m.content {
            Some(Content::SystemUpdateResponse(r)) => Ok(r.code),
            _ if m.command_status != CommandStatus::Ok as i32 => {
                Err(RpcError::Device(CommandStatus::try_from(m.command_status).unwrap_or(CommandStatus::Error)))
            }
            other => Err(RpcError::Unexpected(format!("expected an update response, got {other:?}"))),
        }
    }

    /// Uploads `pkg` and starts the device's updater (see the module text), then reboots into
    /// it. The session is over afterwards ([`Session::wait_until_gone`]).
    pub fn update(&mut self, pkg: &UpdatePackage) -> Result<UpdateReport, RpcError> {
        let mut report = UpdateReport::default();
        // checkStorage (StorageInfoRefreshOperation): the SD card must be there
        if self.storage_info("/ext")?.is_none() {
            return Err(RpcError::Invalid("SD Card is not installed or malfunctioning".into()));
        }
        self.storage_stat("/ext/Manifest")?;
        // createUpdatePath (PathCreateOperation): stat from /ext/update down, mkdir the rest
        let remote = pkg.remote_dir();
        let parts: Vec<&str> = remote[1..].split('/').collect();
        let mut existed = true;
        let mut i = 1;
        while i < parts.len() {
            let path = format!("/{}", parts[..=i].join("/"));
            match self.storage_stat(&path)? {
                Some(e) if e.kind == FileKind::File => return Err(RpcError::Invalid(format!("Remote path {path} is a regular file"))),
                Some(_) => i += 1,
                None => {
                    existed = false;
                    break;
                }
            }
        }
        while i < parts.len() {
            self.storage_mkdir(&format!("/{}", parts[..=i].join("/")))?;
            report.dirs_made += 1;
            i += 1;
        }
        // verifyExistingFiles (ChecksumVerifyOperation), only when the folder was there
        let mut send: Vec<&(String, Vec<u8>)> = pkg.files.iter().collect();
        if existed {
            let mut changed = Vec::new();
            for f in &pkg.files {
                let path = format!("{remote}/{}", f.0);
                let same = match self.storage_md5(&path) {
                    Ok(sum) => sum == md5_hex(&f.1),
                    Err(RpcError::Device(CommandStatus::ErrorStorageNotExist)) => false,
                    Err(e) => return Err(e),
                };
                if same {
                    report.files_kept += 1;
                } else {
                    changed.push(f);
                }
            }
            send = changed;
        }
        // uploadUpdateFiles (FilesUploadOperation)
        for (name, data) in send {
            self.storage_write(&format!("{remote}/{name}"), data)?;
            report.files_sent += 1;
            report.bytes += data.len() as u64;
        }
        // startUpdate (StartUpdaterOperation): arm the updater, reboot into it
        let code = self.system_update(&pkg.manifest())?;
        if code != 0 {
            return Err(RpcError::Invalid(format!("the updater refused the package: {} ({code})", update_result_text(code))));
        }
        self.system_reboot(RebootMode::Update)?;
        Ok(report)
    }
}

fn md5_hex(data: &[u8]) -> String {
    use md5::Digest;
    md5::Md5::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}
