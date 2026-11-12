//! ST's DfuSe protocol (UM0391, on top of USB DFU 1.1) as the C++ app's `dfu/dfusedevice.cpp` speaks
//! it to the STM32WB55's ROM bootloader: `DFU_GETSTATUS` polling, `DFU_DNLOAD` with block 0 for the
//! DfuSe commands (`0x21` set address, `0x41` erase page, and the FUS commands), blocks from 2 for
//! data at the address pointer, `DFU_UPLOAD` the same way, `DFU_ABORT`, `DFU_CLRSTATUS`, and the
//! zero-length download that leaves the bootloader.
//!
//! Pure code over a [`Transport`]: the tests drive it with a fake device, and the same requests in
//! the same order as the C++ app are checked by running both against one fake
//! (`tests/qt_device.rs`). The real USB transport is `crate::usb` (feature `usb`), which no test
//! uses.
//!
//! Where it is stricter than the C++ app was, the C++ app now does the same (fixed in both):
//! every wait is bounded ([`MAX_POLLS`] status reads per step, each poll timeout clamped to
//! [`MAX_POLL_MS`]; the C++ app read the timeout's bytes as signed `char`, so a byte of 0x80 or more
//! asked it to sleep for about 49 days), a transfer size of 0 is refused (both loops never ended),
//! a status that cannot be read stops the step instead of being taken as an error state to clear,
//! data that would need block numbers past 0xFFFF is refused, and an upload the device answers
//! with nothing is a failure (it was taken as the end of memory).

use std::fmt;

use crate::dfuse::DfuseFile;
use crate::layout::{self, MemoryLayout};

/// DFU class requests (DFU 1.1, table 3.2).
pub mod request {
    pub const DETACH: u8 = 0;
    pub const DNLOAD: u8 = 1;
    pub const UPLOAD: u8 = 2;
    pub const GETSTATUS: u8 = 3;
    pub const CLRSTATUS: u8 = 4;
    pub const GETSTATE: u8 = 5;
    pub const ABORT: u8 = 6;
}

/// `bState` values (DFU 1.1, section 6.1.2).
pub mod state {
    pub const APP_IDLE: u8 = 0;
    pub const APP_DETACH: u8 = 1;
    pub const DFU_IDLE: u8 = 2;
    pub const DNLOAD_SYNC: u8 = 3;
    pub const DNBUSY: u8 = 4;
    pub const DNLOAD_IDLE: u8 = 5;
    pub const MANIFEST_SYNC: u8 = 6;
    pub const MANIFEST: u8 = 7;
    pub const MANIFEST_WAIT_RESET: u8 = 8;
    pub const UPLOAD_IDLE: u8 = 9;
    pub const ERROR: u8 = 10;
}

/// `bStatus` OK; anything else is an error code (`errTARGET` = 1 ... `errSTALLEDPKT` = 15).
pub const STATUS_OK: u8 = 0;

/// The longest wait for one poll; a device asking for more gets this (up to 16.7 million ms fit).
pub const MAX_POLL_MS: u32 = 10_000;
/// The most status reads one step (a command, a block) may take before it is given up.
pub const MAX_POLLS: usize = 1000;
/// The most bytes one upload reads.
pub const MAX_UPLOAD: usize = 16 << 20;
/// DfuSe's commands are block 0, block 1 is reserved; data starts at block 2.
const FIRST_DATA_BLOCK: u64 = 2;

/// DfuSe commands in a block-0 download.
pub mod command {
    pub const SET_ADDRESS: u8 = 0x21;
    pub const ERASE: u8 = 0x41;
    /// STM32WB FUS commands (AN5185), sent alone.
    pub const FUS_FW_DELETE: u8 = 0x52;
    pub const FUS_FW_UPGRADE: u8 = 0x53;
    pub const FUS_START_WS: u8 = 0x5A;
}

