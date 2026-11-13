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
//!
//! Two operations start on the running firmware and cross into the bootloader, as the C++ ones do: the developer tab's
//! installs (`WirelessStackUpdateOperation`, `FUSUpdateOperation`) and the legacy update (`LegacyUpdateOperation`, a
//! device without SD card or with an old protobuf). The app's device worker reboots the firmware into its bootloader
//! (after backing up `/int` for the legacy update), the app waits for the bootloader to be listed, and the job here
//! runs on it. Only a firmware on a Unix socket is ever rebooted so: a Flipper on USB would come back as a USB
//! bootloader this build cannot reach, and would be left in recovery mode.

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

/// The legacy update's files from the update server, checked, with the wireless stack only when the device needs it.
pub struct LegacyFiles {
    pub version: String,
    pub firmware: Vec<u8>,
    pub option_bytes: Vec<u8>,
    /// `Some` only when the device's stack is missing, of another type or older (`FirmwareHelper::hasRadioUpdate`).
    pub radio: Option<Vec<u8>>,
    /// What was decided about the wireless stack, for the log.
    pub radio_note: String,
    pub resources: Bundle,
    /// The region data, made before anything changes (`None`: the development region needs none).
    pub region_data: Option<Vec<u8>>,
}

impl LegacyFiles {
    /// The latest version of `channel`, for a device whose wireless stack is `radio_version` of `stack_type` and whose
    /// built-in region is `region`. Everything is fetched and checked before the device is touched.
    pub fn fetch(net: &Net, channel: &str, radio_version: &str, stack_type: i32, region: u32) -> Result<LegacyFiles, String> {
        let f = net.repair_files(channel)?;
        check(&f.firmware, &f.option_bytes)?;
        let resources = bundle(&f.resources)?;
        let update = flipper_updates::repair::has_radio_update(radio_version, stack_type, &f.radio);
        let shown = if radio_version.is_empty() { "none running".to_string() } else { radio_version.to_string() };
        let radio_note = if update {
            format!("the wireless stack {} goes on (the device's: {shown})", f.radio.version)
        } else {
            format!("the device's wireless stack {shown} is current; it stays")
        };
        let region_data = match region {
            0 => None,
            _ => Some(net.region().map_err(|e| format!("The region data: {e}"))?),
        };
        Ok(LegacyFiles {
            version: f.version,
            firmware: f.firmware,
            option_bytes: f.option_bytes,
            radio: update.then_some(f.radio.data),
            radio_note,
            resources,
            region_data,
        })
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
    /// The legacy update's bootloader part, after the firmware backed up `/int` into `backup` (`None`: skipped, as the
    /// C++ app skips it for the firmware Flipper shipped with) and rebooted into the bootloader.
    Legacy {
        files: Box<LegacyFiles>,
        backup: Option<std::path::PathBuf>,
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

/// What runs on the firmware once the device is back: the resources, `/int` restored, the region data, a restart.
pub struct Finish {
    pub bundle: Bundle,
    pub restore: Option<flipper_rpc::restore::BackupArchive>,
    /// Made before anything changed; `None`: fetched then, if the device's region needs it (the repair).
    pub region_data: Option<Vec<u8>>,
}

/// The end of the bootloader part.
pub struct Done {
    pub outcome: Outcome,
    /// The steps on the firmware side, once the device is back.
    pub finish: Option<Finish>,
}

/// Why a running firmware cannot be rebooted into a bootloader the app reaches, if it cannot.
pub fn unreachable_from_firmware(kind: &Kind) -> Option<String> {
    match kind {
        Kind::Socket(_) => None,
        Kind::Serial(_) => Some(
            "This build cannot reach the bootloader over USB yet, so the device would be left in recovery mode; nothing was \
             sent."
                .into(),
        ),
        _ => Some("The device is not running its firmware.".into()),
    }
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
        finish: f.resources.map(|bundle| Finish { bundle, restore: None, region_data: None }),
    })
}

fn legacy<B: Bootloader>(b: &mut B, files: LegacyFiles, backup: Option<std::path::PathBuf>) -> Result<Done, String> {
    // the backup read back whole before anything is written (it goes back on once the firmware runs again)
    let restore = match &backup {
        Some(p) => Some(
            std::fs::File::open(p)
                .map_err(|e| format!("{}: {e}", p.display()))
                .and_then(|f| {
                    flipper_rpc::restore::read_backup(std::io::BufReader::new(f), &flipper_rpc::restore::ArchiveLimits::default())
                        .map_err(|e| e.to_string())
                })
                .map_err(|e| format!("Reading the backup of /int back: {e}; nothing was written to the chip"))?,
        ),
        None => None,
    };
    let radio_error = match &files.radio {
        // with a wireless stack update: the repair's order (boot mode, stack, firmware, option bytes)
        Some(image) => recovery::full_repair(b, &files.firmware, Some(image), &files.option_bytes).map_err(|e| e.to_string())?.radio_error,
        None => {
            recovery::firmware_and_leave(b, &files.firmware).map_err(|e| e.to_string())?;
            None
        }
    };
    Ok(Done {
        outcome: Outcome {
            summary: format!("The firmware {} is written and the chip starts it; {}", files.version, files.radio_note),
            // the partial success both CLIs report (exit 3) for the legacy update too
            warning: radio_error.map(|e| format!("updated, but the radio stack was not installed: {e}")),
        },
        finish: Some(Finish { bundle: files.resources, restore, region_data: files.region_data }),
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
        Job::Legacy { files, backup } => legacy(b, *files, backup),
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
