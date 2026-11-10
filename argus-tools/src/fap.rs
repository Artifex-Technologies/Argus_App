//! The FAP manifest, as the firmware's loader reads it.
//!
//! Layout (Argus_Firmware lib/flipper_application/application_manifest.h, packed, little-endian):
//!
//! | offset | size | field |
//! |---|---|---|
//! | 0  | 4  | manifest_magic = 0x52474448 ("HDGR" on disk) |
//! | 4  | 4  | manifest_version = 1 |
//! | 8  | 2  | api_version.minor |
//! | 10 | 2  | api_version.major |
//! | 12 | 2  | hardware_target_id (7 for the Flipper Zero, f7) |
//! | 14 | 2  | stack_size (0 marks a plugin, .fal) |
//! | 16 | 4  | app_version ((major << 16) \| minor, as fbt writes it) |
//! | 20 | 32 | name (NUL-padded) |
//! | 52 | 1  | has_icon |
//! | 53 | 32 | icon (10x10 1-bit bitmap) |
//!
//! Total 85 bytes. The section is called `.fapmeta`.

use serde::Serialize;

use crate::elf::Elf;
use crate::{Error, Result};

pub const MANIFEST_MAGIC: u32 = 0x5247_4448;
pub const MANIFEST_VERSION: u32 = 1;
pub const MANIFEST_V1_SIZE: usize = 85;
pub const SECTION: &str = ".fapmeta";
/// EM_ARM
pub const MACHINE_ARM: u16 = 40;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Manifest {
    pub magic_ok: bool,
    pub manifest_version: u32,
    pub api_major: u16,
    pub api_minor: u16,
    pub hardware_target: u16,
    pub stack_size: u16,
    pub app_version: u32,
    /// "major.minor" from app_version.
    pub app_version_text: String,
    pub name: String,
    pub has_icon: bool,
    /// Size of the .fapmeta section in the file. The loader requires at least 85 bytes.
    pub section_size: usize,
}

impl Manifest {
    /// What the loader's flipper_application_manifest_is_valid() checks.
    pub fn is_valid(&self) -> bool {
        self.magic_ok && self.manifest_version == MANIFEST_VERSION
    }

    pub fn is_plugin(&self) -> bool {
        self.stack_size == 0
    }

    pub fn api_text(&self) -> String {
        format!("{}.{}", self.api_major, self.api_minor)
    }
}

/// Parse a `.fapmeta` section's bytes.
pub fn parse_manifest(b: &[u8]) -> Result<Manifest> {
    if b.len() < MANIFEST_V1_SIZE {
        return Err(Error::new(
            "manifest_short",
            format!("the .fapmeta section has {} bytes; the loader needs {}", b.len(), MANIFEST_V1_SIZE),
        ));
    }
    let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let raw_name = &b[20..52];
    let end = raw_name.iter().position(|&c| c == 0).unwrap_or(raw_name.len());
    let app_version = u32_at(16);
    Ok(Manifest {
        magic_ok: u32_at(0) == MANIFEST_MAGIC,
        manifest_version: u32_at(4),
        api_minor: u16_at(8),
        api_major: u16_at(10),
        hardware_target: u16_at(12),
        stack_size: u16_at(14),
        app_version,
        app_version_text: format!("{}.{}", app_version >> 16, app_version & 0xFFFF),
        name: crate::display_text(&raw_name[..end], 32),
        has_icon: b[52] != 0,
        section_size: b.len(),
    })
}

/// Everything Argus reads from a FAP file.
#[derive(Debug, Clone, Serialize)]
pub struct FapInfo {
    pub manifest: Manifest,
    /// Symbols the app needs from the firmware's API table.
    pub imports: Vec<String>,
    pub machine: u16,
}

/// Read a FAP: its manifest and its imports. Errors are data problems (not a FAP, truncated...).
pub fn inspect(data: &[u8]) -> Result<FapInfo> {
    let elf = Elf::parse(data)?;
    let section = elf
        .section(SECTION)
        .ok_or_else(|| Error::new("manifest_missing", "the file has no .fapmeta section, so the loader rejects it"))?;
    let bytes = elf
        .section_data(section)
        .ok_or_else(|| Error::new("elf_truncated", "the .fapmeta section points outside the file"))?;
    let manifest = parse_manifest(bytes)?;
    let imports = elf.undefined_symbols()?;
    Ok(FapInfo { manifest, imports, machine: elf.machine })
}