/// The device side, as the C++ app's `USBDevice` offers it: interface 0 of the DFU device, already
/// opened and claimed.
pub trait Transport {
    /// Selects alternate setting `alt` of interface 0.
    fn set_alt(&mut self, alt: u8) -> Result<(), TransportError>;
    /// A class request to interface 0, host to device; returns the bytes the device took.
    fn control_out(&mut self, request: u8, value: u16, data: &[u8]) -> Result<usize, TransportError>;
    /// A class request to interface 0, device to host, of at most `length` bytes.
    fn control_in(&mut self, request: u8, value: u16, length: u16) -> Result<Vec<u8>, TransportError>;
    /// The DFU functional descriptor (type 0x21, 9 bytes) of interface 0, or nothing.
    fn functional_descriptor(&mut self) -> Result<Vec<u8>, TransportError>;
    /// The interface string of alternate setting `alt` (its memory layout), as ASCII.
    fn interface_string(&mut self, alt: u8) -> Result<Vec<u8>, TransportError>;
    /// Waits as the device asked (a fake device only records it).
    fn sleep_ms(&mut self, ms: u32);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError(pub String);

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DfuStatus {
    pub status: u8,
    /// `bwPollTimeout`, 24 bits, as the device sent it.
    pub poll_timeout: u32,
    pub state: u8,
    pub string: u8,
}

impl DfuStatus {
    /// The wait the device asked for, clamped to [`MAX_POLL_MS`].
    pub fn wait_ms(&self) -> u32 {
        self.poll_timeout.min(MAX_POLL_MS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Transport {
        during: &'static str,
        error: TransportError,
    },
    /// The device took fewer bytes than were sent.
    Short {
        during: &'static str,
    },
    /// `DFU_GETSTATUS` did not answer with six bytes.
    BadStatus {
        len: usize,
    },
    /// The device reported an error status.
    Device {
        during: &'static str,
        status: u8,
        state: u8,
    },
    /// The device was not idle after an abort.
    NotIdle {
        status: u8,
        state: u8,
    },
    /// The device stayed busy for [`MAX_POLLS`] status reads.
    Busy {
        during: &'static str,
    },
    NoFunctionalDescriptor,
    ZeroTransferSize,
    /// More data than block numbers up to 0xFFFF can address.
    TooLarge {
        len: usize,
    },
    Layout(layout::Error),
    /// The device answered an upload with nothing.
    EmptyUpload {
        offset: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Transport { during, error } => write!(f, "{during}: {error}"),
            Error::Short { during } => write!(f, "{during}: the device took fewer bytes than were sent"),
            Error::BadStatus { len } => write!(f, "DFU_GETSTATUS answered {len} bytes instead of 6"),
            Error::Device { during, status, state } => write!(f, "{during}: device reports status {status}, state {state}"),
            Error::NotIdle { status, state } => write!(f, "device not idle after abort (status {status}, state {state})"),
            Error::Busy { during } => write!(f, "{during}: device still busy after {MAX_POLLS} status reads"),
            Error::NoFunctionalDescriptor => write!(f, "no DFU functional descriptor"),
            Error::ZeroTransferSize => write!(f, "device reports a transfer size of 0"),
            Error::TooLarge { len } => write!(f, "{len} bytes need block numbers past 0xFFFF"),
            Error::Layout(e) => write!(f, "{e}"),
            Error::EmptyUpload { offset } => write!(f, "device answered the upload at offset {offset} with nothing"),
        }
    }
}

impl std::error::Error for Error {}

impl From<layout::Error> for Error {
    fn from(e: layout::Error) -> Error {
        Error::Layout(e)
    }
}

/// A DfuSe device over a transport. Each method is the C++ `DfuseDevice` method of the same name,
/// request for request.
pub struct Dfu<T> {
    t: T,
}

impl<T: Transport> Dfu<T> {
    pub fn new(t: T) -> Dfu<T> {
        Dfu { t }
    }

    pub fn into_inner(self) -> T {
        self.t
    }

    fn out(&mut self, during: &'static str, request: u8, value: u16, data: &[u8]) -> Result<(), Error> {
        let n = self.t.control_out(request, value, data).map_err(|error| Error::Transport { during, error })?;
        if n != data.len() {
            return Err(Error::Short { during });
        }
        Ok(())
    }

    pub fn get_status(&mut self) -> Result<DfuStatus, Error> {
        let b = self.t.control_in(request::GETSTATUS, 0, 6).map_err(|error| Error::Transport { during: "DFU_GETSTATUS", error })?;
        if b.len() != 6 {
            return Err(Error::BadStatus { len: b.len() });
        }
        Ok(DfuStatus { status: b[0], poll_timeout: u32::from_le_bytes([b[1], b[2], b[3], 0]), state: b[4], string: b[5] })
    }

