//! Recovery mode in the app: the repair and leaving the bootloader, through `flipper_dfu::recovery` and its
//! `Bootloader` interface, on a thread of their own.
//!
//! The only bootloader the app can reach is one on a Unix socket (`--dfu-socket`, `flipper_dfu::line::SocketBootloader`;
//! the fake in the tests). A device in recovery mode found on USB is listed, but no USB transport is wired in: asking it
//! to do anything is refused before anything starts. Nothing here flashes a real device.
//!
//! The repair takes its files from the update server (the latest version of the selected channel, as the C++ app's
//! `FullRepairOperation`; `flipper_updates::repair`), or local files (firmware `.dfu`, the option bytes file, optionally
//! the wireless stack image and the resources), as `argus-cli recovery repair` does. Everything is fetched or read and
//! checked before the bootloader is opened. When the chip has started its firmware, the steps on the firmware side
//! (the resources onto the SD card, region data, a restart; `flipper_rpc::assets::finish_install`) run on the app's
//! device worker once the device is back (see `app.rs`).

use std::path::Path;
use std::sync::mpsc::Sender;
use std::time::Duration;

use flipper_dfu::line::SocketBootloader;
use flipper_dfu::recovery::{self, Bootloader};
use flipper_rpc::assets::Bundle;

use crate::device::{read_local, Outcome, Waker};
use crate::discovery::Kind;
use crate::net::Net;

/// The largest files read: a DfuSe file (as the parser's limit), an option bytes file, a wireless stack image.
const MAX_FIRMWARE: u64 = 16 << 20;
const MAX_OPTION_BYTES: u64 = 64 << 10;
const MAX_RADIO: u64 = 1 << 20;
/// The largest resources bundle read (Flipper's is about 10 MiB; argus-cli's limit).
const MAX_RESOURCES: u64 = 64 << 20;

pub struct RepairFiles {
    pub firmware: Vec<u8>,
    pub option_bytes: Vec<u8>,
    pub radio: Option<Vec<u8>>,
    /// The resources for the steps on the firmware side; without them the repair ends when the chip starts.
    pub resources: Option<Bundle>,
}

fn check(firmware: &[u8], option_bytes: &[u8]) -> Result<(), String> {
    flipper_dfu::dfuse::parse(firmware).map_err(|e| format!("The firmware file is refused: {e}"))?;
    flipper_dfu::wb55::OptionBytesFile::parse(option_bytes).map_err(|e| format!("The option bytes file is refused: {e:?}"))?;
    Ok(())
}

fn bundle(data: &[u8]) -> Result<Bundle, String> {
    flipper_rpc::assets::read_bundle(data, &flipper_rpc::assets::BundleLimits::default())
        .map_err(|e| format!("The resources are refused: {}", e.0))
}

impl RepairFiles {
    /// Reads and checks the files before anything is opened: a damaged file never reaches the device.
    pub fn read(firmware: &Path, option_bytes: &Path, radio: Option<&Path>, resources: Option<&Path>) -> Result<RepairFiles, String> {
        let firmware = read_local(firmware, MAX_FIRMWARE)?;
        let option_bytes = read_local(option_bytes, MAX_OPTION_BYTES)?;
        check(&firmware, &option_bytes)?;
        let radio = radio.map(|p| read_local(p, MAX_RADIO)).transpose()?;
        if radio.as_ref().is_some_and(Vec::is_empty) {
            return Err("The wireless stack image is empty.".into());
        }
        let resources = resources.map(|p| read_local(p, MAX_RESOURCES).and_then(|d| bundle(&d))).transpose()?;
        Ok(RepairFiles { firmware, option_bytes, radio, resources })
    }

    /// The latest version of `channel` on the update server (`FirmwareHelper`), fetched and checked.
    fn fetch(net: &Net, channel: &str) -> Result<(String, RepairFiles), String> {
        let f = net.repair_files(channel)?;
        check(&f.firmware, &f.option_bytes)?;
        let resources = Some(bundle(&f.resources)?);
        Ok((f.version, RepairFiles { firmware: f.firmware, option_bytes: f.option_bytes, radio: Some(f.radio.data), resources }))
    }
}

