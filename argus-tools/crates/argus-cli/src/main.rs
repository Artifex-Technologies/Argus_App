//! argus-cli: the Rust port of qFlipper-cli (cli/ in the C++ app), starting with read-only
//! commands. Every command prints one JSON object on stdout, `{"ok":true,...}` or
//! `{"ok":false,"error":{"code","message"}}`, like argus-tools. Exit status: 0 done,
//! 1 device or protocol error, 2 usage error.
//!
//! It never looks for devices: the Flipper's serial port (`--port`) or a socket carrying its
//! byte stream (`--socket`) must be named. Nothing it does writes to the device.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{json, Value};

use flipper_rpc::ops::InfoSource;
use flipper_rpc::transport::{SerialTransport, Transport};
use flipper_rpc::{RpcError, Session};

const APP_VERSION: &str = include_str!("../../../../VERSION").trim_ascii();

const HELP: &str = "argus-cli — talk to a Flipper Zero over RPC (JSON on stdout)

  argus-cli (--port DEVICE | --socket PATH) [--timeout MS] COMMAND

  info        protobuf version and device information (read-only)
  ls PATH     list a directory on the device, e.g. /ext (read-only)
  version | help

  --port DEVICE   the Flipper's serial port, e.g. /dev/ttyACM0 (never guessed)
  --socket PATH   a Unix socket carrying the same byte stream (tests use a fake Flipper here)
  --timeout MS    per-read timeout, default 5000";

enum Target {
    Port(PathBuf),
    Socket(PathBuf),
}

struct Usage(String);

struct Cmd {
    target: Option<Target>,
    timeout: Duration,
    words: Vec<String>,
}

fn parse(args: Vec<String>) -> Result<Cmd, Usage> {
    let mut cmd = Cmd { target: None, timeout: Duration::from_millis(5000), words: Vec::new() };
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

fn run(cmd: &Cmd) -> Result<Value, RpcError> {
    let target = cmd.target.as_ref().expect("checked by main");
    let mut s = Session::start(open(target, cmd.timeout)?, cmd.timeout)?;
    match cmd.words[0].as_str() {
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
            let entries: Vec<Value> =
                s.storage_list(path)?.into_iter().map(|e| json!({"name": e.name, "type": e.kind.as_str(), "size": e.size})).collect();
            Ok(json!({"ok": true, "command": "ls", "path": path, "entries": entries}))
        }
        _ => unreachable!("checked by main"),
    }
}

fn main() -> ExitCode {
    let cmd = match parse(std::env::args().skip(1).collect()) {
        Ok(c) => c,
        Err(Usage(m)) => return fail("usage", m, 2),
    };
    let words: Vec<&str> = cmd.words.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["help"] | [] => {
            println!("{HELP}");
            return ExitCode::SUCCESS;
        }
        ["version"] => {
            print(json!({"ok": true, "version": APP_VERSION}));
            return ExitCode::SUCCESS;
        }
        ["info"] | ["ls", _] => {}
        ["ls"] => return fail("usage", "ls needs a device path".into(), 2),
        _ => return fail("usage", format!("unknown command: {}", words.join(" ")), 2),
    }
    if cmd.target.is_none() {
        return fail("usage", "name the Flipper with --port DEVICE or --socket PATH".into(), 2);
    }
    match run(&cmd) {
        Ok(v) => {
            print(v);
            ExitCode::SUCCESS
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
