//! `argus-cli recovery ...`: the Flipper in its DFU bootloader (recovery mode), through
//! `flipper_dfu`: the steps of the C++ app's `recovery.cpp`. One JSON object on stdout.
//!
//! There is no USB transport in argus-cli yet: the only way to reach a bootloader is
//! `--dfu-socket PATH`, a Unix socket speaking `flipper_dfu::line` (the fake bootloader in tests).
//! Every command but `info` changes the device and does nothing without `--yes`; files are read and
//! checked first, before anything is opened.

use std::io::Read;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use flipper_dfu::line::{connect, SocketBootloader, REENUMERATION};
use flipper_dfu::recovery::{self, OptionBytesOutcome, WirelessStatus};
use flipper_dfu::wb55::{OptionBytesFile, Wb55};
use flipper_dfu::{dfuse, wb55};

use flipper_rpc::assets::{self, Bundle};
use flipper_rpc::session::{RpcError, Session};

use flipper_updates::region::RegionData;

use super::updates::{client, fetch_region_data, get_directory, region_json, server};
use super::{fail, open, print, Cmd, GONE_WAIT};

/// A failure on the firmware side: the device's, or fetching the region data.
struct Failure {
    code: String,
    message: String,
}

impl From<RpcError> for Failure {
    fn from(e: RpcError) -> Failure {
        Failure { code: e.code().to_string(), message: e.to_string() }
    }
}