pub enum Job {
    Repair(RepairFiles),
    /// The repair with the files of the latest version of a channel on the update server.
    FromServer {
        net: Net,
        channel: String,
    },
    /// Leave the bootloader and start the firmware.
    Exit,
    /// The developer tab: a wireless stack image (below SFSA) or a FUS image (at its address) through the FUS, between
    /// the boot modes (`flipper_dfu::recovery::core2_update`).
    Core2 {
        image: Vec<u8>,
        fus: Option<u32>,
    },
}

/// A wireless stack or FUS image for the developer tab: a regular file, not empty, at most the flash's size.
pub fn read_image(path: &Path) -> Result<Vec<u8>, String> {
    let data = read_local(path, MAX_RADIO)?;
    if data.is_empty() {
        return Err(format!("{} is empty.", path.display()));
    }
    Ok(data)
}

/// The end of the bootloader part.
pub struct Done {
    pub outcome: Outcome,
    /// The resources for the steps on the firmware side, once the device is back.
    pub finish: Option<Bundle>,
}

/// Why a device cannot be reached for recovery, if it cannot.
pub fn unreachable(kind: &Kind) -> Option<String> {
    match kind {
        Kind::RecoverySocket(_) => None,
        Kind::RecoveryUsb(_) => Some("This build cannot reach a device in recovery mode over USB yet; nothing was sent.".into()),
        _ => Some("The device is not in recovery mode.".into()),
    }
}

fn repair<B: Bootloader>(b: &mut B, f: RepairFiles, version: Option<String>) -> Result<Done, String> {
    let r = recovery::full_repair(b, &f.firmware, f.radio.as_deref(), &f.option_bytes).map_err(|e| e.to_string())?;
    let mut summary = "The firmware is written and the chip starts it".to_string();
    if let Some(v) = version {
        summary.push_str(&format!(" (version {v} from the update server)"));
    }
    Ok(Done {
        outcome: Outcome {
            summary,
            // the partial success both CLIs report (exit 3): the chip runs, without its wireless stack
            warning: r.radio_error.map(|e| format!("repaired, but the radio stack was not installed: {e}")),
        },
        finish: f.resources,
    })
}

fn run<B: Bootloader>(b: &mut B, job: Job) -> Result<Done, String> {
    match job {
        Job::Repair(f) => repair(b, f, None),
        Job::FromServer { net, channel } => {
            // every file fetched and checked before the bootloader is opened
            let (version, f) = RepairFiles::fetch(&net, &channel)?;
            repair(b, f, Some(version))
        }
        Job::Core2 { image, fus } => {
            recovery::core2_update(b, &image, fus).map_err(|e| e.to_string())?;
            let what = if fus.is_some() { "The FUS is installed" } else { "The wireless stack is installed" };
            Ok(Done { outcome: Outcome { summary: format!("{what} and the chip starts its firmware"), warning: None }, finish: None })
        }
        Job::Exit => {
            let mut d = b.open().map_err(|e| e.to_string())?;
            recovery::exit_recovery(&mut d).map_err(|e| e.to_string())?;
            Ok(Done { outcome: Outcome { summary: "The chip left recovery mode".into(), warning: None }, finish: None })
        }
    }
}

/// Runs `job` on the bootloader `kind` names, on a new thread; the result comes back on `tx`.
pub fn start(kind: Kind, poll: Duration, job: Job, tx: Sender<Result<Done, String>>, waker: Waker) -> Result<(), String> {
    if let Some(why) = unreachable(&kind) {
        return Err(why);
    }
    let Kind::RecoverySocket(path) = kind else { unreachable!("checked above") };
    std::thread::Builder::new()
        .name("argus-recovery".into())
        .spawn(move || {
            let mut b = SocketBootloader { path, poll };
            let _ = tx.send(run(&mut b, job));
            waker();
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}
