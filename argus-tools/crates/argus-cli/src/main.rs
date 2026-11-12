//! argus-cli: the Rust port of qFlipper-cli (cli/ in the C++ app), starting with read-only
//! commands. Every command prints one JSON object on stdout, `{"ok":true,...}` or
//! `{"ok":false,"error":{"code","message"}}`, like argus-tools. Exit status: 0 done,
//! 1 device or protocol error, 2 usage error.
//!
//! It never looks for devices: the Flipper's serial port (`--port`) or a socket carrying its
//! byte stream (`--socket`) must be named. Commands that change the device (`mkdir`, `rm`, `mv`,
//! `push`, `restore`, `update`, `reboot`, `erase`) do nothing without `--yes`; `pull` and `backup` write
//! only new local files (they never overwrite).

mod chymaera;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{json, Value};

use flipper_rpc::ops::{Entry, InfoSource};
use flipper_rpc::transfer::Limits;
use flipper_rpc::transport::{SerialTransport, Transport};
use flipper_rpc::{RpcError, Session};

/// How long a reboot or factory reset waits for the device to leave the port (`"gone"`).
const GONE_WAIT: Duration = Duration::from_secs(5);

const APP_VERSION: &str = include_str!("../../../../VERSION").trim_ascii();

const HELP: &str = "argus-cli — talk to a Flipper Zero over RPC (JSON on stdout)

  argus-cli (--port DEVICE | --socket PATH) [--timeout MS] COMMAND

  info               protobuf version and device information
  ls PATH            list a directory on the device, e.g. /ext
  stat PATH          one file or directory (exists: false when missing)
  df PATH            total and free space of /int or /ext
  md5 PATH           MD5 of a file, computed on the device
  date               the device clock
  pull PATH DEST     copy a file or directory off the device to DEST (must not exist)
  backup FILE        /int as a .tar.gz the C++ app's restore reads (FILE must not exist)
  screen DIR         the screen as PBM images (DIR must not exist); --frames N, default 1
  capture pull PATH  pull a capture folder (or a path inside one) into the datastore
  capture pull-all   pull every capture folder into the datastore
  chymaera ...       serve | watch | parse FILE | import FILE | selftest, as qFlipper-cli
                     chymaera (plain text like it; serve and watch use the device named
                     with --port/--socket, if any: see argus-cli chymaera)
  version | help

  These change the device and do nothing without --yes (exit 2, nothing opened):
  mkdir PATH         make a folder
  rm PATH            remove a file or an empty folder (--recursive: a folder and all in it)
  mv OLD NEW         rename or move
  push FILE PATH     write a local file to the device (replacing PATH)
  restore FILE       restore a backup of /int (from backup, or the C++ app's), then reboot;
                     the archive is checked whole first, before the port is opened: without
                     --yes only that check runs
  update FILE        upload an update package (.tgz) to /ext/update and start the device's
                     own updater (no DFU); checked whole first, like restore
  reboot             restart the device
  erase              factory reset: the device formats /int and restarts

  The other commands only read from the device. Names the device lists that could lead outside
  DEST (.., /, \\, NUL, control characters) are refused and listed under \"refused\".

  --port DEVICE   the Flipper's serial port, e.g. /dev/ttyACM0 (never guessed)
  --socket PATH   a Unix socket carrying the same byte stream (tests use a fake Flipper here)
  --timeout MS    per-read timeout, default 5000
  --datastore DB  the capture datastore, default the one the Argus app and the Sarina API use
                  ($XDG_DATA_HOME/Argus/chymaera/chymaera.db)";

enum Target {
    Port(PathBuf),
    Socket(PathBuf),
}

struct Usage(String);

struct Cmd {
    target: Option<Target>,
    datastore: Option<PathBuf>,
    timeout: Duration,
    frames: usize,
    yes: bool,
    recursive: bool,
    words: Vec<String>,
}

fn parse(args: Vec<String>) -> Result<Cmd, Usage> {
    let mut cmd = Cmd {
        target: None,
        datastore: None,
        timeout: Duration::from_millis(5000),
        frames: 1,
        yes: false,
        recursive: false,
        words: Vec::new(),
    };
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| Usage(format!("{name} needs a value")));
        match a.as_str() {
            "--port" | "--socket" => {
                if cmd.target.is_some() {
                    return Err(Usage("give one --port or --socket".into()));
                }
                let p = PathBuf::from(value(&a)?);
                cmd.target = Some(if a == "--port" { Target::Port(p) } else { Target::Socket(p) });
            }
            "--datastore" => cmd.datastore = Some(PathBuf::from(value("--datastore")?)),
            "--yes" => cmd.yes = true,
            "--recursive" => cmd.recursive = true,
            "--frames" => {
                let n: usize = value("--frames")?.parse().map_err(|_| Usage("--frames takes a number".into()))?;
                if !(1..=flipper_rpc::screen::MAX_FRAMES).contains(&n) {
                    return Err(Usage(format!("--frames must be between 1 and {}", flipper_rpc::screen::MAX_FRAMES)));
                }
                cmd.frames = n;
            }
            "--timeout" => {
                let ms: u64 = value("--timeout")?.parse().map_err(|_| Usage("--timeout takes milliseconds".into()))?;
                if !(10..=600_000).contains(&ms) {
                    return Err(Usage("--timeout must be between 10 and 600000 ms".into()));
                }
                cmd.timeout = Duration::from_millis(ms);
            }
            s if s.starts_with("--") => return Err(Usage(format!("unknown option {s}"))),
            _ => cmd.words.push(a),
        }
    }
    Ok(cmd)
}

