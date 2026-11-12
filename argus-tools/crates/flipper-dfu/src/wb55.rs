//! The STM32WB55 on top of the DfuSe protocol, as the C++ app's `dfu/device/stm32wb55/` has it: the
//! option bytes (read, compared with a file, written), the FUS state and commands (AN5185), the
//! device information table with the FUS and wireless stack versions, and the OTP area.
//!
//! The option bytes decide how the chip boots and how it is protected; a wrong write can lock it for
//! good (RDP level 2 is permanent). So, in both versions now (fixed in the C++ app too):
//!
//! - a write starts from the 128 bytes read from the device and changes only the named fields, with
//!   their complements; the C++ app built the write from an uninitialised buffer, so every bit no
//!   field covers (the twelve reserved words among them) was whatever was on the stack;
//! - RDP (read protection) is never written: it stays what the device has;
//! - a value must fit its field (`RDP:0x1CC` used to be cut to 0xCC, level 2);
//! - an option bytes file's third column is honoured: only `rw` fields are corrected, `r` fields are
//!   only compared (the C++ app ignored the column and would rewrite SFSA, RDP and the rest);
//! - `IWGDSTDBY` (ST's misspelling, used in Flipper's `ob.data`) is the same field as `IWDGSTDBY`,
//!   and a field named twice is refused.

use std::fmt;

use crate::device::{Dfu, Error as DfuError, Transport};

pub const OPTION_BYTES_ADDR: u32 = 0x1FFF_8000;
pub const OPTION_BYTES_SIZE: usize = 128;
pub const OTP_ADDR: u32 = 0x1FFF_7000;
pub const OTP_MAX: usize = 1024;
pub const FUS_STATUS_ADDR: u32 = 0xFFFF_0054;
pub const SRAM2A_BASE: u32 = 0x2003_0000;
pub const DIT_FUS_MAGIC: u32 = 0x0A94_656B;
/// FUS commands (AN5185), sent as a one-byte block-0 download.
pub const FUS_FW_DELETE: u8 = 0x52;
pub const FUS_FW_UPGRADE: u8 = 0x53;
pub const FUS_START_WS: u8 = 0x5A;

/// The alternate settings of the STM32WB55's DFU interface.
pub mod partition {
    pub const FLASH: u8 = 0;
    pub const OPTION_BYTES: u8 = 1;
    pub const OTP: u8 = 2;
}

/// One option byte field: the byte offset of its 32-bit word (the complement word follows it), its
/// first bit and its width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    pub name: &'static str,
    pub word: usize,
    pub shift: u32,
    pub width: u32,
}

const fn f(name: &'static str, word: usize, shift: u32, width: u32) -> Field {
    Field { name, word, shift, width }
}

/// The fields, in the C++ app's order (`OptionBytes::fieldNames`, without the misspelled alias),
/// with the bit layout of its packed structure.
pub const FIELDS: &[Field] = &[
    f("RDP", 0, 0, 8),
    f("BOR_LEV", 0, 9, 3),
    f("nBOOT0", 0, 27, 1),
    f("nBOOT1", 0, 23, 1),
    f("nSWBOOT0", 0, 26, 1),
    f("SRAM2RST", 0, 25, 1),
    f("SRAM2PE", 0, 24, 1),
    f("nRST_STOP", 0, 12, 1),
    f("nRST_STDBY", 0, 13, 1),
    f("nRSTSHDW", 0, 14, 1),
    f("WWDGSW", 0, 19, 1),
    f("IWDGSTDBY", 0, 18, 1),
    f("IWDGSTOP", 0, 17, 1),
    f("IWDGSW", 0, 16, 1),
    f("IPCCDBA", 104, 0, 14),
    f("ESE", 0, 8, 1),
    f("SFSA", 112, 0, 8),
    f("FSD", 112, 8, 1),
    f("DDS", 112, 12, 1),
    f("C2OPT", 120, 31, 1),
    f("NBRSD", 120, 30, 1),
    f("SNBRSA", 120, 25, 5),
    f("BRSD", 120, 23, 1),
    f("SBRSA", 120, 18, 5),
    f("SBRV", 120, 0, 18),
    f("PCROP1A_STRT", 8, 0, 9),
    f("PCROP1A_END", 16, 0, 9),
    f("PCROP_RDP", 16, 31, 1),
    f("PCROP1B_STRT", 40, 0, 9),
    f("PCROP1B_END", 48, 0, 9),
    f("WRP1A_STRT", 24, 0, 8),
    f("WRP1A_END", 24, 16, 8),
    f("WRP1B_STRT", 32, 0, 8),
    f("WRP1B_END", 32, 16, 8),
    f("AGC_TRIM", 0, 29, 3),
];

