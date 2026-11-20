//! One Argus app at a time. Two would race for the same serial port and the same datastore, so the first one to
//! start holds a lock in the private runtime folder (`$XDG_RUNTIME_DIR/argus/app.lock`) for as long as it runs, and
//! listens on `app.sock` next to it. A later start finds the lock taken, asks the running one to come forward, and
//! exits. The lock is the kernel's (flock), so it is released when the process ends, even after a crash.
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

const SHOW: &[u8] = b"show\n";

pub enum Instance {
    /// This is the only one: keep the guard for the life of the process.
    First(Guard),
    /// Another one runs; it was asked to come forward.
    Running,
}

pub struct Guard {
    _lock: File,
    listener: UnixListener,
}

/// Takes the lock in `dir` (made 0700 if missing), or asks the app holding it to come forward.
pub fn claim(dir: &Path) -> io::Result<Instance> {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let lock = OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(dir.join("app.lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            if let Ok(mut s) = UnixStream::connect(dir.join("app.sock")) {
                let _ = s.write_all(SHOW);
            }
            return Ok(Instance::Running);
        }
        Err(std::fs::TryLockError::Error(e)) => return Err(e),
    }
    // the lock is ours, so a socket left here is a dead app's
    let sock = dir.join("app.sock");
    match std::fs::remove_file(&sock) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let listener = UnixListener::bind(&sock)?;
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))?;
    Ok(Instance::First(Guard { _lock: lock, listener }))
}

impl Guard {
    /// Calls `show` each time a later start asks; the lock moves to the listening thread and is held until exit.
    pub fn on_show(self, show: impl Fn() + Send + 'static) {
        std::thread::spawn(move || {
            let Guard { _lock, listener } = self;
            for conn in listener.incoming().flatten() {
                let _ = conn.set_read_timeout(Some(Duration::from_secs(1)));
                let mut buf = [0u8; 8];
                let mut n = 0;
                let mut conn = conn;
                while n < SHOW.len() {
                    match conn.read(&mut buf[n..]) {
                        Ok(0) | Err(_) => break,
                        Ok(k) => n += k,
                    }
                }
                if &buf[..n] == SHOW {
                    show();
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn a_second_start_finds_the_first_and_asks_it_to_come_forward() {
        let dir = std::env::temp_dir().join(format!("argus-single-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let Instance::First(guard) = claim(&dir).unwrap() else { panic!("the first start must get the lock") };
        let (tx, rx) = mpsc::channel();
        guard.on_show(move || tx.send(()).unwrap());
        assert!(matches!(claim(&dir).unwrap(), Instance::Running));
        rx.recv_timeout(Duration::from_secs(5)).expect("the running app was asked to come forward");
        assert!(matches!(claim(&dir).unwrap(), Instance::Running), "still held after a request");
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
