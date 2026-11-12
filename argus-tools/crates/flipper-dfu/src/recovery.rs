//! The recovery flows of the C++ app's `backend/flipperzero/recovery.cpp`, step for step, on a
//! [`Dfu`] device: leaving recovery, the boot mode (option bytes), the FUS and wireless stack steps
//! (AN5185), flashing a firmware `.dfu`, placing a wireless stack image below the secure flash, and
//! correcting the option bytes from a file.
//!
//! Each of these changes the device; callers ask first. Fixed in both versions: the wireless stack's
//! address (`origin + 4 KiB x SFSA - size`, rounded down to a page) wrapped around for an image
//! larger than the room below SFSA, and correcting the option bytes went on after their read had
//! failed (writing the file's fields over nothing). Here an `OptionBytes` only exists once read.

use std::fmt;

use crate::device::{Dfu, Transport};
use crate::dfuse;
use crate::wb55::{self, fus, partition, OptionBytes, OptionBytesFile, Wb55, Wb55Error};

/// The STM32WB55's flash page.
pub const PAGE_SIZE: u32 = 0x1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Device(Wb55Error),
    File(dfuse::Error),
    OptionBytesFile(wb55::Error),
    /// The FUS reported a state the flow does not expect.
    UnexpectedFusState(wb55::FusState),
    EmptyImage,
    /// The wireless stack image does not fit below the secure flash start.
    NoRoom {
        size: usize,
        origin: u32,
        sfsa: u32,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Device(e) => write!(f, "{e}"),
            Error::File(e) => write!(f, "{e}"),
            Error::OptionBytesFile(e) => write!(f, "{e}"),
            Error::UnexpectedFusState(s) => write!(f, "unexpected FUS state 0x{:02x}, error 0x{:02x}", s.status, s.error),
            Error::EmptyImage => write!(f, "the image is empty"),
            Error::NoRoom { size, origin, sfsa } => {
                write!(f, "a {size}-byte image does not fit below SFSA 0x{sfsa:x} (flash at 0x{origin:08x})")
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<Wb55Error> for Error {
    fn from(e: Wb55Error) -> Error {
        Error::Device(e)
    }
}

impl From<crate::device::Error> for Error {
    fn from(e: crate::device::Error) -> Error {
        Error::Device(Wb55Error::Dfu(e))
    }
}

/// `Recovery::exitRecoveryMode`.
pub fn exit_recovery<T: Transport>(d: &mut Dfu<T>) -> Result<(), Error> {
    Ok(d.leave()?)
}

/// `Recovery::setBootMode`: nBOOT0 and nSWBOOT0 set (boot the firmware) or cleared (stay in the
/// bootloader).
pub fn set_boot_mode<T: Transport>(d: &mut Dfu<T>, normal: bool) -> Result<(), Error> {
    let current = d.option_bytes()?;
    let mut ob = current.clone();
    ob.set("nBOOT0", u32::from(normal)).map_err(Wb55Error::from)?;
    ob.set("nSWBOOT0", u32::from(normal)).map_err(Wb55Error::from)?;
    Ok(d.set_option_bytes(&ob, &current)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WirelessStatus {
    FusRunning,
    WirelessStackRunning,
    ErrorOccurred,
    Unhandled,
}

/// `Recovery::wirelessStatus`.
pub fn wireless_status<T: Transport>(d: &mut Dfu<T>) -> Result<WirelessStatus, Error> {
    let s = d.fus_state()?;
    Ok(match (s.status, s.error) {
        (fus::IDLE, fus::NO_ERROR) => WirelessStatus::FusRunning,
        (fus::ERROR_OCCURRED, fus::NOT_RUNNING) => WirelessStatus::WirelessStackRunning,
        (fus::ERROR_OCCURRED, _) => WirelessStatus::ErrorOccurred,
        _ => WirelessStatus::Unhandled,
    })
}

/// `Recovery::startFUS`: if the FUS runs, the device is restarted for consistency; if the wireless
/// stack runs, a second state read starts the FUS (its answer is not used).
pub fn start_fus<T: Transport>(d: &mut Dfu<T>) -> Result<(), Error> {
    let s = d.fus_state()?;
    match (s.status, s.error) {
        (fus::IDLE, fus::NO_ERROR) => Ok(d.leave()?),
        (fus::ERROR_OCCURRED, fus::NOT_RUNNING) => {
            let _ = d.fus_state();
            Ok(())
        }
        _ => Err(Error::UnexpectedFusState(s)),
    }
}

pub fn start_wireless_stack<T: Transport>(d: &mut Dfu<T>) -> Result<(), Error> {
    Ok(d.command(&[wb55::FUS_START_WS])?)
}

pub fn delete_wireless_stack<T: Transport>(d: &mut Dfu<T>) -> Result<(), Error> {
    Ok(d.command(&[wb55::FUS_FW_DELETE])?)
}

pub fn upgrade_wireless_stack<T: Transport>(d: &mut Dfu<T>) -> Result<(), Error> {
    Ok(d.command(&[wb55::FUS_FW_UPGRADE])?)
}

/// `Recovery::downloadFirmware`: a DfuSe file, parsed before anything is sent.
pub fn download_firmware<T: Transport>(d: &mut Dfu<T>, file: &[u8]) -> Result<(), Error> {
    let f = dfuse::parse(file).map_err(Error::File)?;
    Ok(d.download_file(&f)?)
}

/// Where the wireless stack image goes: right below the secure flash start (SFSA, in pages), on a
/// page boundary, and never below the flash itself.
pub fn wireless_stack_address(origin: u32, sfsa: u32, size: usize) -> Result<u32, Error> {
    let top = u64::from(origin) + u64::from(PAGE_SIZE) * u64::from(sfsa);
    let no_room = Error::NoRoom { size, origin, sfsa };
    let start = top.checked_sub(size as u64).ok_or(no_room.clone())? & !u64::from(PAGE_SIZE - 1);
    if start < u64::from(origin) || top > 1 << 32 {
        return Err(no_room);
    }
    Ok(start as u32)
}

/// `Recovery::downloadWirelessStack`: the image erased and written at `address`, or (None) right
/// below SFSA as the option bytes give it.
pub fn download_wireless_stack<T: Transport>(d: &mut Dfu<T>, image: &[u8], address: Option<u32>) -> Result<u32, Error> {
    if image.is_empty() {
        return Err(Error::EmptyImage);
    }
    let address = match address {
        Some(a) => a,
        None => {
            let ob = d.option_bytes()?;
            let origin = d.partition_origin(partition::FLASH)?;
            wireless_stack_address(origin, ob.value("SFSA").map_err(Wb55Error::from)?, image.len())?
        }
    };
    d.erase(address, image.len() as u64)?;
    d.download(image, address, partition::FLASH)?;
    Ok(address)
}

/// `Recovery::checkWirelessStack`: whether a wireless stack is installed.
pub fn check_wireless_stack<T: Transport>(d: &mut Dfu<T>) -> Result<bool, Error> {
    Ok(d.version_info()?.wireless != "0.0.0")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionBytesOutcome {
    /// Nothing to correct: the device was told to leave the bootloader.
    Left { differ: wb55::FieldValues },
    /// The `rw` fields that differed were written.
    Written { fixed: wb55::FieldValues, differ: wb55::FieldValues },
}

/// `Recovery::downloadOptionBytes`: the file's `rw` fields that differ are written; if none
/// differ, the device leaves the bootloader. `r` fields that differ are reported, never written.
pub fn download_option_bytes<T: Transport>(d: &mut Dfu<T>, file: &[u8]) -> Result<OptionBytesOutcome, Error> {
    let loaded = OptionBytesFile::parse(file).map_err(Error::OptionBytesFile)?;
    let actual: OptionBytes = d.option_bytes()?;
    let (fixed, differ) = loaded.compare(&actual);
    if fixed.is_empty() {
        d.leave()?;
        return Ok(OptionBytesOutcome::Left { differ });
    }
    let mut ob = actual.clone();
    for (name, value) in &fixed {
        ob.set(name, *value).map_err(Wb55Error::from)?;
    }
    d.set_option_bytes(&ob, &actual)?;
    Ok(OptionBytesOutcome::Written { fixed, differ })
}

/// How the multi-step flows reach the chip across its resets: each step opens the bootloader
/// again, waiting (bounded) for it to come back, as the C++ operations wait for the device to
/// re-enumerate.
pub trait Bootloader {
    type T: Transport;
    /// The bootloader, once it answers; an error once the wait is over.
    fn open(&mut self) -> Result<Dfu<Self::T>, Error>;
    /// Waits between status polls.
    fn pause(&mut self);
}

/// `WirelessStackDownloadOperation`'s bounds: whole installs, checks after one, and status polls
/// while the FUS deletes or installs (the C++ loops polled for ever while the FUS reported a state
/// it does not expect; both now give up).
pub const INSTALL_TRIES: usize = 3;
pub const CHECK_TRIES: usize = 3;
pub const MAX_STATUS_POLLS: usize = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowError {
    Step {
        step: &'static str,
        error: Error,
    },
    /// The FUS reported an error, or the wireless stack still running after a delete.
    Fus {
        step: &'static str,
        status: WirelessStatus,
    },
    /// The FUS never settled within [`MAX_STATUS_POLLS`] polls.
    FusTimeout {
        step: &'static str,
    },
    GaveUp,
}

impl fmt::Display for FlowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FlowError::Step { step, error } => write!(f, "{step}: {error}"),
            FlowError::Fus { step, status } => write!(f, "{step}: the FUS reports {status:?}"),
            FlowError::FusTimeout { step } => write!(f, "{step}: the FUS did not finish within {MAX_STATUS_POLLS} polls"),
            FlowError::GaveUp => write!(f, "could not install the wireless stack after {INSTALL_TRIES} tries"),
        }
    }
}

impl std::error::Error for FlowError {}

fn step<B: Bootloader, R>(b: &mut B, name: &'static str, f: impl FnOnce(&mut Dfu<B::T>) -> Result<R, Error>) -> Result<R, FlowError> {
    let mut d = b.open().map_err(|error| FlowError::Step { step: name, error })?;
    f(&mut d).map_err(|error| FlowError::Step { step: name, error })
}

/// Polls the wireless status until `settled` says it is done (Ok(true)) or failed (Err). A status
/// that cannot be read (the chip is resetting) or that the flow does not know means "wait".
fn poll_fus<B: Bootloader>(
    b: &mut B,
    name: &'static str,
    settled: impl Fn(WirelessStatus) -> Result<bool, WirelessStatus>,
) -> Result<(), FlowError> {
    for _ in 0..MAX_STATUS_POLLS {
        b.pause();
        let Ok(mut d) = b.open() else { continue };
        let Ok(status) = wireless_status(&mut d) else { continue };
        match settled(status) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(status) => return Err(FlowError::Fus { step: name, status }),
        }
    }
    Err(FlowError::FusTimeout { step: name })
}

/// `WirelessStackDownloadOperation`: start the FUS, delete the wireless stack, write the image
/// below SFSA, have the FUS install it, and check its version; the whole install up to
/// [`INSTALL_TRIES`] times.
pub fn install_wireless_stack<B: Bootloader>(b: &mut B, image: &[u8]) -> Result<(), FlowError> {
    for _ in 0..INSTALL_TRIES {
        step(b, "start the FUS", start_fus)?;
        step(b, "delete the wireless stack", delete_wireless_stack)?;
        poll_fus(b, "delete the wireless stack", |s| match s {
            WirelessStatus::FusRunning => Ok(true),
            WirelessStatus::WirelessStackRunning | WirelessStatus::ErrorOccurred => Err(s),
            WirelessStatus::Unhandled => Ok(false),
        })?;
        step(b, "write the wireless stack", |d| download_wireless_stack(d, image, None).map(|_| ()))?;
        step(b, "install the wireless stack", upgrade_wireless_stack)?;
        poll_fus(b, "install the wireless stack", |s| match s {
            WirelessStatus::ErrorOccurred => Err(s),
            WirelessStatus::Unhandled => Ok(false),
            _ => Ok(true),
        })?;
        for _ in 0..CHECK_TRIES {
            b.pause();
            if let Ok(mut d) = b.open() {
                if check_wireless_stack(&mut d).unwrap_or(false) {
                    return Ok(());
                }
            }
        }
    }
    Err(FlowError::GaveUp)
}

/// What a repair did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairReport {
    /// The wireless stack step's error; the repair goes on without it, as the C++ app does.
    pub radio_error: Option<String>,
    pub option_bytes: OptionBytesOutcome,
}