/// ST's misspelling of `IWDGSTDBY`, accepted in files as the same field.
pub const ALIAS: (&str, &str) = ("IWGDSTDBY", "IWDGSTDBY");

pub fn field(name: &str) -> Option<&'static Field> {
    let name = if name == ALIAS.0 { ALIAS.1 } else { name };
    FIELDS.iter().find(|f| f.name == name)
}

fn mask(width: u32) -> u32 {
    if width == 32 {
        u32::MAX
    } else {
        (1 << width) - 1
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Size(usize),
    UnknownField(String),
    TooWide {
        field: &'static str,
        value: u32,
    },
    /// RDP is never written.
    Rdp,
    File {
        line: usize,
        reason: &'static str,
    },
    TooLarge,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Size(n) => write!(f, "option bytes are {OPTION_BYTES_SIZE} bytes, not {n}"),
            Error::UnknownField(n) => write!(f, "no option byte field {n:?}"),
            Error::TooWide { field, value } => write!(f, "0x{value:x} does not fit {field}"),
            Error::Rdp => write!(f, "read protection (RDP) is never changed"),
            Error::File { line, reason } => write!(f, "option bytes file, line {line}: {reason}"),
            Error::TooLarge => write!(f, "option bytes file too large"),
        }
    }
}

impl std::error::Error for Error {}

/// The option bytes as read from the device: the raw 128 bytes, changed field by field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionBytes {
    raw: [u8; OPTION_BYTES_SIZE],
}

impl OptionBytes {
    pub fn from_bytes(b: &[u8]) -> Result<OptionBytes, Error> {
        let raw: [u8; OPTION_BYTES_SIZE] = b.try_into().map_err(|_| Error::Size(b.len()))?;
        Ok(OptionBytes { raw })
    }

    fn word(&self, at: usize) -> u32 {
        u32::from_le_bytes([self.raw[at], self.raw[at + 1], self.raw[at + 2], self.raw[at + 3]])
    }

    fn set_word(&mut self, at: usize, w: u32) {
        self.raw[at..at + 4].copy_from_slice(&w.to_le_bytes());
    }

    pub fn get(&self, f: &Field) -> u32 {
        (self.word(f.word) >> f.shift) & mask(f.width)
    }

    pub fn value(&self, name: &str) -> Result<u32, Error> {
        Ok(self.get(field(name).ok_or_else(|| Error::UnknownField(name.into()))?))
    }

    /// Sets a field and its complement. RDP cannot be set, and the value must fit.
    pub fn set(&mut self, name: &str, value: u32) -> Result<(), Error> {
        let f = field(name).ok_or_else(|| Error::UnknownField(name.into()))?;
        if f.name == "RDP" {
            return Err(Error::Rdp);
        }
        if value & !mask(f.width) != 0 {
            return Err(Error::TooWide { field: f.name, value });
        }
        let m = mask(f.width) << f.shift;
        let w = (self.word(f.word) & !m) | (value << f.shift);
        self.set_word(f.word, w);
        let c = (self.word(f.word + 4) & !m) | ((!value & mask(f.width)) << f.shift);
        self.set_word(f.word + 4, c);
        Ok(())
    }

    pub fn bytes(&self) -> &[u8; OPTION_BYTES_SIZE] {
        &self.raw
    }

    /// The bytes as they are written: every field but RDP with its complement rewritten from its
    /// value (as the C++ app's `OptionBytes::data`), every other bit as read.
    pub fn encoded(&self) -> [u8; OPTION_BYTES_SIZE] {
        let mut out = self.clone();
        for f in FIELDS.iter().filter(|f| f.name != "RDP") {
            let v = self.get(f);
            out.set(f.name, v).expect("a field's own value fits it");
        }
        out.raw
    }

    /// Every field, in [`FIELDS`] order.
    pub fn fields(&self) -> Vec<(&'static str, u32)> {
        FIELDS.iter().map(|f| (f.name, self.get(f))).collect()
    }
}

