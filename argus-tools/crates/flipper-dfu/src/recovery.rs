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