    pub fn clear_status(&mut self) -> Result<(), Error> {
        self.out("DFU_CLRSTATUS", request::CLRSTATUS, 0, &[])
    }

    pub fn abort(&mut self) -> Result<(), Error> {
        self.out("DFU_ABORT", request::ABORT, 0, &[])?;
        let s = self.get_status()?;
        if s.status != STATUS_OK || s.state != state::DFU_IDLE {
            return Err(Error::NotIdle { status: s.status, state: s.state });
        }
        self.t.sleep_ms(s.wait_ms());
        Ok(())
    }

    /// Brings the device to `dfuIDLE`: clears an error status, aborts anything else.
    pub fn prepare(&mut self) -> Result<(), Error> {
        let s = self.get_status()?;
        if s.status != STATUS_OK {
            self.clear_status()
        } else if s.state != state::DFU_IDLE {
            self.abort()
        } else {
            Ok(())
        }
    }

    /// Polls after a download until the device leaves `busy_state` (`DNBUSY`), or, with `until`,
    /// until it reaches that state. Every status must be OK.
    fn poll(&mut self, during: &'static str, until: Option<u8>) -> Result<(), Error> {
        self.poll_or_reset(during, until, false)
    }

    /// [`Self::poll`]; with `reset_ok`, a status that cannot be read means the device has reset
    /// (as it does after its option bytes are written) and counts as done.
    fn poll_or_reset(&mut self, during: &'static str, until: Option<u8>, reset_ok: bool) -> Result<(), Error> {
        for _ in 0..MAX_POLLS {
            let s = match self.get_status() {
                Err(Error::Transport { .. } | Error::BadStatus { .. }) if reset_ok => return Ok(()),
                s => s?,
            };
            if s.status != STATUS_OK {
                return Err(Error::Device { during, status: s.status, state: s.state });
            }
            self.t.sleep_ms(s.wait_ms());
            let done = match until {
                Some(want) => s.state == want,
                None => s.state != state::DNBUSY,
            };
            if done {
                return Ok(());
            }
        }
        Err(Error::Busy { during })
    }

    pub fn set_address(&mut self, address: u32) -> Result<(), Error> {
        let mut cmd = vec![command::SET_ADDRESS];
        cmd.extend(address.to_le_bytes());
        self.out("set address", request::DNLOAD, 0, &cmd)?;
        self.poll("set address", None)
    }

    pub fn erase_page(&mut self, address: u32) -> Result<(), Error> {
        let mut cmd = vec![command::ERASE];
        cmd.extend(address.to_le_bytes());
        self.out("erase", request::DNLOAD, 0, &cmd)?;
        self.poll("erase", None)
    }

    /// The memory layout of alternate setting `alt`.
    pub fn layout(&mut self, alt: u8) -> Result<MemoryLayout, Error> {
        let s = self.t.interface_string(alt).map_err(|error| Error::Transport { during: "interface string", error })?;
        Ok(MemoryLayout::parse(&s)?)
    }

    /// Where alternate setting `alt`'s memory starts.
    pub fn partition_origin(&mut self, alt: u8) -> Result<u32, Error> {
        Ok(self.layout(alt)?.address)
    }

    /// Erases every page that `address..address + size` touches, using alternate setting 0's
    /// layout (as the C++ app). The range must lie inside it.
    pub fn erase(&mut self, address: u32, size: u64) -> Result<(), Error> {
        self.prepare()?;
        let layout = self.layout(0)?;
        let pages = layout.page_addresses(address, u64::from(address) + size)?;
        for page in pages {
            self.erase_page(page)?;
        }
        Ok(())
    }

    fn transfer_size(&mut self) -> Result<u16, Error> {
        let d = self.t.functional_descriptor().map_err(|error| Error::Transport { during: "functional descriptor", error })?;
        if d.len() < 9 {
            return Err(Error::NoFunctionalDescriptor);
        }
        match u16::from_le_bytes([d[5], d[6]]) {
            0 => Err(Error::ZeroTransferSize),
            n => Ok(n),
        }
    }

    fn set_alt(&mut self, alt: u8) -> Result<(), Error> {
        self.t.set_alt(alt).map_err(|error| Error::Transport { during: "set alternate setting", error })
    }

