//! A content-addressed object store: every file is kept once, under its SHA-256.
//! Layout: `<root>/objects/<first 2 hex>/<remaining 62 hex>`. Folders are 0700 and files 0600:
//! an SD card holds secrets (U2F keys, saved Wi-Fi), so its snapshots must not be world-readable.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use md5::Md5;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectInfo {
    pub sha256: String,
    pub md5: String,
    pub size: u64,
}

pub struct Store {
    root: PathBuf,
}

pub fn valid_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn mkdir_private(p: &Path) -> Result<()> {
    fs::create_dir_all(p)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(p, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Write `data` to `path` through a temporary file and a rename, so readers never see half a file.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path.parent().ok_or_else(|| Error::new("bad_path", "no parent folder"))?;
    let mut tmp = tempfile::Builder::new().prefix(".tmp-").tempfile_in(dir)?;
    tmp.write_all(data)?;
    tmp.as_file().sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o600))?;
    }
    tmp.persist(path).map_err(|e| Error::from(e.error))?;
    Ok(())
}

impl Store {
    pub fn open(root: &Path) -> Result<Store> {
        mkdir_private(root)?;
        mkdir_private(&root.join("objects"))?;
        Ok(Store { root: root.to_path_buf() })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn object_path(&self, sha: &str) -> Option<PathBuf> {
        valid_sha256(sha).then(|| self.root.join("objects").join(&sha[..2]).join(&sha[2..]))
    }

    pub fn has(&self, sha: &str) -> bool {
        self.object_path(sha).is_some_and(|p| p.is_file())
    }

    /// Store what `r` yields. Returns its hashes; an object already present is not written again.
    pub fn put_reader(&self, r: &mut dyn Read) -> Result<ObjectInfo> {
        let objects = self.root.join("objects");
        let mut tmp = tempfile::Builder::new().prefix(".in-").tempfile_in(&objects)?;
        let (mut sha, mut md5) = (Sha256::new(), Md5::new());
        let mut size = 0u64;
        let mut buf = vec![0u8; 64 << 10];
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            sha.update(&buf[..n]);
            md5.update(&buf[..n]);
            tmp.write_all(&buf[..n])?;
            size += n as u64;
        }
        let info = ObjectInfo { sha256: hex::encode(sha.finalize()), md5: hex::encode(md5.finalize()), size };
        let dest = self.object_path(&info.sha256).expect("hex digest");
        if dest.is_file() {
            return Ok(info);
        }
        mkdir_private(dest.parent().expect("has parent"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o600))?;
        }
        tmp.persist(&dest).map_err(|e| Error::from(e.error))?;
        Ok(info)
    }

    /// Store a local file. Refuses anything that is not a regular file (a symlink is not followed).
    pub fn put_file(&self, path: &Path) -> Result<ObjectInfo> {
        let meta = fs::symlink_metadata(path)?;
        if !meta.file_type().is_file() {
            return Err(Error::new("not_a_file", format!("{} is not a regular file", path.display())));
        }
        self.put_reader(&mut File::open(path)?)
    }

    /// Re-hash an object and compare it with its name.
    pub fn verify(&self, sha: &str) -> bool {
        let Some(p) = self.object_path(sha) else { return false };
        let Ok(mut f) = File::open(p) else { return false };
        let mut h = Sha256::new();
        io::copy(&mut f, &mut h).is_ok() && hex::encode(h.finalize()) == sha
    }

    /// Every object's digest.
    pub fn all_objects(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for d in fs::read_dir(self.root.join("objects"))? {
            let d = d?;
            let prefix = d.file_name().to_string_lossy().into_owned();
            if prefix.len() != 2 || !d.file_type()?.is_dir() {
                continue;
            }
            for f in fs::read_dir(d.path())? {
                let name = format!("{prefix}{}", f?.file_name().to_string_lossy());
                if valid_sha256(&name) {
                    out.push(name);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    pub fn remove(&self, sha: &str) -> Result<()> {
        if let Some(p) = self.object_path(sha) {
            fs::remove_file(p)?;
        }
        Ok(())
    }
}