/// `FullRepairOperation`'s steps on the bootloader, with local files instead of the update server's:
/// recovery boot mode, the wireless stack (if given; a failure there does not stop the repair),
/// the firmware, then the option bytes from the file, after which the chip boots its firmware. The
/// steps after that (assets, region data, a restart) run on the firmware over RPC and are not here.
pub fn full_repair<B: Bootloader>(
    b: &mut B,
    firmware: &[u8],
    radio: Option<&[u8]>,
    option_bytes: &[u8],
) -> Result<RepairReport, FlowError> {
    // checked before anything changes
    let fw = dfuse::parse(firmware).map_err(|e| FlowError::Step { step: "read the firmware", error: Error::File(e) })?;
    OptionBytesFile::parse(option_bytes)
        .map_err(|e| FlowError::Step { step: "read the option bytes file", error: Error::OptionBytesFile(e) })?;
    step(b, "set the recovery boot mode", |d| set_boot_mode(d, false))?;
    let radio_error = match radio {
        Some(image) => install_wireless_stack(b, image).err().map(|e| e.to_string()),
        None => None,
    };
    step(b, "write the firmware", |d| Ok(d.download_file(&fw)?))?;
    let option_bytes = step(b, "correct the option bytes", |d| download_option_bytes(d, option_bytes))?;
    Ok(RepairReport { radio_error, option_bytes })
}