impl From<(String, String)> for Failure {
    fn from((code, message): (String, String)) -> Failure {
        Failure { code, message: format!("region data: {message}") }
    }
}

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
  install-radio RADIO.bin [--port DEVICE]
                           the developer tab's wireless stack install (the C++ WirelessStackUpdate-
                           Operation): with --port, the running firmware there is rebooted into its
                           bootloader first; then recovery boot mode, the FUS steps with the image
                           below SFSA, then the OS boot mode (the chip starts its firmware)
  install-fus FUS.bin [ADDRESS] [--port DEVICE]
                           the same with a FUS image written at ADDRESS (hex; default 080EC000,
                           ST's FUS address on Flipper's chip). It invalidates the chip's keys
  repair FIRMWARE.dfu OB.data [RADIO.bin] [--resources RESOURCES.tgz --port DEVICE]
                           the C++ app's full repair with local files: recovery boot mode,
                           the wireless stack (FUS: delete, write, install, check; a failure
                           there does not stop the repair), the firmware, the option bytes;
                           the chip then boots its firmware. Waits for the chip after each
                           reset (at most 30 s), polls the FUS every --poll-ms (1000).
                           With --resources: then, on the firmware at --port, what finish does
  repair-from-server CHANNEL [--port DEVICE]
                           the C++ app's full repair as its GUI and qFlipper-cli run it: the
                           files of CHANNEL's latest version on --update-server (full_dfu f7,
                           the wireless stack from core2_firmware_tgz, ob.data from
                           scripts_tgz, resources_tgz), each checked against the directory's
                           SHA-256 and read whole before anything opens (the wireless stack
                           image must match its manifest's SHA-256); then repair; with --port
                           also finish with those resources
  legacy-update FIRMWARE.dfu OB.data [RADIO.bin] --resources R.tgz --backup FILE
                           the C++ app's legacy update (a device without SD card or with old
                           firmware), with --port and --dfu-socket: /int backed up to FILE
                           (which must not exist; it stays), a reboot into the bootloader;
                           with RADIO.bin the repair's steps, else the firmware and a restart
                           into it; then on the firmware what finish does, with /int restored
                           from FILE before the restart. Give RADIO.bin only when the device's
                           wireless stack is older than the image (the C++ app reads both
                           versions; argus-cli has only the image)
  legacy-update-from-server CHANNEL --backup FILE
                           the same as the C++ app's GUI runs it: the files of CHANNEL's latest
                           version on --update-server (as repair-from-server), the wireless stack
                           installed only when the device's (read on --port) is missing, of another
                           type or older by number than the image's (FirmwareHelper::hasRadioUpdate)
  finish RESOURCES.tgz     on the firmware at --port (no --dfu-socket): the C++ app's steps
                           after a repair or update: the resources onto /ext (compared with
                           /ext/Manifest; only what changed is removed and written), region
                           data (the development region needs none; for any other the region
                           bundle is fetched from --update-server before the resources (for
                           legacy-update before the backup), and its bands for the country the server saw,
                           else --country, else the default ones, go to /int/.region_data),
                           and a restart

  --dfu-socket PATH        a Unix socket to a DFU bootloader (tests use the fake); USB
                           recovery is not built into argus-cli
  --port DEVICE            the Flipper's serial port once its firmware runs (never guessed)
  --update-server URL      where the region bundle comes from (default Flipper's; https only)
  --country CC             the country for the region data when the server names none

  Exit status: 0 done; 1 failed; 2 usage, or not confirmed with --yes; 3 repaired, but the radio
  (wireless) stack was not installed (\"ok\": false, \"partial\": true, the reason in \"error\")";

/// The exit status of a repair that did everything but install the wireless stack (the C++ CLI's too).
const PARTIAL: u8 = 3;

/// The largest wireless stack image: the whole flash.
const MAX_RADIO: u64 = 1 << 20;

type Device = flipper_dfu::line::SocketDevice;

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
    Repair {
        firmware: Vec<u8>,
        option_bytes: Vec<u8>,
        radio: Option<Vec<u8>>,
    },
    Legacy {
        firmware: Vec<u8>,
        option_bytes: Vec<u8>,
        radio: Option<Vec<u8>>,
    },
    Finish,
    /// The files come from the update server (fetched in `run`, before anything opens).
    FromServer(String),
    /// The legacy update with the update server's files.
    LegacyFromServer(String),
    /// A wireless stack (None) or FUS image (its address) through the FUS, between the boot modes.
    Core2(Vec<u8>, Option<u32>),
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
        ["repair-from-server", channel] => Plan::FromServer(channel.to_string()),
        ["legacy-update-from-server", channel] => Plan::LegacyFromServer(channel.to_string()),
        ["install-radio" | "install-fus", f, rest @ ..] if rest.len() <= usize::from(words[0] == "install-fus") => {
            let data = checked(read_limited(f, MAX_RADIO))?;
            if data.is_empty() {
                return Err(("file".into(), format!("{f} is empty"), 1));
            }
            let address = match (words[0], rest.first()) {
                ("install-radio", _) => None,
                (_, None) => Some(recovery::FUS_ADDRESS),
                (_, Some(a)) => Some(
                    u32::from_str_radix(a.trim_start_matches("0x"), 16)
                        .map_err(|_| ("usage".to_string(), format!("not a hexadecimal address: {a}"), 2))?,
                ),
            };
            Plan::Core2(data, address)
        }
        [] | ["help"] => return Err(usage("recovery needs a command")),
        _ => return Err(usage(&format!("unknown recovery command: {}", words.join(" ")))),
    })
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
        Plan::Repair { .. } | Plan::Legacy { .. } | Plan::Finish | Plan::FromServer(_) | Plan::LegacyFromServer(_) | Plan::Core2(..) => {
            unreachable!("run by run()")
        }
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
    let mut plan = match plan(&words) {
        Ok(p) => p,
        Err((code, m, status)) => return fail(&code, m, status),
    };
    if matches!(plan, Plan::FromServer(_) | Plan::LegacyFromServer(_)) && cmd.resources.is_some() {
        return fail("usage", format!("{} takes its resources from the server, not --resources", words[0]), 2);
    }
    if let Plan::LegacyFromServer(_) = &plan {
        if cmd.target.is_none() || cmd.dfu_socket.is_none() {
            return fail("usage", "legacy-update-from-server needs --port DEVICE and --dfu-socket PATH".into(), 2);
        }
        if cmd.backup.is_none() {
            return fail("usage", "legacy-update needs --backup FILE (where /int is kept)".into(), 2);
        }
    }
    if let Plan::FromServer(_) | Plan::LegacyFromServer(_) = &plan {
        if !cmd.yes {
            print(json!({
                "ok": false, "command": format!("recovery {}", words.join(" ")), "checked": true,
                "error": {"code": "confirm", "message": format!("recovery {} changes the device; run again with --yes", words.join(" "))},
            }));
            return ExitCode::from(2);
        }
    }
    // the server's files, fetched and checked before anything opens; the resources go to finish when --port is given
    let mut fetched: Option<Bundle> = None;
    let mut server_version: Option<String> = None;
    let mut legacy_radio: Option<flipper_updates::repair::RadioImage> = None;
    if let Plan::FromServer(channel) | Plan::LegacyFromServer(channel) = &plan {
        let files = match client().and_then(|c| {
            let dir = get_directory(&c, server(cmd))?;
            flipper_updates::repair::fetch(&c, &dir, channel).map_err(|m| ("update_server".to_string(), m))
        }) {
            Ok(f) => f,
            Err((code, m)) => return fail(&code, m, 1),
        };
        if let Err(e) = dfuse::parse(&files.firmware) {
            return fail("file", format!("the firmware from the server: {e}"), 1);
        }
        if let Err(e) = OptionBytesFile::parse(&files.option_bytes) {
            return fail("file", format!("the option bytes from the server: {e}"), 1);
        }
        let bundle = match assets::read_bundle(&files.resources[..], &assets::BundleLimits::default()) {
            Ok(b) => b,
            Err(e) => return fail("file", format!("the resources from the server: {}", e.0), 1),
        };
        if cmd.target.is_some() {
            fetched = Some(bundle);
        }
        server_version = Some(files.version.clone());
        plan = if let Plan::LegacyFromServer(_) = plan {
            // the wireless stack is decided once the device's own is read (FirmwareHelper::hasRadioUpdate)
            legacy_radio = Some(files.radio);
            Plan::Legacy { firmware: files.firmware, option_bytes: files.option_bytes, radio: None }
        } else {
            Plan::Repair { firmware: files.firmware, option_bytes: files.option_bytes, radio: Some(files.radio.data) }
        };
    }
    // the resources (for finish, or a repair that goes on to the firmware) are checked before anything opens
    let bundle = match (&plan, &cmd.resources) {
        (Plan::Finish, Some(_)) => return fail("usage", "finish takes RESOURCES.tgz as its argument, not --resources".into(), 2),
        (Plan::Finish, None) => Some(read_bundle(Path::new(&words[1]))),
        (Plan::Repair { .. } | Plan::Legacy { .. }, Some(r)) => Some(read_bundle(r)),
        (Plan::Legacy { .. }, None) if legacy_radio.is_none() => {
            return fail("usage", "legacy-update needs --resources RESOURCES.tgz".into(), 2)
        }
        (_, Some(_)) => return fail("usage", "--resources goes with recovery repair".into(), 2),
        (_, None) => None,
    };
    let bundle = match bundle {
        Some(Ok(b)) => Some(b),
        Some(Err(m)) => return fail("file", m, 1),
        None => fetched.take(),
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
        return match finish(cmd, b, None, None) {
            Ok(v) => {
                print(json!({"ok": true, "command": "recovery finish", "firmware": v}));
                ExitCode::SUCCESS
            }
            Err(e) => fail(&e.code, e.message, 1),
        };
    }
    let Some(path) = &cmd.dfu_socket else {
        return fail("usage", "recovery needs --dfu-socket PATH; USB recovery is not built into argus-cli".into(), 2);
    };
    if let (Plan::Legacy { firmware, option_bytes, radio }, Some(bundle)) = (&plan, &bundle) {
        let mut b = SocketBootloader { path: path.to_path_buf(), poll: cmd.poll };
        // from the server: the wireless stack only when the device's is missing, of another type or older
        let mut decided = None;
        if let Some(image) = &legacy_radio {
            let (version, stack_type) = match firmware_session(cmd).map_err(Failure::from).and_then(|mut s| device_radio(&mut s)) {
                Ok(v) => v,
                Err(e) => return fail(&e.code, format!("before the update, on the firmware: {}", e.message), 1),
            };
            let update = flipper_updates::repair::has_radio_update(&version, stack_type, image);
            decided = Some((
                update.then_some(image.data.as_slice()),
                json!({
                    "device": version, "device_type": stack_type, "image": image.version, "image_type": image.stack_type, "install": update,
                }),
            ));
        }
        let radio = match &decided {
            Some((r, _)) => *r,
            None => radio.as_deref(),
        };
        let (code, mut out) = legacy_update(cmd, &mut b, firmware, option_bytes, radio, bundle);
        if let Some((_, v)) = decided {
            out["command"] = json!("recovery legacy-update-from-server");
            out["radio_update"] = v;
            out["server_version"] = json!(server_version);
        }
        print(out);
        return ExitCode::from(code);
    }
    if let Plan::Core2(image, address) = &plan {
        let what = if address.is_some() { "recovery install-fus" } else { "recovery install-radio" };
        // a running firmware first reboots into its bootloader (StartRecoveryOperation)
        let mut rebooted = false;
        if cmd.target.is_some() {
            let r = firmware_session(cmd).and_then(|mut s| {
                s.system_reboot(flipper_rpc::write::RebootMode::Dfu)?;
                s.wait_until_gone(GONE_WAIT);
                Ok(())
            });
            if let Err(e) = r {
                return fail(e.code(), format!("rebooting into the bootloader: {e}"), 1);
            }
            rebooted = true;
        }
        let mut b = SocketBootloader { path: path.to_path_buf(), poll: cmd.poll };
        return match recovery::core2_update(&mut b, image, *address) {
            Ok(()) => {
                print(json!({
                    "ok": true, "command": what, "bytes": image.len(), "address": address.map(|a| format!("0x{a:08X}")),
                    "rebooted_into_bootloader": rebooted,
                }));
                ExitCode::SUCCESS
            }
            Err(e) => fail("device", e.to_string(), 1),
        };
    }
    if let Plan::Repair { firmware, option_bytes, radio } = &plan {
        let mut b = SocketBootloader { path: path.to_path_buf(), poll: cmd.poll };
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
                if let Some(v) = &server_version {
                    out["server_version"] = json!(v);
                }
                if let Some(b) = &bundle {
                    match finish(cmd, b, None, None) {
                        Ok(v) => out["firmware"] = v,
                        Err(e) => {
                            // the bootloader part is done; say so with the failure
                            out["ok"] = json!(false);
                            out["error"] = json!({"code": e.code, "message": format!("after the repair, on the firmware: {}", e.message)});
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
/// `--port`, reads its region, makes its region data (unless `region` brings it, or it is the development
/// region) before anything changes, installs the resources and the region data, and restarts it.
fn finish(
    cmd: &Cmd,
    bundle: &Bundle,
    restore: Option<&flipper_rpc::restore::BackupArchive>,
    region: Option<RegionData>,
) -> Result<Value, Failure> {
    let mut s = firmware_session(cmd)?;
    let builtin = read_region(&mut s)?;
    let region = match (builtin, region) {
        (0, _) => None,
        (_, Some(d)) => Some(d),
        (_, None) => Some(fetch_region_data(cmd)?),
    };
    let (r, restored) = s.finish_install(bundle, builtin, region.as_ref().map(|d| d.bytes.as_slice()), restore)?;
    let gone = s.wait_until_gone(GONE_WAIT);
    let region_data = match &region {
        None => json!("not needed (development region)"),
        Some(d) => {
            let mut v = region_json(d);
            v["path"] = json!(assets::REGION_DATA_PATH);
            v
        }
    };
    Ok(json!({
        "region": builtin, "region_data": region_data,
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

/// The device's wireless stack as the C++ `DeviceInfo` has it: "major.minor.sub" and its type, or "" when the radio is
/// not alive.
fn device_radio(s: &mut Firmware) -> Result<(String, i32), Failure> {
    let devinfo = s.property_get("devinfo")?;
    let get = |k: &str| devinfo.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
    if get("radio.alive") != "true" {
        return Ok((String::new(), 0));
    }
    let version = format!("{}.{}.{}", get("radio.stack.major"), get("radio.stack.minor"), get("radio.stack.sub"));
    Ok((version, get("radio.stack.type").parse().unwrap_or(0)))
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
    // on the firmware: the region first (its data is fetched now, so nothing changes when that fails), the
    // backup, and the reboot into the bootloader
    let mut region = None;
    let step1 = (|| -> Result<Value, Failure> {
        let mut s = firmware_session(cmd)?;
        if read_region(&mut s)? != 0 {
            region = Some(fetch_region_data(cmd)?);
        }
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
        Err(e) => return failed("before the update, on the firmware", &e.code, e.message),
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
    match finish(cmd, bundle, Some(&archive), region) {
        Ok(v) => out["firmware"] = v,
        Err(e) => {
            out["ok"] = json!(false);
            out["error"] = json!({"code": e.code, "message": format!("after the update, on the firmware: {}; /int is backed up in {}", e.message, backup.display())});
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