/// Whether a file's field may be corrected (`rw`) or only compared (`r`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    ReadWrite,
}

/// Field names with values.
pub type FieldValues = Vec<(&'static str, u32)>;

/// An option bytes file (Flipper's `ob.data`): `NAME:0xVALUE:r|rw` per line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionBytesFile {
    pub entries: Vec<(&'static Field, u32, Access)>,
}

/// The largest option bytes file read.
pub const MAX_FILE: usize = 64 << 10;

fn hex_value(s: &str) -> Option<u32> {
    let digits = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    digits.bytes().try_fold(0u32, |n, d| n.checked_mul(16)?.checked_add(char::from(d).to_digit(16)?))
}

impl OptionBytesFile {
    pub fn parse(data: &[u8]) -> Result<OptionBytesFile, Error> {
        if data.len() > MAX_FILE {
            return Err(Error::TooLarge);
        }
        let mut entries: Vec<(&'static Field, u32, Access)> = Vec::new();
        let lines: Vec<&[u8]> = data.split_inclusive(|&b| b == b'\n').collect();
        for (i, line) in lines.iter().enumerate() {
            let n = i + 1;
            let bad = |reason| Error::File { line: n, reason };
            let line = line.strip_suffix(b"\n").unwrap_or(line);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let line = std::str::from_utf8(line).map_err(|_| bad("not text"))?;
            let tokens: Vec<&str> = line.split(':').collect();
            let [name, value, access] = tokens[..] else { return Err(bad("not NAME:VALUE:ACCESS")) };
            let f = field(name).ok_or(bad("unknown field"))?;
            let value = hex_value(value).ok_or(bad("value is not a hex number"))?;
            if value & !mask(f.width) != 0 {
                return Err(bad("value does not fit the field"));
            }
            let access = match access {
                "r" => Access::Read,
                "rw" => Access::ReadWrite,
                _ => return Err(bad("access is not r or rw")),
            };
            if f.name == "RDP" && access != Access::Read {
                return Err(bad("RDP must be read-only"));
            }
            if entries.iter().any(|(e, _, _)| e.name == f.name) {
                return Err(bad("field named twice"));
            }
            entries.push((f, value, access));
        }
        Ok(OptionBytesFile { entries })
    }

    /// What differs from `actual`: the `rw` fields to correct, and the `r` fields that differ (never
    /// written).
    pub fn compare(&self, actual: &OptionBytes) -> (FieldValues, FieldValues) {
        let (mut fix, mut differ) = (Vec::new(), Vec::new());
        for (f, want, access) in &self.entries {
            if actual.get(f) != *want {
                match access {
                    Access::ReadWrite => fix.push((f.name, *want)),
                    Access::Read => differ.push((f.name, *want)),
                }
            }
        }
        (fix, differ)
    }
}

/// The FUS state (AN5185): status and error bytes read at 0xFFFF0054.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FusState {
    pub status: u8,
    pub error: u8,
}

pub mod fus {
    pub const IDLE: u8 = 0x00;
    pub const ERROR_OCCURRED: u8 = 0xFF;
    pub const NO_ERROR: u8 = 0x00;
    pub const NOT_RUNNING: u8 = 0xFE;
}

impl FusState {
    /// As the C++ `FUSState` reads it: statuses other than idle and error are ranges (0x10..0x1F
    /// is a firmware upgrade, and so on), normalised to their first value.
    pub fn new(status: u8, error: u8) -> FusState {
        let status = if status == fus::IDLE || status == fus::ERROR_OCCURRED { status } else { status & 0xF0 };
        FusState { status, error }
    }
}

/// The FUS and wireless stack versions from the device information table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    pub fus: String,
    pub wireless: String,
}

fn version(b: &[u8]) -> String {
    // build:4, branch:4, sub, minor, major
    format!("{}.{}.{}", b[3], b[2], b[1])
}

/// The table size read (the FUS table is 56 bytes, the wireless stack's 32).
pub const INFO_TABLE_SIZE: usize = 56;

