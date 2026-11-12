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

use flipper_rpc::assets::{self, Bundle};
use flipper_rpc::session::{RpcError, Session};

use super::{fail, open, print, Cmd, GONE_WAIT};

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
  repair FIRMWARE.dfu OB.data [RADIO.bin] [--resources RESOURCES.tgz --port DEVICE]
                           the C++ app's full repair with local files: recovery boot mode,
                           the wireless stack (FUS: delete, write, install, check; a failure
                           there does not stop the repair), the firmware, the option bytes;
                           the chip then boots its firmware. Waits for the chip after each
                           reset (at most 30 s), polls the FUS every --poll-ms (1000).
                           With --resources: then, on the firmware at --port, what finish does
  legacy-update FIRMWARE.dfu OB.data [RADIO.bin] --resources R.tgz --backup FILE
                           the C++ app's legacy update (a device without SD card or with old
                           firmware), with --port and --dfu-socket: /int backed up to FILE
                           (which must not exist; it stays), a reboot into the bootloader;
                           with RADIO.bin the repair's steps, else the firmware and a restart
                           into it; then on the firmware what finish does, with /int restored
                           from FILE before the restart. Give RADIO.bin only when the device's
                           wireless stack is older than the image (the C++ app reads both
                           versions; argus-cli has only the image)
  finish RESOURCES.tgz     on the firmware at --port (no --dfu-socket): the C++ app's steps
                           after a repair or update: the resources onto /ext (compared with
                           /ext/Manifest; only what changed is removed and written), region
                           data (the development region needs none; any other is refused,
                           as its data comes from Flipper's server), and a restart

  --dfu-socket PATH        a Unix socket to a DFU bootloader (tests use the fake); USB
                           recovery is not built into argus-cli
  --port DEVICE            the Flipper's serial port once its firmware runs (never guessed)

  Exit status: 0 done; 1 failed; 2 usage, or not confirmed with --yes; 3 repaired, but the radio
  (wireless) stack was not installed (\"ok\": false, \"partial\": true, the reason in \"error\")";

/// The exit status of a repair that did everything but install the wireless stack (the C++ CLI's too).
const PARTIAL: u8 = 3;

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
    Legacy { firmware: Vec<u8>, option_bytes: Vec<u8>, radio: Option<Vec<u8>> },
    Finish,
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
        ["repair" | "legacy-update", fw, ob, rest @ ..] if rest.len() <= 1 => {
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
            if words[0] == "legacy-update" {
                Plan::Legacy { firmware, option_bytes, radio }
            } else {
                Plan::Repair { firmware, option_bytes, radio }
            }
        }
        ["finish", _] => Plan::Finish,
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
/// (bounded) until its socket is back, as the C++ app waits for the device to re-enumerate. Opening
/// sends nothing (the C++ app's `beginTransaction` sends nothing either).
struct SocketBootloader<'a> {
    path: &'a Path,
    poll: Duration,
}

impl Bootloader for SocketBootloader<'_> {
    type T = LineTransport<BufReader<UnixStream>, UnixStream>;

    fn open(&mut self) -> Result<Device, recovery::Error> {
        let deadline = Instant::now() + REENUMERATION;
        loop {
            if let Ok(d) = connect(self.path) {
                return Ok(d);
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
        Plan::Repair { .. } | Plan::Legacy { .. } | Plan::Finish => unreachable!("run by run()"),
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
    // the resources (for finish, or a repair that goes on to the firmware) are checked before anything opens
    let bundle = match (&plan, &cmd.resources) {
        (Plan::Finish, Some(_)) => return fail("usage", "finish takes RESOURCES.tgz as its argument, not --resources".into(), 2),
        (Plan::Finish, None) => Some(read_bundle(Path::new(&words[1]))),
        (Plan::Repair { .. } | Plan::Legacy { .. }, Some(r)) => Some(read_bundle(r)),
        (Plan::Legacy { .. }, None) => return fail("usage", "legacy-update needs --resources RESOURCES.tgz".into(), 2),
        (_, Some(_)) => return fail("usage", "--resources goes with recovery repair".into(), 2),
        (_, None) => None,
    };
    let bundle = match bundle {
        Some(Ok(b)) => Some(b),
        Some(Err(m)) => return fail("file", m, 1),
        None => None,
    };
    if !matches!(plan, Plan::Info) && !cmd.yes {
        let what = words.join(" ");
        print(json!({
            "ok": false, "command": format!("recovery {what}"), "checked": true,
            "error": {"code": "confirm", "message": format!("recovery {what} changes the device; run again with --yes")},
        }));
        return ExitCode::from(2);
    }
    if let Plan::Legacy { .. } = &plan {
        match &cmd.backup {
            None => return fail("usage", "legacy-update needs --backup FILE (where /int is kept)".into(), 2),
            Some(f) if f.exists() => return fail("file", format!("{} exists; the backup never replaces a file", f.display()), 1),
            Some(_) => {}
        }
    } else if cmd.backup.is_some() {
        return fail("usage", "--backup goes with recovery legacy-update".into(), 2);
    }
    if bundle.is_some() && cmd.target.is_none() {
        return fail("usage", "the steps on the firmware need --port DEVICE (the Flipper once its firmware runs)".into(), 2);
    }
    if let (Plan::Finish, Some(b)) = (&plan, &bundle) {
        return match finish(cmd, b, None) {
            Ok(v) => {
                print(json!({"ok": true, "command": "recovery finish", "firmware": v}));
                ExitCode::SUCCESS
            }
            Err(e) => fail(e.code(), e.to_string(), 1),
        };
    }
    let Some(path) = &cmd.dfu_socket else {
        return fail("usage", "recovery needs --dfu-socket PATH; USB recovery is not built into argus-cli".into(), 2);
    };
    if let (Plan::Legacy { firmware, option_bytes, radio }, Some(bundle)) = (&plan, &bundle) {
        let mut b = SocketBootloader { path, poll: cmd.poll };
        let (code, out) = legacy_update(cmd, &mut b, firmware, option_bytes, radio.as_deref(), bundle);
        print(out);
        return ExitCode::from(code);
    }
    if let Plan::Repair { firmware, option_bytes, radio } = &plan {
        let mut b = SocketBootloader { path, poll: cmd.poll };
        return match recovery::full_repair(&mut b, firmware, radio.as_deref(), option_bytes) {
            Ok(r) => {
                let (written, fixed, differ) = match r.option_bytes {
                    OptionBytesOutcome::Left { differ } => (false, Vec::new(), differ),
                    OptionBytesOutcome::Written { fixed, differ } => (true, fixed, differ),
                };
                let mut out = json!({
                    "ok": true, "command": "recovery repair", "firmware_bytes": firmware.len(),
                    "radio": radio.as_ref().map(|image| json!({"bytes": image.len(), "installed": r.radio_error.is_none(), "error": r.radio_error})),
                    "option_bytes_written": written, "fixed": fields(&fixed), "read_only_differ": fields(&differ),
                });
                if let Some(b) = &bundle {
                    match finish(cmd, b, None) {
                        Ok(v) => out["firmware"] = v,
                        Err(e) => {
                            // the bootloader part is done; say so with the failure
                            out["ok"] = json!(false);
                            out["error"] = json!({"code": e.code(), "message": format!("after the repair, on the firmware: {e}")});
                            print(out);
                            return ExitCode::from(1);
                        }
                    }
                }
                // the wireless stack step failed and the rest went on: not plain success (exit 3)
                if let Some(why) = &r.radio_error {
                    out["ok"] = json!(false);
                    out["partial"] = json!(true);
                    out["error"] = json!({"code": "radio_not_installed", "message": format!("repaired, but the radio stack was not installed: {why}")});
                    print(out);
                    return ExitCode::from(PARTIAL);
                }
                print(out);
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

/// The largest resources bundle read (Flipper's is about 10 MiB).
const MAX_BUNDLE: u64 = 64 << 20;

fn read_bundle(path: &Path) -> Result<Bundle, String> {
    let data = read_limited(&path.to_string_lossy(), MAX_BUNDLE)?;
    assets::read_bundle(&data[..], &assets::BundleLimits::default()).map_err(|e| format!("{}: {}", path.display(), e.0))
}

/// The steps on the firmware: waits (at most 30 s, as for the bootloader) until the Flipper answers on
/// `--port`, reads its region, installs the resources, and restarts it.
fn finish(cmd: &Cmd, bundle: &Bundle, restore: Option<&flipper_rpc::restore::BackupArchive>) -> Result<Value, RpcError> {
    let mut s = firmware_session(cmd)?;
    let region = read_region(&mut s)?;
    let (r, restored) = s.finish_install(bundle, region, restore)?;
    let gone = s.wait_until_gone(GONE_WAIT);
    Ok(json!({
        "region": region, "region_data": "not needed (development region)",
        "assets": {"card": r.card, "device_manifest": r.device_manifest, "removed": r.removed, "dirs_made": r.dirs_made,
                   "files_written": r.files_written, "bytes": r.bytes},
        "restored": restored.map(|x| json!({"files": x.files, "bytes": x.bytes, "dirs_made": x.dirs_made, "dirs_kept": x.dirs_kept})),
        "restarted": true, "gone": gone,
    }))
}

type Firmware = Session<Box<dyn flipper_rpc::transport::Transport>>;

/// The Flipper on `--port`, once it answers (at most 30 s, as for the bootloader).
fn firmware_session(cmd: &Cmd) -> Result<Firmware, RpcError> {
    let target = cmd.target.as_ref().expect("checked by run()");
    let deadline = Instant::now() + REENUMERATION;
    loop {
        match open(target, cmd.timeout).and_then(|io| Session::start(io, cmd.timeout)) {
            Ok(s) => return Ok(s),
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn read_region(s: &mut Firmware) -> Result<u32, RpcError> {
    let devinfo = s.property_get("devinfo")?;
    assets::builtin_region(&devinfo).ok_or_else(|| RpcError::Unexpected("devinfo has no hardware.region.builtin".into()))
}

/// `LegacyUpdateOperation` with local files: (exit status, JSON).
fn legacy_update(
    cmd: &Cmd,
    b: &mut SocketBootloader,
    firmware: &[u8],
    option_bytes: &[u8],
    radio: Option<&[u8]>,
    bundle: &Bundle,
) -> (u8, Value) {
    let backup = cmd.backup.as_ref().expect("checked by run()");
    let failed = |stage: &str, code: &str, m: String| {
        (1, json!({"ok": false, "command": "recovery legacy-update", "error": {"code": code, "message": format!("{stage}: {m}")}}))
    };
    // on the firmware: the region first (nothing changes for a region argus-cli cannot provision), the backup,
    // and the reboot into the bootloader
    let step1 = (|| -> Result<Value, RpcError> {
        let mut s = firmware_session(cmd)?;
        assets::check_region(read_region(&mut s)?)?;
        let mtime = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let r = s.backup(backup, &flipper_rpc::transfer::Limits::backup(), mtime)?;
        s.system_reboot(flipper_rpc::write::RebootMode::Dfu)?;
        let gone = s.wait_until_gone(GONE_WAIT);
        Ok(
            json!({"file": backup.display().to_string(), "files": r.files, "dirs": r.dirs, "bytes": r.bytes, "refused": r.refused, "gone": gone}),
        )
    })();
    let backed_up = match step1 {
        Ok(v) => v,
        Err(e) => return failed("before the update, on the firmware", e.code(), e.to_string()),
    };
    let archive = match std::fs::File::open(backup)
        .map_err(|e| e.to_string())
        .and_then(|f| flipper_rpc::restore::read_backup(f, &flipper_rpc::restore::ArchiveLimits::default()).map_err(|e| e.0))
    {
        Ok(a) => a,
        Err(m) => return failed("reading the backup back", "file", m),
    };
    // in the bootloader
    let (radio_error, written) = match radio {
        Some(image) => match recovery::full_repair(b, firmware, Some(image), option_bytes) {
            Ok(r) => (r.radio_error, matches!(r.option_bytes, OptionBytesOutcome::Written { .. })),
            Err(e) => return failed("in the bootloader", "device", e.to_string()),
        },
        None => match recovery::firmware_and_leave(b, firmware) {
            Ok(()) => (None, false),
            Err(e) => return failed("in the bootloader", "device", e.to_string()),
        },
    };
    let mut out = json!({
        "ok": true, "command": "recovery legacy-update", "backup": backed_up, "firmware_bytes": firmware.len(),
        "radio": radio.map(|image| json!({"bytes": image.len(), "installed": radio_error.is_none(), "error": radio_error})),
        "option_bytes_written": written,
    });
    // on the firmware again
    match finish(cmd, bundle, Some(&archive)) {
        Ok(v) => out["firmware"] = v,
        Err(e) => {
            out["ok"] = json!(false);
            out["error"] = json!({"code": e.code(), "message": format!("after the update, on the firmware: {e}; /int is backed up in {}", backup.display())});
            return (1, out);
        }
    }
    if let Some(why) = &radio_error {
        out["ok"] = json!(false);
        out["partial"] = json!(true);
        out["error"] = json!({"code": "radio_not_installed", "message": format!("updated, but the radio stack was not installed: {why}")});
        return (PARTIAL, out);
    }
    (0, out)
}
