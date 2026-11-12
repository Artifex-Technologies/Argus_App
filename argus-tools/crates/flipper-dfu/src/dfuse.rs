//! DfuSe files (ST's UM0391), the format of the Flipper's firmware for recovery (`.dfu`): a prefix,
//! one or more targets (one per alternate setting, each a list of elements: an address and the
//! bytes for it), and the standard DFU suffix with a CRC over everything before it.
//!
//! The port of the C++ app's `dfu/dfusefile.cpp`. Both now refuse the same files (checked against
//! the C++ parser itself by `tests/qt_oracle.rs`): every length is checked against the bytes that
//! are there before anything is read or allocated, so a count of four billion elements or an
//! element larger than the file costs nothing. A file is refused when
//!
//! - it is over [`MAX_FILE`] bytes, or the prefix's image size is not the file's size minus the
//!   suffix, or its version is not 1;
//! - it has no target, or a target's size is not the sum of its elements, or anything follows the
//!   last target before the suffix;
//! - an element is empty (the C++ app erased a whole page for it and wrote nothing back), runs past
//!   the end of the 32-bit address space, or overlaps another element for the same alternate
//!   setting (which of the two the device kept would depend on the order of writes);
//! - the suffix's signature or length is wrong, or the CRC does not match.

use std::fmt;

/// The largest file read. The Flipper's flash is 1 MiB; a firmware file is a little over that.
pub const MAX_FILE: usize = 16 << 20;
/// `"DfuSe"`, `bVersion`, `DFUImageSize`, `bTargets`.
pub const PREFIX_LEN: usize = 11;
/// `"Target"`, `bAlternateSetting`, `bTargetNamed`, `szTargetName[255]`, `dwTargetSize`, `dwNbElements`.
pub const TARGET_PREFIX_LEN: usize = 274;
/// `dwElementAddress`, `dwElementSize`.
pub const ELEMENT_HEADER_LEN: usize = 8;
/// `bcdDevice`, `idProduct`, `idVendor`, `bcdDFU`, `"UFD"`, `bLength`, `dwCRC`.
pub const SUFFIX_LEN: usize = 16;

/// A parsed file. Element data is copied out (the file is at most [`MAX_FILE`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DfuseFile {
    pub version: u8,
    pub targets: Vec<Target>,
    pub suffix: Suffix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Suffix {
    pub bcd_device: u16,
    pub id_product: u16,
    pub id_vendor: u16,
    pub bcd_dfu: u16,
    pub crc: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub alt: u8,
    /// `bTargetNamed`, a 32-bit field (nonzero: the name is meaningful).
    pub named: u32,
    /// `szTargetName`, all 255 bytes as they are in the file.
    pub name: Vec<u8>,
    /// `dwTargetSize`: the bytes of all elements with their headers.
    pub size: u32,
    pub elements: Vec<Element>,
}

