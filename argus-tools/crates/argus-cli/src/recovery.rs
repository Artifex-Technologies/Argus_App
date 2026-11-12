//! `argus-cli recovery ...`: the Flipper in its DFU bootloader (recovery mode), through
//! `flipper_dfu`: the steps of the C++ app's `recovery.cpp`. One JSON object on stdout.
//!
//! There is no USB transport in argus-cli yet: the only way to reach a bootloader is
//! `--dfu-socket PATH`, a Unix socket speaking `flipper_dfu::line` (the fake bootloader in tests).
//! Every command but `info` changes the device and does nothing without `--yes`; files are read and
//! checked first, before anything is opened.

use std::io::{BufReader, Read};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use flipper_dfu::device::Dfu;
use flipper_dfu::line::LineTransport;
use flipper_dfu::recovery::{self, Bootloader, OptionBytesOutcome, WirelessStatus};
use flipper_dfu::wb55::{OptionBytesFile, Wb55};
use flipper_dfu::{dfuse, wb55};

use super::{fail, print, Cmd};

const USAGE: &str = "argus-cli --dfu-socket PATH recovery COMMAND (the DFU bootloader; JSON on stdout)

  info                     option bytes, FUS state and versions (only reads)

  These change the device and do nothing without --yes (exit 2, nothing opened):
  exit                     leave the bootloader and start the firmware
  boot normal|dfu          set the boot mode in the option bytes (nBOOT0, nSWBOOT0)
  firmware FILE.dfu        erase and write a DfuSe file (checked whole first)
  radio FILE.bin           erase and write a wireless stack image right below SFSA
  fus start|delete|upgrade|start-stack
                           FUS steps (AN5185): start the FUS, delete the wireless stack,
                           install the image written by radio, start the wireless stack
  option-bytes FILE        correct the option bytes from a file like Flipper's ob.data:
                           only its rw fields are written, RDP never
  repair FIRMWARE.dfu OB.data [RADIO.bin]
                           the C++ app's full repair with local files: recovery boot mode,
                           the wireless stack (FUS: delete, write, install, check; a failure
                           there does not stop the repair), the firmware, the option bytes;
                           the chip then boots its firmware. Waits for the chip after each
                           reset (at most 30 s), polls the FUS every --poll-ms (1000)

  --dfu-socket PATH        a Unix socket to a DFU bootloader (tests use the fake); USB
                           recovery is not built into argus-cli";

/// The largest wireless stack image: the whole flash.
const MAX_RADIO: u64 = 1 << 20;

type Device = Dfu<LineTransport<BufReader<UnixStream>, UnixStream>>;

fn read_limited(path: &str, max: u64) -> Result<Vec<u8>, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    if !f.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err(format!("{path} is not a regular file"));
    }
    let mut data = Vec::new();
    f.take(max + 1).read_to_end(&mut data).map_err(|e| format!("{path}: {e}"))?;
    if data.len() as u64 > max {
        return Err(format!("{path} is larger than {max} bytes"));
    }
    Ok(data)
}

