//! The memory layout a DfuSe device describes in each alternate setting's interface string
//! (ST's UM0424), for example `@Internal Flash  /0x08000000/256*04Kg`: a name, a start address,
//! and banks of `count*size` pages (a multiplier ` `, `B`, `K` or `M`, then the access type `a`
//! to `g`). The C++ app reads it to know which pages to erase before a write.
//!
//! The port of `dfu/dfumemorylayout.cpp`, with the same grammar in both now (checked against the
//! C++ parser by `tests/qt_oracle.rs`). The string comes from the device, so it is refused, not
//! guessed at, when anything is off: the C++ app used to read `1*K` past the start of the string
//! (one character before the multiplier), took any number it could not read as 0, ignored `M`,
//! kept the banks it had read before an error, and walked every page of a bank one at a time, so a
//! bank of 4 billion empty pages hung it. Now: exactly three `/`-separated fields, the address as
//! `0x` and 1 to 8 hex digits, at least one bank, each with 1 to 10 digits on both sides of `*`,
//! no empty page and no empty bank, and the whole layout inside the 32-bit address space.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryLayout {
    /// The first field, trimmed of ASCII whitespace (as Qt's `trimmed()`: with `\v`).
    pub name: Vec<u8>,
    pub address: u32,
    pub banks: Vec<PageBank>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageBank {
    pub page_count: u32,
    pub page_size: u32,
    /// The access type letter, `a` to `g` (`a` readable, `b` erasable, `d` writable, and their sums).
    pub kind: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Syntax,
    Address,
    Bank(usize),
    TooLarge,
    /// `page_addresses`: the range is empty, starts before the layout or ends after it.
    Range {
        start: u32,
        end: u64,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Syntax => write!(f, "bad memory layout descriptor syntax"),
            Error::Address => write!(f, "bad address in the memory layout descriptor"),
            Error::Bank(i) => write!(f, "bad page bank {i} in the memory layout descriptor"),
            Error::TooLarge => write!(f, "the memory layout runs past the end of the address space"),
            Error::Range { start, end } => write!(f, "range 0x{start:08x}..0x{end:x} is not inside the memory layout"),
        }
    }
}

impl std::error::Error for Error {}

/// Qt's `isspace` for bytes (`QByteArray::trimmed`): space, `\t`, `\n`, `\v`, `\f`, `\r`.
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn trim(mut s: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = s {
        if !is_space(*first) {
            break;
        }
        s = rest;
    }
    while let [rest @ .., last] = s {
        if !is_space(*last) {
            break;
        }
        s = rest;
    }
    s
}

fn decimal(s: &[u8]) -> Option<u64> {
    if s.is_empty() || s.len() > 10 || !s.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(s.iter().fold(0u64, |n, d| n * 10 + u64::from(d - b'0')))
}

fn bank(s: &[u8]) -> Option<PageBank> {
    let s = trim(s);
    let star = s.iter().position(|&b| b == b'*')?;
    let (count, rest) = (&s[..star], &s[star + 1..]);
    let [digits @ .., mult, kind] = rest else { return None };
    let count = decimal(count)?;
    let size = decimal(digits)?;
    let mult: u64 = match mult {
        b' ' | b'B' => 1,
        b'K' => 1 << 10,
        b'M' => 1 << 20,
        _ => return None,
    };
    if !(b'a'..=b'g').contains(kind) {
        return None;
    }
    let size = size * mult;
    if count == 0 || size == 0 || count > u64::from(u32::MAX) || size > u64::from(u32::MAX) {
        return None;
    }
    Some(PageBank { page_count: count as u32, page_size: size as u32, kind: *kind })
}

impl MemoryLayout {
    /// Parses an interface string as the device sends it.
    pub fn parse(desc: &[u8]) -> Result<MemoryLayout, Error> {
        // libusb hands the C++ app ASCII only; a real ST descriptor is ASCII
        if !desc.is_ascii() {
            return Err(Error::Syntax);
        }
        let fields: Vec<&[u8]> = desc.split(|&b| b == b'/').collect();
        let [name, address, banks] = fields[..] else { return Err(Error::Syntax) };
        let address = trim(address);
        let hex = address.strip_prefix(b"0x").or_else(|| address.strip_prefix(b"0X")).ok_or(Error::Address)?;
        if hex.is_empty() || hex.len() > 8 || !hex.iter().all(u8::is_ascii_hexdigit) {
            return Err(Error::Address);
        }
        let address = hex.iter().fold(0u32, |n, &d| (n << 4) | (char::from(d).to_digit(16).unwrap_or(0)));
        let mut parsed = Vec::new();
        let mut total = u64::from(address);
        for (i, b) in banks.split(|&b| b == b',').enumerate() {
            let bank = bank(b).ok_or(Error::Bank(i))?;
            total += u64::from(bank.page_count) * u64::from(bank.page_size);
            if total > 1 << 32 {
                return Err(Error::TooLarge);
            }
            parsed.push(bank);
        }
        Ok(MemoryLayout { name: trim(name).to_vec(), address, banks: parsed })
    }

    /// One past the last address the layout covers.
    pub fn end(&self) -> u64 {
        u64::from(self.address) + self.banks.iter().map(|b| u64::from(b.page_count) * u64::from(b.page_size)).sum::<u64>()
    }

    /// The addresses to erase for `start..end`: `start` itself (the device erases the page that
    /// holds it), then the start of every later page that begins before `end`. Refuses an empty
    /// range and one that is not inside the layout (the C++ app erased one page for an empty
    /// element, and let a range run past the last bank unerased).
    pub fn page_addresses(&self, start: u32, end: u64) -> Result<Vec<u32>, Error> {
        if u64::from(start) >= end || start < self.address || end > self.end() {
            return Err(Error::Range { start, end });
        }
        let mut out = vec![start];
        let mut bank_start = u64::from(self.address);
        for b in &self.banks {
            let size = u64::from(b.page_size);
            let bank_end = bank_start + u64::from(b.page_count) * size;
            // page boundaries p = bank_start + k*size, k >= 1, with start < p < end and p <= bank_end
            let first_k = (u64::from(start).saturating_sub(bank_start)) / size + 1;
            let mut p = bank_start + first_k * size;
            while p <= bank_end && p < end {
                out.push(p as u32);
                p += size;
            }
            if bank_end >= end {
                break;
            }
            bank_start = bank_end;
        }
        Ok(out)
    }
}
