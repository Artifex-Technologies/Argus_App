//! Will the firmware load this app? The rule, from Argus_Firmware
//! lib/flipper_application/flipper_application.c (flipper_application_validate_manifest) and
//! application_manifest.c, in the order the loader applies it:
//!
//! 1. The manifest must be valid: magic 0x52474448 and manifest_version 1.
//! 2. hardware_target_id must equal the firmware's target (7 on the Flipper Zero).
//! 3. The API **major** must equal the firmware's: a smaller one is "API too old", a larger one
//!    "API too new". The minor is **not** compared there (that check is commented out in the
//!    firmware source).
//! 4. At load time every imported symbol is looked up in the firmware's exported API table; one
//!    missing symbol fails the load. This is what a newer minor usually breaks, and also what
//!    breaks apps built for a fork (Momentum, Unleashed...) that exports extra symbols under the
//!    same version number.
//!
//! So with the exported table of the device's API version at hand (tables are built in for the
//! versions in `data/`), Argus answers 1-4 exactly. Without it, it answers 1-3 and calls an app
//! whose minor is newer than the firmware's "unknown".
//!
//! One more check guards the device: the loader reads the whole `.fapmeta` section into an
//! 85-byte struct (flipper_application_process_manifest_section reads `size` bytes, not
//! `sizeof`), so a larger section overflows firmware memory. Such a file is refused outright,
//! even when the user forces an install.

use std::collections::HashSet;

use serde::Serialize;

use crate::fap::{FapInfo, MANIFEST_V1_SIZE};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FirmwareApi {
    pub major: u16,
    pub minor: u16,
    pub target: u16,
}

impl FirmwareApi {
    /// "88.2" or "88.2/7" (target defaults to 7).
    pub fn parse(s: &str) -> Option<FirmwareApi> {
        let (ver, target) = match s.split_once('/') {
            Some((v, t)) => (v, t.trim_start_matches('f').parse().ok()?),
            None => (s, 7),
        };
        let (maj, min) = ver.split_once('.')?;
        Some(FirmwareApi { major: maj.trim().parse().ok()?, minor: min.trim().parse().ok()?, target })
    }

    pub fn text(&self) -> String {
        format!("{}.{}", self.major, self.minor)
    }
}

pub struct SymbolTable {
    pub api: FirmwareApi,
    names: HashSet<&'static str>,
}

impl SymbolTable {
    fn from_text(text: &'static str, target: u16) -> Option<SymbolTable> {
        let mut api = None;
        let mut names = HashSet::new();
        for line in text.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            if let Some(v) = line.strip_prefix("version ") {
                api = FirmwareApi::parse(&format!("{v}/{target}"));
            } else {
                names.insert(line);
            }
        }
        Some(SymbolTable { api: api?, names })
    }

    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

const TABLES: &[(&str, u16, &str)] = &[("88.2", 7, include_str!("../data/api_symbols_f7_88.2.txt"))];

