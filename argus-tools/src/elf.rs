//! A small, bounded ELF32 little-endian reader: just enough to find a FAP's sections and the
//! symbols it imports. Every offset and size is checked against the buffer; nothing panics on a
//! malformed file (the fuzz targets and property tests hold it to that).

use crate::{Error, Result};

const MAX_SECTIONS: usize = 1024;
const MAX_SYMBOLS: usize = 100_000;
const MAX_NAME: usize = 256;

pub const SHT_SYMTAB: u32 = 2;
pub const SHT_STRTAB: u32 = 3;

#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub kind: u32,
    pub offset: usize,
    pub size: usize,
    pub link: u32,
    pub entsize: usize,
}

#[derive(Debug)]
pub struct Elf<'a> {
    data: &'a [u8],
    pub machine: u16,
    pub sections: Vec<Section>,
}

fn u16_at(d: &[u8], off: usize) -> Result<u16> {
    let b = d.get(off..off.checked_add(2).ok_or_else(trunc)?).ok_or_else(trunc)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(d: &[u8], off: usize) -> Result<u32> {
    let b = d.get(off..off.checked_add(4).ok_or_else(trunc)?).ok_or_else(trunc)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn trunc() -> Error {
    Error::new("elf_truncated", "the ELF file is cut short or points outside itself")
}

/// The NUL-terminated string at `off` inside `table`, capped at MAX_NAME bytes.
fn str_at(table: &[u8], off: usize) -> Option<String> {
    let rest = table.get(off..)?;
    let end = rest.iter().take(MAX_NAME).position(|&b| b == 0)?;
    Some(String::from_utf8_lossy(&rest[..end]).into_owned())
}

impl<'a> Elf<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Elf<'a>> {
        if data.len() < 52 || &data[0..4] != b"\x7fELF" {
            return Err(Error::new("not_elf", "not an ELF file"));
        }
        if data[4] != 1 || data[5] != 1 {
            return Err(Error::new("elf_unsupported", "not a 32-bit little-endian ELF file (a FAP is ELF32 ARM)"));
        }
        let machine = u16_at(data, 18)?;
        let shoff = u32_at(data, 0x20)? as usize;
        let shentsize = u16_at(data, 0x2E)? as usize;
        let shnum = u16_at(data, 0x30)? as usize;
        let shstrndx = u16_at(data, 0x32)? as usize;
        if shnum == 0 {
            return Err(Error::new("elf_no_sections", "the ELF file has no section table"));
        }
        if shentsize != 40 || shnum > MAX_SECTIONS {
            return Err(Error::new("elf_bad_sections", "the ELF section table has an unexpected layout"));
        }
        let table_len = shnum.checked_mul(40).ok_or_else(trunc)?;
        let table_end = shoff.checked_add(table_len).ok_or_else(trunc)?;
        if table_end > data.len() {
            return Err(trunc());
        }

        let mut raw = Vec::with_capacity(shnum);
        for i in 0..shnum {
            let base = shoff + i * 40;
            let name_off = u32_at(data, base)? as usize;
            let kind = u32_at(data, base + 4)?;
            let offset = u32_at(data, base + 16)? as usize;
            let size = u32_at(data, base + 20)? as usize;
            let link = u32_at(data, base + 24)?;
            let entsize = u32_at(data, base + 36)? as usize;
            raw.push((name_off, kind, offset, size, link, entsize));
        }

        let names: &[u8] = match raw.get(shstrndx) {
            Some(&(_, _, off, size, _, _)) => {
                let end = off.checked_add(size).ok_or_else(trunc)?;
                data.get(off..end).ok_or_else(trunc)?
            }
            None => return Err(Error::new("elf_bad_sections", "the section-name table index is out of range")),
        };

        let sections = raw
            .into_iter()
            .map(|(name_off, kind, offset, size, link, entsize)| Section {
                name: str_at(names, name_off).unwrap_or_default(),
                kind,
                offset,
                size,
                link,
                entsize,
            })
            .collect();
        Ok(Elf { data, machine, sections })
    }

    /// The bytes of a section, or None if it has none in the file or points outside it.
    pub fn section_data(&self, s: &Section) -> Option<&'a [u8]> {
        let end = s.offset.checked_add(s.size)?;
        self.data.get(s.offset..end)
    }

    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.name == name)
    }

    /// Names of the symbols the file needs from outside (undefined, global or weak), sorted and unique.
    pub fn undefined_symbols(&self) -> Result<Vec<String>> {
        let Some(symtab) = self.sections.iter().find(|s| s.kind == SHT_SYMTAB) else {
            return Ok(Vec::new());
        };
        if symtab.entsize != 16 {
            return Err(Error::new("elf_bad_symtab", "the symbol table has an unexpected entry size"));
        }
        let syms = self.section_data(symtab).ok_or_else(trunc)?;
        let strtab = self
            .sections
            .get(symtab.link as usize)
            .and_then(|s| self.section_data(s))
            .ok_or_else(trunc)?;
        let count = syms.len() / 16;
        if count > MAX_SYMBOLS {
            return Err(Error::new("elf_bad_symtab", "the symbol table is implausibly large"));
        }
        let mut out = Vec::new();
        for i in 1..count {
            let e = &syms[i * 16..i * 16 + 16];
            let name_off = u32::from_le_bytes([e[0], e[1], e[2], e[3]]) as usize;
            let info = e[12];
            let shndx = u16::from_le_bytes([e[14], e[15]]);
            let bind = info >> 4;
            if shndx == 0 && (bind == 1 || bind == 2) {
                if let Some(name) = str_at(strtab, name_off) {
                    if !name.is_empty() {
                        out.push(name);
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }
}