impl VersionInfo {
    pub fn parse(table: &[u8; INFO_TABLE_SIZE]) -> VersionInfo {
        let magic = u32::from_le_bytes([table[0], table[1], table[2], table[3]]);
        if magic == DIT_FUS_MAGIC {
            VersionInfo { fus: version(&table[12..16]), wireless: version(&table[20..24]) }
        } else {
            VersionInfo { fus: version(&table[4..8]), wireless: version(&table[16..20]) }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wb55Error {
    Dfu(DfuError),
    OptionBytes(Error),
    /// A read came back with fewer bytes than asked for.
    ShortRead {
        what: &'static str,
        len: usize,
    },
    BadTableAddress(u32),
}

impl fmt::Display for Wb55Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Wb55Error::Dfu(e) => write!(f, "{e}"),
            Wb55Error::OptionBytes(e) => write!(f, "{e}"),
            Wb55Error::ShortRead { what, len } => write!(f, "failed to read {what} ({len} bytes)"),
            Wb55Error::BadTableAddress(a) => write!(f, "invalid device information table address 0x{a:08x}"),
        }
    }
}

impl std::error::Error for Wb55Error {}

impl From<DfuError> for Wb55Error {
    fn from(e: DfuError) -> Wb55Error {
        Wb55Error::Dfu(e)
    }
}

impl From<Error> for Wb55Error {
    fn from(e: Error) -> Wb55Error {
        Wb55Error::OptionBytes(e)
    }
}

/// The C++ `STM32WB55` methods, request for request.
pub trait Wb55 {
    fn option_bytes(&mut self) -> Result<OptionBytes, Wb55Error>;
    /// Writes the option bytes; the device reloads them and resets, so losing it after the write
    /// is success. RDP is checked to be unchanged first.
    fn set_option_bytes(&mut self, ob: &OptionBytes, current: &OptionBytes) -> Result<(), Wb55Error>;
    fn version_info(&mut self) -> Result<VersionInfo, Wb55Error>;
    fn otp(&mut self, len: usize) -> Result<Vec<u8>, Wb55Error>;
    fn fus_state(&mut self) -> Result<FusState, Wb55Error>;
}

impl<T: Transport> Wb55 for Dfu<T> {
    fn option_bytes(&mut self) -> Result<OptionBytes, Wb55Error> {
        let b = self.upload(OPTION_BYTES_ADDR, OPTION_BYTES_SIZE, partition::OPTION_BYTES)?;
        if b.len() != OPTION_BYTES_SIZE {
            return Err(Wb55Error::ShortRead { what: "option bytes", len: b.len() });
        }
        Ok(OptionBytes::from_bytes(&b)?)
    }

    fn set_option_bytes(&mut self, ob: &OptionBytes, current: &OptionBytes) -> Result<(), Wb55Error> {
        let rdp = field("RDP").expect("RDP is a field");
        if ob.get(rdp) != current.get(rdp) {
            return Err(Error::Rdp.into());
        }
        self.download_may_reset(&ob.encoded(), OPTION_BYTES_ADDR, partition::OPTION_BYTES)?;
        Ok(())
    }

    fn version_info(&mut self) -> Result<VersionInfo, Wb55Error> {
        let b = self.upload(SRAM2A_BASE, 4, partition::FLASH)?;
        let [a, b2, c, d] = b[..] else { return Err(Wb55Error::ShortRead { what: "the information table address", len: b.len() }) };
        let address = u32::from_le_bytes([a, b2, c, d]);
        if address < SRAM2A_BASE {
            return Err(Wb55Error::BadTableAddress(address));
        }
        let t = self.upload(address, INFO_TABLE_SIZE, partition::FLASH)?;
        let table: &[u8; INFO_TABLE_SIZE] =
            t[..].try_into().map_err(|_| Wb55Error::ShortRead { what: "the information table", len: t.len() })?;
        Ok(VersionInfo::parse(table))
    }

    fn otp(&mut self, len: usize) -> Result<Vec<u8>, Wb55Error> {
        let len = len.min(OTP_MAX);
        let b = self.upload(OTP_ADDR, len, partition::OTP)?;
        if b.len() != len {
            return Err(Wb55Error::ShortRead { what: "OTP", len: b.len() });
        }
        Ok(b)
    }

    fn fus_state(&mut self) -> Result<FusState, Wb55Error> {
        let b = self.upload(FUS_STATUS_ADDR, 2, partition::FLASH)?;
        let [status, error] = b[..] else { return Err(Wb55Error::ShortRead { what: "FUS status", len: b.len() }) };
        Ok(FusState::new(status, error))
    }
}