/// The exported table for exactly this API version and target, if Argus carries one.
pub fn builtin_table(api: &FirmwareApi) -> Option<SymbolTable> {
    TABLES.iter().find(|(v, t, _)| *v == api.text() && *t == api.target).and_then(|(_, t, text)| SymbolTable::from_text(text, *t))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Compatible,
    Incompatible,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct Compat {
    pub status: Status,
    /// Stable code: ok, invalid_manifest, target_mismatch, api_too_old, api_too_new,
    /// missing_symbols, plugin_host_symbols, minor_newer_unchecked, manifest_oversized, not_a_fap.
    pub code: &'static str,
    pub reason: String,
    /// true when no "force" may override (the file would harm the loader).
    pub blocked: bool,
    pub firmware_api: String,
    pub app_api: String,
    /// Up to 20 imported symbols the firmware does not export.
    pub missing_symbols: Vec<String>,
    pub missing_count: usize,
    /// Whether the exported-symbol table was used (an exact answer).
    pub symbols_checked: bool,
}

pub fn check(info: &FapInfo, fw: &FirmwareApi, table: Option<&SymbolTable>) -> Compat {
    let m = &info.manifest;
    let mk = |status, code, reason: String| Compat {
        status,
        code,
        reason,
        blocked: false,
        firmware_api: fw.text(),
        app_api: m.api_text(),
        missing_symbols: Vec::new(),
        missing_count: 0,
        symbols_checked: false,
    };

    if m.section_size > MANIFEST_V1_SIZE {
        let mut c = mk(
            Status::Incompatible,
            "manifest_oversized",
            format!(
                "the .fapmeta section is {} bytes; the firmware's loader copies all of it into an {}-byte buffer, so loading it would overwrite device memory",
                m.section_size, MANIFEST_V1_SIZE
            ),
        );
        c.blocked = true;
        return c;
    }
    if !m.is_valid() {
        return mk(Status::Incompatible, "invalid_manifest", "the manifest's magic or version is wrong; the loader rejects it".into());
    }
    if m.hardware_target != fw.target {
        return mk(
            Status::Incompatible,
            "target_mismatch",
            format!("built for hardware target {}, the device is target {}", m.hardware_target, fw.target),
        );
    }
    if m.api_major < fw.major {
        return mk(
            Status::Incompatible,
            "api_too_old",
            format!(
                "built for API {} (major {}); the firmware is API {} and needs major {}",
                m.api_text(),
                m.api_major,
                fw.text(),
                fw.major
            ),
        );
    }
    if m.api_major > fw.major {
        return mk(
            Status::Incompatible,
            "api_too_new",
            format!(
                "built for API {} (major {}); the firmware is API {} and needs major {}",
                m.api_text(),
                m.api_major,
                fw.text(),
                fw.major
            ),
        );
    }

    if let Some(t) = table.filter(|t| t.api == *fw) {
        let missing: Vec<&String> = info.imports.iter().filter(|s| !t.contains(s)).collect();
        if missing.is_empty() {
            let mut c = mk(
                Status::Compatible,
                "ok",
                format!(
                    "API major {} matches and all {} imported symbols are exported by API {}",
                    m.api_major,
                    info.imports.len(),
                    fw.text()
                ),
            );
            c.symbols_checked = true;
            return c;
        }
        if m.is_plugin() {
            // A plugin (.fal) also links against its host app's own API (the JS engine, the NFC
            // app...), which no firmware table lists.
            let mut c = mk(
                Status::Unknown,
                "plugin_host_symbols",
                format!("a plugin: {} of its imported symbols are not firmware exports and must come from its host app", missing.len()),
            );
            c.missing_count = missing.len();
            c.missing_symbols = missing.into_iter().take(20).cloned().collect();
            c.symbols_checked = true;
            return c;
        }
        let mut c = mk(
            Status::Incompatible,
            "missing_symbols",
            format!(
                "API major matches, but {} of its {} imported symbols are not exported by API {} (built for a newer minor or for another firmware)",
                missing.len(),
                info.imports.len(),
                fw.text()
            ),
        );
        c.missing_count = missing.len();
        c.missing_symbols = missing.into_iter().take(20).cloned().collect();
        c.symbols_checked = true;
        return c;
    }

    if m.api_minor <= fw.minor {
        mk(
            Status::Compatible,
            "ok",
            format!("API major {} matches and its minor {} is not newer than the firmware's {}", m.api_major, m.api_minor, fw.minor),
        )
    } else {
        mk(
            Status::Unknown,
            "minor_newer_unchecked",
            format!(
                "API major matches, but it was built for minor {} and the firmware has {}: it loads only if every symbol it uses exists, and Argus has no symbol table for API {}",
                m.api_minor,
                fw.minor,
                fw.text()
            ),
        )
    }
}

/// The verdict for a file that is not a readable FAP at all.
pub fn not_a_fap(fw: &FirmwareApi, why: &str) -> Compat {
    Compat {
        status: Status::Incompatible,
        code: "not_a_fap",
        reason: format!("not a loadable FAP: {why}"),
        blocked: true,
        firmware_api: fw.text(),
        app_api: String::new(),
        missing_symbols: Vec::new(),
        missing_count: 0,
        symbols_checked: false,
    }
}