/// What a command will do, checked before anything is opened.
enum Plan {
    Info,
    Exit,
    Boot(bool),
    Firmware(Vec<u8>, usize),
    Radio(Vec<u8>),
    Fus(&'static str),
    OptionBytes(Vec<u8>),
    Repair { firmware: Vec<u8>, option_bytes: Vec<u8>, radio: Option<Vec<u8>> },
}

fn plan(words: &[&str]) -> Result<Plan, (String, String, u8)> {
    let usage = |m: &str| ("usage".to_string(), format!("{m}\n\n{USAGE}"), 2);
    let checked = |r: Result<Vec<u8>, String>| r.map_err(|m| ("file".to_string(), m, 1));
    Ok(match words {
        ["info"] => Plan::Info,
        ["exit"] => Plan::Exit,
        ["boot", "normal"] => Plan::Boot(true),
        ["boot", "dfu"] => Plan::Boot(false),
        ["firmware", f] => {
            let data = checked(read_limited(f, dfuse::MAX_FILE as u64))?;
            let parsed = dfuse::parse(&data).map_err(|e| ("file".to_string(), format!("{f}: {e}"), 1))?;
            let elements = parsed.targets.iter().map(|t| t.elements.len()).sum();
            Plan::Firmware(data, elements)
        }
        ["radio", f] => {
            let data = checked(read_limited(f, MAX_RADIO))?;
            if data.is_empty() {
                return Err(("file".into(), format!("{f} is empty"), 1));
            }
            Plan::Radio(data)
        }
        ["fus", "start"] => Plan::Fus("start"),
        ["fus", "delete"] => Plan::Fus("delete"),
        ["fus", "upgrade"] => Plan::Fus("upgrade"),
        ["fus", "start-stack"] => Plan::Fus("start-stack"),
        ["option-bytes", f] => {
            let data = checked(read_limited(f, wb55::MAX_FILE as u64))?;
            OptionBytesFile::parse(&data).map_err(|e| ("file".to_string(), format!("{f}: {e}"), 1))?;
            Plan::OptionBytes(data)
        }
        ["repair", fw, ob, rest @ ..] if rest.len() <= 1 => {
            let firmware = checked(read_limited(fw, dfuse::MAX_FILE as u64))?;
            dfuse::parse(&firmware).map_err(|e| ("file".to_string(), format!("{fw}: {e}"), 1))?;
            let option_bytes = checked(read_limited(ob, wb55::MAX_FILE as u64))?;
            OptionBytesFile::parse(&option_bytes).map_err(|e| ("file".to_string(), format!("{ob}: {e}"), 1))?;
            let radio = match rest.first() {
                Some(r) => {
                    let data = checked(read_limited(r, MAX_RADIO))?;
                    if data.is_empty() {
                        return Err(("file".into(), format!("{r} is empty"), 1));
                    }
                    Some(data)
                }
                None => None,
            };
            Plan::Repair { firmware, option_bytes, radio }
        }
        [] | ["help"] => return Err(usage("recovery needs a command")),
        _ => return Err(usage(&format!("unknown recovery command: {}", words.join(" ")))),
    })
}

/// How long one answer from the bootloader may take.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the chip may take to come back after a reset (the C++ operations' timeout is 30 s).
const REENUMERATION: Duration = Duration::from_secs(30);

fn connect(path: &Path) -> Result<Device, String> {
    let s = UnixStream::connect(path).map_err(|e| format!("{}: {e}", path.display()))?;
    s.set_read_timeout(Some(ANSWER_TIMEOUT)).map_err(|e| e.to_string())?;
    let reader = BufReader::new(s.try_clone().map_err(|e| e.to_string())?);
    Ok(Dfu::new(LineTransport::new(reader, s)))
}

/// The bootloader on `--dfu-socket`, opened again for every step: after a reset it is waited for
/// (bounded) until it answers a status request.
struct SocketBootloader<'a> {
    path: &'a Path,
    poll: Duration,
}