    /// Writes `data` at `address` through alternate setting `alt`, one transfer-size block at a
    /// time. The memory must have been erased.
    pub fn download(&mut self, data: &[u8], address: u32, alt: u8) -> Result<(), Error> {
        self.download_impl(data, address, alt, false)
    }

    /// [`Self::download`] for a write after which the device resets (the option bytes): losing the
    /// device after the last block has been sent is success. An error status is still an error.
    pub fn download_may_reset(&mut self, data: &[u8], address: u32, alt: u8) -> Result<(), Error> {
        self.download_impl(data, address, alt, true)
    }

    fn download_impl(&mut self, data: &[u8], address: u32, alt: u8, may_reset: bool) -> Result<(), Error> {
        self.set_alt(alt)?;
        self.prepare()?;
        self.set_address(address)?;
        let size = self.transfer_size()?;
        if data.len().div_ceil(usize::from(size)) as u64 > 0x1_0000 - FIRST_DATA_BLOCK {
            return Err(Error::TooLarge { len: data.len() });
        }
        let blocks = data.len().div_ceil(usize::from(size));
        for (i, chunk) in data.chunks(usize::from(size)).enumerate() {
            let block = (FIRST_DATA_BLOCK + i as u64) as u16;
            self.out("DFU_DNLOAD", request::DNLOAD, block, chunk)?;
            self.poll_or_reset("download", Some(state::DNLOAD_IDLE), may_reset && i + 1 == blocks)?;
        }
        Ok(())
    }

    /// A whole DfuSe file: every element's pages erased first, then every element written.
    pub fn download_file(&mut self, file: &DfuseFile) -> Result<(), Error> {
        for t in &file.targets {
            for e in &t.elements {
                self.erase(e.address, e.data.len() as u64)?;
            }
        }
        for t in &file.targets {
            for e in &t.elements {
                self.download(&e.data, e.address, t.alt)?;
            }
        }
        Ok(())
    }

    /// A raw block-0 download through alternate setting 0 (the FUS commands).
    pub fn command(&mut self, data: &[u8]) -> Result<(), Error> {
        self.set_alt(0)?;
        self.prepare()?;
        self.out("DFU_DNLOAD", request::DNLOAD, 0, data)?;
        self.poll("command", None)
    }

    /// Reads `len` bytes from `address` through alternate setting `alt`. Fewer come back when the
    /// device ends the transfer early (a short block), as the C++ app reads it.
    pub fn upload(&mut self, address: u32, len: usize, alt: u8) -> Result<Vec<u8>, Error> {
        if len > MAX_UPLOAD {
            return Err(Error::TooLarge { len });
        }
        self.set_alt(alt)?;
        self.prepare()?;
        self.abort()?;
        self.set_address(address)?;
        // the C++ app does not check this one either; set_address has left the device in dnIDLE
        let _ = self.abort();
        let size = self.transfer_size()?;
        if len.div_ceil(usize::from(size)) as u64 > 0x1_0000 - FIRST_DATA_BLOCK {
            return Err(Error::TooLarge { len });
        }
        let mut out = Vec::with_capacity(len);
        let mut block = FIRST_DATA_BLOCK;
        while out.len() < len {
            let want = (len - out.len()).min(usize::from(size));
            let got = self
                .t
                .control_in(request::UPLOAD, block as u16, want as u16)
                .map_err(|error| Error::Transport { during: "DFU_UPLOAD", error })?;
            if got.is_empty() {
                return Err(Error::EmptyUpload { offset: out.len() });
            }
            let short = got.len() < want;
            out.extend(&got[..got.len().min(want)]);
            if short {
                break;
            }
            block += 1;
        }
        Ok(out)
    }

    /// Leaves the bootloader: set the address pointer, then a zero-length download. The device
    /// resets, so the last status read is expected to fail and is not checked.
    pub fn leave(&mut self) -> Result<(), Error> {
        self.set_alt(0)?;
        self.prepare()?;
        self.abort()?;
        let _ = self.set_address(0x080F_FFFF);
        self.out("DFU_DNLOAD", request::DNLOAD, 0, &[])?;
        let _ = self.get_status();
        Ok(())
    }
}