impl Target {
    /// The name up to its first NUL.
    pub fn name_bytes(&self) -> &[u8] {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(self.name.len());
        &self.name[..end]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    pub address: u32,
    pub data: Vec<u8>,
}

impl Element {
    /// One past the last address (never wraps: checked by [`parse`]).
    pub fn end(&self) -> u64 {
        u64::from(self.address) + self.data.len() as u64
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    TooLarge(usize),
    TooShort,
    Signature,
    Version(u8),
    ImageSize { prefix: u32, file: usize },
    NoTargets,
    TargetCut(usize),
    TargetSignature(usize),
    TooManyElements { target: usize, count: u32 },
    ElementCut { target: usize, element: usize },
    EmptyElement { target: usize, element: usize },
    AddressOverflow { target: usize, element: usize },
    TargetSize { target: usize, prefix: u32, actual: u64 },
    TrailingBytes(usize),
    Overlap { alt: u8, first: u32, second: u32 },
    Suffix,
    Crc { file: u32, computed: u32 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::TooLarge(n) => write!(f, "file is {n} bytes, over the {MAX_FILE}-byte limit"),
            Error::TooShort => write!(f, "file is too short for a DfuSe prefix and suffix"),
            Error::Signature => write!(f, "not a DfuSe file (no \"DfuSe\" signature)"),
            Error::Version(v) => write!(f, "unknown DfuSe version {v}"),
            Error::ImageSize { prefix, file } => write!(f, "prefix says the image is {prefix} bytes; the file holds {file}"),
            Error::NoTargets => write!(f, "the file has no target"),
            Error::TargetCut(t) => write!(f, "target {t} is cut off"),
            Error::TargetSignature(t) => write!(f, "target {t} has no \"Target\" signature"),
            Error::TooManyElements { target, count } => write!(f, "target {target} claims {count} elements, more than the file can hold"),
            Error::ElementCut { target, element } => write!(f, "element {element} of target {target} is cut off"),
            Error::EmptyElement { target, element } => write!(f, "element {element} of target {target} is empty"),
            Error::AddressOverflow { target, element } => {
                write!(f, "element {element} of target {target} runs past the end of the address space")
            }
            Error::TargetSize { target, prefix, actual } => {
                write!(f, "target {target} says it is {prefix} bytes; its elements are {actual}")
            }
            Error::TrailingBytes(n) => write!(f, "{n} unexpected bytes after the last target"),
            Error::Overlap { alt, first, second } => {
                write!(f, "elements at 0x{first:08x} and 0x{second:08x} overlap (alternate setting {alt})")
            }
            Error::Suffix => write!(f, "no valid DFU suffix"),
            Error::Crc { file, computed } => write!(f, "checksum does not match (file 0x{file:08x}, computed 0x{computed:08x})"),
        }
    }
}

impl std::error::Error for Error {}

/// The DFU suffix's CRC: CRC-32 (reflected, polynomial 0xEDB88320) starting at all ones, with no
/// final inversion, over every byte of the file but the last four.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
        }
    }
    crc
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Parses a whole file. Never reads past `bytes`, never allocates more than `bytes.len()` in
/// element data.
pub fn parse(bytes: &[u8]) -> Result<DfuseFile, Error> {
    if bytes.len() > MAX_FILE {
        return Err(Error::TooLarge(bytes.len()));
    }
    if bytes.len() < PREFIX_LEN + SUFFIX_LEN {
        return Err(Error::TooShort);
    }
    if &bytes[..5] != b"DfuSe" {
        return Err(Error::Signature);
    }
    let version = bytes[5];
    if version != 1 {
        return Err(Error::Version(version));
    }
    let image_size = u32_at(bytes, 6);
    let image_end = bytes.len() - SUFFIX_LEN;
    if image_size as usize != image_end {
        return Err(Error::ImageSize { prefix: image_size, file: image_end });
    }
    let count = bytes[10];
    if count == 0 {
        return Err(Error::NoTargets);
    }

    let mut at = PREFIX_LEN;
    let mut targets = Vec::with_capacity(count as usize);
    for t in 0..count as usize {
        if image_end - at < TARGET_PREFIX_LEN {
            return Err(Error::TargetCut(t));
        }
        let p = &bytes[at..at + TARGET_PREFIX_LEN];
        if &p[..6] != b"Target" {
            return Err(Error::TargetSignature(t));
        }
        let alt = p[6];
        let named = u32_at(p, 7);
        let name = p[11..266].to_vec();
        let size = u32_at(p, 266);
        let n = u32_at(p, 270);
        at += TARGET_PREFIX_LEN;
        if n as usize > (image_end - at) / ELEMENT_HEADER_LEN {
            return Err(Error::TooManyElements { target: t, count: n });
        }
        let start = at;
        let mut elements = Vec::with_capacity(n as usize);
        for e in 0..n as usize {
            if image_end - at < ELEMENT_HEADER_LEN {
                return Err(Error::ElementCut { target: t, element: e });
            }
            let address = u32_at(bytes, at);
            let len = u32_at(bytes, at + 4) as usize;
            at += ELEMENT_HEADER_LEN;
            if len > image_end - at {
                return Err(Error::ElementCut { target: t, element: e });
            }
            if len == 0 {
                return Err(Error::EmptyElement { target: t, element: e });
            }
            if u64::from(address) + len as u64 > 1 << 32 {
                return Err(Error::AddressOverflow { target: t, element: e });
            }
            elements.push(Element { address, data: bytes[at..at + len].to_vec() });
            at += len;
        }
        let actual = (at - start) as u64;
        if u64::from(size) != actual {
            return Err(Error::TargetSize { target: t, prefix: size, actual });
        }
        targets.push(Target { alt, named, name, size, elements });
    }
    if at != image_end {
        return Err(Error::TrailingBytes(image_end - at));
    }

    let s = &bytes[image_end..];
    if &s[8..11] != b"UFD" || s[11] as usize != SUFFIX_LEN {
        return Err(Error::Suffix);
    }
    let suffix =
        Suffix { bcd_device: u16_at(s, 0), id_product: u16_at(s, 2), id_vendor: u16_at(s, 4), bcd_dfu: u16_at(s, 6), crc: u32_at(s, 12) };
    let computed = crc32(&bytes[..bytes.len() - 4]);
    if computed != suffix.crc {
        return Err(Error::Crc { file: suffix.crc, computed });
    }

    check_overlaps(&targets)?;
    Ok(DfuseFile { version, targets, suffix })
}

fn check_overlaps(targets: &[Target]) -> Result<(), Error> {
    let mut spans: Vec<(u8, u32, u64)> = targets.iter().flat_map(|t| t.elements.iter().map(move |e| (t.alt, e.address, e.end()))).collect();
    spans.sort_unstable();
    for w in spans.windows(2) {
        let ((alt_a, start_a, end_a), (alt_b, start_b, _)) = (w[0], w[1]);
        if alt_a == alt_b && u64::from(start_b) < end_a {
            return Err(Error::Overlap { alt: alt_a, first: start_a, second: start_b });
        }
    }
    Ok(())
}
