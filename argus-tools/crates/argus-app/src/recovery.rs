//! Recovery mode in the app: the repair and leaving the bootloader, through `flipper_dfu::recovery` and its
//! `Bootloader` interface, on a thread of their own.
//!
//! The only bootloader the app can reach is one on a Unix socket (`--dfu-socket`, `flipper_dfu::line::SocketBootloader`;
//! the fake in the tests). A device in recovery mode found on USB is listed, but no USB transport is wired in: asking it
//! to do anything is refused before anything starts. Nothing here flashes a real device.
//!
//! The repair takes local files (firmware `.dfu`, the option bytes file, optionally the wireless stack image), as
//! `argus-cli recovery repair` does; the steps after it on the firmware side (resources, region data, a restart) are not
//! in the app yet (`argus-cli recovery repair --resources ... --port` does them).

use std::path::Path;
use std::sync::mpsc::Sender;
use std::time::Duration;

use flipper_dfu::line::SocketBootloader;
use flipper_dfu::recovery::{self, Bootloader};

use crate::device::{read_local, Outcome, Waker};
use crate::discovery::Kind;

/// The largest files read: a DfuSe file (as the parser's limit), an option bytes file, a wireless stack image.
const MAX_FIRMWARE: u64 = 16 << 20;
const MAX_OPTION_BYTES: u64 = 64 << 10;
const MAX_RADIO: u64 = 1 << 20;

pub struct RepairFiles {
    pub firmware: Vec<u8>,
    pub option_bytes: Vec<u8>,
    pub radio: Option<Vec<u8>>,
}

impl RepairFiles {
    /// Reads and checks the files before anything is opened: a damaged file never reaches the device.
    pub fn read(firmware: &Path, option_bytes: &Path, radio: Option<&Path>) -> Result<RepairFiles, String> {
        let firmware = read_local(firmware, MAX_FIRMWARE)?;
        flipper_dfu::dfuse::parse(&firmware).map_err(|e| format!("The firmware file is refused: {e}"))?;
        let option_bytes = read_local(option_bytes, MAX_OPTION_BYTES)?;
        flipper_dfu::wb55::OptionBytesFile::parse(&option_bytes).map_err(|e| format!("The option bytes file is refused: {e:?}"))?;
        let radio = radio.map(|p| read_local(p, MAX_RADIO)).transpose()?;
        Ok(RepairFiles { firmware, option_bytes, radio })
    }
}

pub enum Job {
    Repair(RepairFiles),
    /// Leave the bootloader and start the firmware.
    Exit,
}

/// Why a device cannot be reached for recovery, if it cannot.
pub fn unreachable(kind: &Kind) -> Option<String> {
    match kind {
        Kind::RecoverySocket(_) => None,
        Kind::RecoveryUsb(_) => Some("This build cannot reach a device in recovery mode over USB yet; nothing was sent.".into()),
        _ => Some("The device is not in recovery mode.".into()),
    }
}

fn run<B: Bootloader>(b: &mut B, job: Job) -> Result<Outcome, String> {
    match job {
        Job::Repair(f) => {
            let r = recovery::full_repair(b, &f.firmware, f.radio.as_deref(), &f.option_bytes).map_err(|e| e.to_string())?;
            Ok(Outcome {
                summary: "The firmware is written and the chip starts it".into(),
                // the partial success both CLIs report (exit 3): the chip runs, without its wireless stack
                warning: r.radio_error.map(|e| format!("repaired, but the radio stack was not installed: {e}")),
            })
        }
        Job::Exit => {
            let mut d = b.open().map_err(|e| e.to_string())?;
            recovery::exit_recovery(&mut d).map_err(|e| e.to_string())?;
            Ok(Outcome { summary: "The chip left recovery mode".into(), warning: None })
        }
    }
}

/// Runs `job` on the bootloader `kind` names, on a new thread; the result comes back on `tx`.
pub fn start(kind: Kind, poll: Duration, job: Job, tx: Sender<Result<Outcome, String>>, waker: Waker) -> Result<(), String> {
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