/// JSON text with every control character escaped. serde_json escapes U+0000..U+001F but
/// leaves DEL and the C1 controls (U+0080..U+009F, e.g. CSI) raw; a file name from the device
/// must not be able to drive the terminal the output is printed on.
fn to_safe_json(v: &Value) -> String {
    let s = v.to_string();
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_control() {
            out.push_str(&format!("\\u{:04x}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

fn print(v: Value) {
    println!("{}", to_safe_json(&v));
}

fn fail(code: &str, message: String, status: u8) -> ExitCode {
    print(json!({"ok": false, "error": {"code": code, "message": message}}));
    ExitCode::from(status)
}

fn open(target: &Target, timeout: Duration) -> Result<Box<dyn Transport>, RpcError> {
    use std::os::unix::fs::FileTypeExt;
    let check = |p: &Path, want_socket: bool| -> Result<(), RpcError> {
        let ft = std::fs::metadata(p).map_err(RpcError::Io)?.file_type();
        let ok = if want_socket { ft.is_socket() } else { ft.is_char_device() };
        if ok {
            Ok(())
        } else {
            Err(RpcError::Invalid(format!("{} is not a {}", p.display(), if want_socket { "socket" } else { "serial device" })))
        }
    };
    match target {
        Target::Port(p) => {
            check(p, false)?;
            Ok(Box::new(SerialTransport::open(p, timeout).map_err(RpcError::Io)?))
        }
        Target::Socket(p) => {
            check(p, true)?;
            Ok(Box::new(std::os::unix::net::UnixStream::connect(p).map_err(RpcError::Io)?))
        }
    }
}

/// A local file to read: a regular file (a FIFO or device would hang or never end), at most
/// `max` bytes.
fn read_local(path: &Path, max: u64) -> Result<Vec<u8>, RpcError> {
    use std::io::Read;
    let meta = std::fs::metadata(path).map_err(|e| RpcError::Local(format!("{}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(RpcError::Local(format!("{} is not a regular file", path.display())));
    }
    let mut data = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(max + 1).read_to_end(&mut data))
        .map_err(|e| RpcError::Local(format!("{}: {e}", path.display())))?;
    if data.len() as u64 > max {
        return Err(RpcError::Local(format!("{} is larger than {max} bytes", path.display())));
    }
    Ok(data)
}

/// Reads and checks a backup without opening the device.
fn check_backup(path: &Path) -> Result<flipper_rpc::restore::BackupArchive, (String, String)> {
    let meta = std::fs::metadata(path).map_err(|e| ("local".to_string(), format!("{}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(("local".into(), format!("{} is not a regular file", path.display())));
    }
    let f = std::fs::File::open(path).map_err(|e| ("local".to_string(), format!("{}: {e}", path.display())))?;
    flipper_rpc::restore::read_backup(std::io::BufReader::new(f), &flipper_rpc::restore::ArchiveLimits::default())
        .map_err(|e| ("archive".to_string(), e.to_string()))
}

/// Reads and checks an update package without opening the device.
fn check_package(path: &Path) -> Result<flipper_rpc::update::UpdatePackage, (String, String)> {
    let meta = std::fs::metadata(path).map_err(|e| ("local".to_string(), format!("{}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(("local".into(), format!("{} is not a regular file", path.display())));
    }
    let f = std::fs::File::open(path).map_err(|e| ("local".to_string(), format!("{}: {e}", path.display())))?;
    flipper_rpc::update::read_update_package(std::io::BufReader::new(f), &flipper_rpc::update::PackageLimits::default())
        .map_err(|e| ("archive".to_string(), e.to_string().replace("backup refused", "update package refused")))
}

fn run(cmd: &Cmd) -> Result<Value, RpcError> {
    let target = cmd.target.as_ref().expect("checked by main");
    // read before the port is opened, so a bad local file changes nothing on the device
    let pushed = match cmd.words.as_slice() {
        [c, local, _] if c == "push" => Some(read_local(Path::new(local), flipper_rpc::write::MAX_WRITE as u64)?),
        _ => None,
    };
    let archive = match cmd.words.as_slice() {
        [c, file] if c == "restore" => Some(check_backup(Path::new(file)).map_err(|(_, m)| RpcError::Invalid(m))?),
        _ => None,
    };
    let package = match cmd.words.as_slice() {
        [c, file] if c == "update" => Some(check_package(Path::new(file)).map_err(|(_, m)| RpcError::Invalid(m))?),
        _ => None,
    };
    let mut s = Session::start(open(target, cmd.timeout)?, cmd.timeout)?;
    match cmd.words[0].as_str() {
        "update" => {
            let pkg = package.expect("checked above");
            let r = s.update(&pkg)?;
            let gone = s.wait_until_gone(GONE_WAIT);
            Ok(json!({
                "ok": true, "command": "update", "file": cmd.words[1], "package": pkg.dir, "manifest": pkg.manifest(),
                "dirs_made": r.dirs_made, "files_sent": r.files_sent, "files_kept": r.files_kept, "bytes": r.bytes,
                "updater_started": true, "gone": gone,
            }))
        }
        "mkdir" => {
            s.storage_mkdir(&cmd.words[1])?;
            Ok(json!({"ok": true, "command": "mkdir", "path": cmd.words[1]}))
        }
        "rm" => {
            s.storage_remove(&cmd.words[1], cmd.recursive)?;
            Ok(json!({"ok": true, "command": "rm", "path": cmd.words[1], "recursive": cmd.recursive}))
        }
        "mv" => {
            s.storage_rename(&cmd.words[1], &cmd.words[2])?;
            Ok(json!({"ok": true, "command": "mv", "from": cmd.words[1], "to": cmd.words[2]}))
        }
        "push" => {
            let data = pushed.expect("read above");
            s.storage_write(&cmd.words[2], &data)?;
            Ok(json!({"ok": true, "command": "push", "path": cmd.words[2], "bytes": data.len()}))
        }
        "restore" => {
            let r = s.restore(&archive.expect("checked above"))?;
            // as the C++ app: the device restarts so that it loads the restored settings
            s.system_reboot(flipper_rpc::write::RebootMode::Os)?;
            let gone = s.wait_until_gone(GONE_WAIT);
            Ok(json!({
                "ok": true, "command": "restore", "file": cmd.words[1], "dirs_made": r.dirs_made,
                "dirs_kept": r.dirs_kept, "files": r.files, "bytes": r.bytes, "rebooted": true, "gone": gone,
            }))
        }
        "reboot" => {
            s.system_reboot(flipper_rpc::write::RebootMode::Os)?;
            Ok(json!({"ok": true, "command": "reboot", "gone": s.wait_until_gone(GONE_WAIT)}))
        }
        "erase" => {
            s.factory_reset()?;
            Ok(json!({"ok": true, "command": "erase", "gone": s.wait_until_gone(GONE_WAIT)}))
        }
        "info" => {
            let version = s.protobuf_version()?;
            let (source, pairs) = s.device_info(version)?;
            let info: serde_json::Map<String, Value> = pairs.into_iter().map(|(k, v)| (k, Value::String(v))).collect();
            Ok(json!({
                "ok": true,
                "command": "info",
                "protobuf": {"major": version.0, "minor": version.1},
                "source": match source { InfoSource::Property => "property", InfoSource::Legacy => "legacy" },
                "info": info,
            }))
        }
        "ls" => {
            let path = &cmd.words[1];
            let entries: Vec<Value> = s.storage_list(path)?.iter().map(entry).collect();
            Ok(json!({"ok": true, "command": "ls", "path": path, "entries": entries}))
        }
        "stat" => {
            let path = &cmd.words[1];
            Ok(match s.storage_stat(path)? {
                Some(e) => json!({"ok": true, "command": "stat", "path": path, "exists": true, "entry": entry(&e)}),
                None => json!({"ok": true, "command": "stat", "path": path, "exists": false}),
            })
        }
        "df" => {
            let path = &cmd.words[1];
            Ok(match s.storage_info(path)? {
                Some(sp) => json!({"ok": true, "command": "df", "path": path, "present": true, "total": sp.total, "free": sp.free}),
                None => json!({"ok": true, "command": "df", "path": path, "present": false}),
            })
        }
        "md5" => {
            let path = &cmd.words[1];
            Ok(json!({"ok": true, "command": "md5", "path": path, "md5": s.storage_md5(path)?}))
        }
        "date" => {
            let d = s.system_datetime()?;
            Ok(json!({"ok": true, "command": "date", "datetime": d.iso(), "weekday": d.weekday}))
        }
        "pull" => {
            let (remote, dest) = (&cmd.words[1], PathBuf::from(&cmd.words[2]));
            let r = s.pull(remote, &dest, &Limits::default())?;
            Ok(
                json!({"ok": true, "command": "pull", "path": remote, "dest": dest.display().to_string(), "files": r.files, "dirs": r.dirs, "bytes": r.bytes, "refused": r.refused}),
            )
        }
        "capture" => {
            let all = cmd.words[1] == "pull-all";
            let roots: Vec<&str> = if all { argus_capture::sync::CAPTURE_ROOTS.to_vec() } else { vec![cmd.words[2].as_str()] };
            if let Some(bad) = roots.iter().find(|r| !argus_capture::sync::is_capture_root(r)) {
                return Err(RpcError::Invalid(format!(
                    "only the capture folders may be pulled, not {bad}: {}",
                    argus_capture::sync::CAPTURE_ROOTS.join(", ")
                )));
            }
            let db = match &cmd.datastore {
                Some(p) => p.clone(),
                None => argus_capture::store::default_path().ok_or_else(|| RpcError::Local("no home folder for the datastore".into()))?,
            };
            let store = argus_capture::store::Store::open(&db).map_err(|e| RpcError::Local(e.to_string()))?;
            let label = if all { "all capture folders".to_string() } else { cmd.words[2].clone() };
            let r = argus_capture::sync::pull(&mut s, &store, &roots, &label, &Limits::default()).map_err(|e| match e {
                argus_capture::sync::PullError::Rpc(e) => e,
                other => RpcError::Local(other.to_string()),
            })?;
            let failed: Vec<Value> = r.failed.iter().map(|(p, why)| json!({"path": p, "reason": why})).collect();
            let mut out = json!({
                "ok": r.failed.is_empty(),
                "command": if all { "capture pull-all" } else { "capture pull" },
                "new": r.new,
                "duplicates": r.duplicates,
                "failed": failed,
                "missing_roots": r.missing_roots,
                "bytes": r.bytes,
                "recorded": r.recorded,
                "datastore": db.display().to_string(),
            });
            if !r.failed.is_empty() {
                out["error"] = json!({"code": "failed", "message": format!("{} file(s) could not be read", r.failed.len())});
            }
            Ok(out)
        }
        "screen" => {
            use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
            let dir = PathBuf::from(&cmd.words[1]);
            let r = s.screen_frames(cmd.frames)?;
            std::fs::DirBuilder::new().mode(0o700).create(&dir).map_err(|e| RpcError::Local(format!("{}: {e}", dir.display())))?;
            let mut files = Vec::new();
            for (i, f) in r.frames.iter().enumerate() {
                let path = dir.join(format!("frame-{:04}.pbm", i + 1));
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)
                    .and_then(|mut file| std::io::Write::write_all(&mut file, &f.to_pbm()))
                    .map_err(|e| RpcError::Local(format!("{}: {e}", path.display())))?;
                files.push(path.display().to_string());
            }
            let orientation: Vec<i32> = r.frames.iter().map(|f| f.orientation).collect();
            Ok(
                json!({"ok": true, "command": "screen", "dir": dir.display().to_string(), "frames": r.frames.len(), "dropped": r.dropped, "orientation": orientation, "files": files}),
            )
        }
        "backup" => {
            let out = PathBuf::from(&cmd.words[1]);
            let mtime = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let r = s.backup(&out, &Limits::backup(), mtime)?;
            Ok(
                json!({"ok": true, "command": "backup", "file": out.display().to_string(), "files": r.files, "dirs": r.dirs, "bytes": r.bytes, "refused": r.refused}),
            )
        }
        _ => unreachable!("checked by main"),
    }
}

fn entry(e: &Entry) -> Value {
    json!({"name": e.name, "type": e.kind.as_str(), "size": e.size})
}

fn main() -> ExitCode {
    let cmd = match parse(std::env::args().skip(1).collect()) {
        Ok(c) => c,
        Err(Usage(m)) => return fail("usage", m, 2),
    };
    let words: Vec<&str> = cmd.words.iter().map(String::as_str).collect();
    if words.first() == Some(&"chymaera") {
        return chymaera::run(&cmd);
    }
    match words.as_slice() {
        ["help"] | [] => {
            println!("{HELP}");
            return ExitCode::SUCCESS;
        }
        ["version"] => {
            print(json!({"ok": true, "version": APP_VERSION}));
            return ExitCode::SUCCESS;
        }
        ["info"] | ["date"] | ["ls" | "stat" | "df" | "md5", _] | ["pull", _, _] | ["backup", _] => {}
        ["restore", file] => {
            // the archive is checked first, with or without --yes; the port is not opened yet
            match check_backup(Path::new(file)) {
                Err((code, m)) => return fail(&code, m, 1),
                Ok(a) if !cmd.yes => {
                    print(json!({
                        "ok": false, "command": "restore", "file": file, "checked": true,
                        "files": a.files(), "dirs": a.dirs(), "bytes": a.bytes(),
                        "error": {"code": "confirm", "message": "the backup is fine; restoring it changes the device: run again with --yes"},
                    }));
                    return ExitCode::from(2);
                }
                Ok(_) => {}
            }
        }
        ["update", file] => match check_package(Path::new(file)) {
            Err((code, m)) => return fail(&code, m, 1),
            Ok(p) if !cmd.yes => {
                print(json!({
                    "ok": false, "command": "update", "file": file, "checked": true, "package": p.dir, "files": p.files.len(),
                    "error": {"code": "confirm", "message": "the package is fine; installing it changes the device: run again with --yes"},
                }));
                return ExitCode::from(2);
            }
            Ok(_) => {}
        },
        ["mkdir" | "rm", _] | ["mv" | "push", _, _] | ["reboot"] | ["erase"] if !cmd.yes => {
            return fail("confirm", format!("{} changes the device; run again with --yes", words[0]), 2)
        }
        ["mkdir" | "rm", _] | ["mv" | "push", _, _] | ["reboot"] | ["erase"] => {}
        ["mkdir" | "rm" | "restore" | "update"] => return fail("usage", format!("{} needs a path", words[0]), 2),
        ["mv" | "push", ..] => return fail("usage", format!("{} needs two paths", words[0]), 2),
        ["screen", dir] if Path::new(dir).exists() => return fail("exists", format!("{dir} exists"), 1),
        ["screen", _] => {}
        ["screen"] => return fail("usage", "screen needs a new local folder".into(), 2),
        ["ls" | "stat" | "df" | "md5"] => return fail("usage", format!("{} needs a device path", words[0]), 2),
        ["pull", ..] => return fail("usage", "pull needs a device path and a new local path".into(), 2),
        ["capture", "pull", path] if !argus_capture::sync::is_capture_root(path) => {
            // refused before the device is even opened
            let roots = argus_capture::sync::CAPTURE_ROOTS.join(", ");
            return fail("not_allowed", format!("only the capture folders may be pulled, not {path}: {roots}"), 1);
        }
        ["capture", "pull", _] | ["capture", "pull-all"] => {}
        ["capture", ..] => return fail("usage", "capture pull PATH | capture pull-all".into(), 2),
        ["backup"] => return fail("usage", "backup needs a new local file".into(), 2),
        _ => return fail("usage", format!("unknown command: {}", words.join(" ")), 2),
    }
    if cmd.target.is_none() {
        return fail("usage", "name the Flipper with --port DEVICE or --socket PATH".into(), 2);
    }
    match run(&cmd) {
        Ok(v) => {
            let ok = v["ok"] == true;
            print(v);
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(e) => fail(e.code(), e.to_string(), 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_are_escaped() {
        let v = json!({"name": "a\u{9b}31m\u{7f}\u{1b}b"});
        assert_eq!(to_safe_json(&v), r#"{"name":"a\u009b31m\u007f\u001bb"}"#);
    }
}