impl Bootloader for SocketBootloader<'_> {
    type T = LineTransport<BufReader<UnixStream>, UnixStream>;

    fn open(&mut self) -> Result<Device, recovery::Error> {
        let deadline = Instant::now() + REENUMERATION;
        loop {
            if let Ok(mut d) = connect(self.path) {
                if d.get_status().is_ok() {
                    return Ok(d);
                }
            }
            if Instant::now() >= deadline {
                let gone =
                    flipper_dfu::device::TransportError(format!("the bootloader did not come back within {} s", REENUMERATION.as_secs()));
                return Err(flipper_dfu::device::Error::Transport { during: "waiting for the bootloader", error: gone }.into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn pause(&mut self) {
        std::thread::sleep(self.poll);
    }
}

fn status_name(s: WirelessStatus) -> &'static str {
    match s {
        WirelessStatus::FusRunning => "fus_running",
        WirelessStatus::WirelessStackRunning => "wireless_stack_running",
        WirelessStatus::ErrorOccurred => "error",
        WirelessStatus::Unhandled => "unhandled",
    }
}

fn fields(v: &wb55::FieldValues) -> Value {
    Value::Object(v.iter().map(|(n, x)| (n.to_string(), json!(x))).collect())
}

fn execute(d: &mut Device, plan: Plan) -> Result<Value, String> {
    let e = |e: &dyn std::fmt::Display| e.to_string();
    Ok(match plan {
        Plan::Info => {
            let ob = d.option_bytes().map_err(|x| e(&x))?;
            let ob: Map<String, Value> = ob.fields().into_iter().map(|(n, v)| (n.to_string(), json!(v))).collect();
            let fus = d.fus_state().map_err(|x| e(&x))?;
            let status = recovery::wireless_status(d).map_err(|x| e(&x))?;
            let versions = d.version_info().ok().map(|v| json!({"fus": v.fus, "wireless": v.wireless}));
            json!({
                "ok": true, "command": "recovery info", "option_bytes": ob,
                "fus": {"status": fus.status, "error": fus.error}, "wireless": status_name(status), "versions": versions,
            })
        }
        Plan::Exit => {
            recovery::exit_recovery(d).map_err(|x| e(&x))?;
            json!({"ok": true, "command": "recovery exit"})
        }
        Plan::Boot(normal) => {
            recovery::set_boot_mode(d, normal).map_err(|x| e(&x))?;
            json!({"ok": true, "command": "recovery boot", "mode": if normal { "normal" } else { "dfu" }})
        }
        Plan::Firmware(data, elements) => {
            recovery::download_firmware(d, &data).map_err(|x| e(&x))?;
            json!({"ok": true, "command": "recovery firmware", "bytes": data.len(), "elements": elements})
        }
        Plan::Radio(image) => {
            let at = recovery::download_wireless_stack(d, &image, None).map_err(|x| e(&x))?;
            json!({"ok": true, "command": "recovery radio", "address": format!("0x{at:08x}"), "bytes": image.len()})
        }
        Plan::Fus(step) => {
            match step {
                "start" => recovery::start_fus(d),
                "delete" => recovery::delete_wireless_stack(d),
                "upgrade" => recovery::upgrade_wireless_stack(d),
                _ => recovery::start_wireless_stack(d),
            }
            .map_err(|x| e(&x))?;
            json!({"ok": true, "command": "recovery fus", "step": step})
        }
        Plan::Repair { .. } => unreachable!("run by repair()"),
        Plan::OptionBytes(data) => match recovery::download_option_bytes(d, &data).map_err(|x| e(&x))? {
            OptionBytesOutcome::Left { differ } => {
                json!({"ok": true, "command": "recovery option-bytes", "written": false, "read_only_differ": fields(&differ)})
            }
            OptionBytesOutcome::Written { fixed, differ } => json!({
                "ok": true, "command": "recovery option-bytes", "written": true, "fixed": fields(&fixed),
                "read_only_differ": fields(&differ),
            }),
        },
    })
}

pub fn run(cmd: &Cmd) -> ExitCode {
    let words: Vec<&str> = cmd.words[1..].iter().map(String::as_str).collect();
    let plan = match plan(&words) {
        Ok(p) => p,
        Err((code, m, status)) => return fail(&code, m, status),
    };
    if !matches!(plan, Plan::Info) && !cmd.yes {
        let what = words.join(" ");
        print(json!({
            "ok": false, "command": format!("recovery {what}"), "checked": true,
            "error": {"code": "confirm", "message": format!("recovery {what} changes the device; run again with --yes")},
        }));
        return ExitCode::from(2);
    }
    let Some(path) = &cmd.dfu_socket else {
        return fail("usage", "recovery needs --dfu-socket PATH; USB recovery is not built into argus-cli".into(), 2);
    };
    if let Plan::Repair { firmware, option_bytes, radio } = &plan {
        let mut b = SocketBootloader { path, poll: cmd.poll };
        return match recovery::full_repair(&mut b, firmware, radio.as_deref(), option_bytes) {
            Ok(r) => {
                let (written, fixed, differ) = match r.option_bytes {
                    OptionBytesOutcome::Left { differ } => (false, Vec::new(), differ),
                    OptionBytesOutcome::Written { fixed, differ } => (true, fixed, differ),
                };
                print(json!({
                    "ok": true, "command": "recovery repair", "firmware_bytes": firmware.len(),
                    "radio": radio.as_ref().map(|image| json!({"bytes": image.len(), "installed": r.radio_error.is_none(), "error": r.radio_error})),
                    "option_bytes_written": written, "fixed": fields(&fixed), "read_only_differ": fields(&differ),
                }));
                ExitCode::SUCCESS
            }
            Err(e) => fail("device", e.to_string(), 1),
        };
    }
    let mut d = match connect(path) {
        Ok(d) => d,
        Err(m) => return fail("device", m, 1),
    };
    match execute(&mut d, plan) {
        Ok(v) => {
            print(v);
            ExitCode::SUCCESS
        }
        Err(m) => fail("device", m, 1),
    }
}
