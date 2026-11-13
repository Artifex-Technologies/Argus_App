//! The files of a repair from the update server: the C++ app's `FirmwareHelper` (with `RadioManifestHelper` and
//! `ScriptsHelper`) for `FullRepairOperation`. From the latest version of a channel: the firmware (`full_dfu` for
//! target f7), the wireless stack (`core2_firmware_tgz`: its `Manifest.json` names the image), the option bytes
//! (`scripts_tgz`: `scripts/ob.data`) and the resources for the SD card (`resources_tgz`, f7 or any). Each is fetched
//! with [`Client::get_file`] (its SHA-256 from the directory checked) and read whole before anything is opened.
//!
//! The radio manifest is read as Qt reads it (`RadioManifest`): the same keys are required, numbers are Qt's
//! `toInt()`, and the image is the manifest's file whose name sorts first (a `QMap` keyed by name). One rule is new in
//! both apps (2026-11-13): the image must be in the archive and match the manifest's SHA-256; the C++ app flashed
//! whatever bytes the archive held under that name (only the archive as a whole was checked, against the directory).
//!
//! Nothing here talks to a device.

use std::collections::BTreeMap;

use qjson::{Object, Value};
use sha2::{Digest, Sha256};

use crate::directory::{self, Directory};
use crate::fetch::{Client, MAX_FILE};

/// The largest wireless stack or scripts archive unpacked (Flipper's are well under 1 MiB).
const MAX_SMALL_TAR: u64 = 16 << 20;
const MAX_ENTRIES: usize = 64;

/// The wireless stack image of a `core2_firmware.tgz` and what its manifest says about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadioImage {
    pub data: Vec<u8>,
    /// "major.minor.sub", as the C++ app formats it.
    pub version: String,
    /// The stack type (`copro.radio.version.type`).
    pub stack_type: i32,
    pub file: String,
}

fn object<'a>(v: &'a Value, what: &str) -> Result<&'a Object, String> {
    let o = v.as_object().ok_or_else(|| format!("Expected {what} to be an object"))?;
    if o.entries().is_empty() {
        return Err(format!("{what} is empty"));
    }
    Ok(o)
}

fn has_all(o: &Object, keys: &[&str]) -> bool {
    keys.iter().all(|k| o.contains(k))
}

fn get<'a>(o: &'a Object, key: &str) -> &'a Value {
    o.get(key).unwrap_or(&Value::Null)
}

/// `RadioManifest::Section`: the version "major.minor.sub" and the files (name to SHA-256, in Qt's map order).
fn section(v: &Value) -> Result<(String, BTreeMap<Vec<u16>, Value>), String> {
    let o = object(v, "Section")?;
    if !has_all(o, &["version", "files"]) {
        return Err("Malformed Section".into());
    }
    let ver = object(get(o, "version"), "Section version")?;
    if !has_all(ver, &["major", "minor", "sub"]) {
        return Err("Malformed section version".into());
    }
    let n = |k: &str| get(ver, k).to_int(0);
    let version = format!("{}.{}.{}", n("major"), n("minor"), n("sub"));
    let Value::Array(files) = get(o, "files") else {
        return Err("Expected File list to be an array".into());
    };
    if files.is_empty() {
        return Err("File list is empty".into());
    }
    let mut map = BTreeMap::new();
    for f in files {
        let fo = object(f, "FileInfo")?;
        if !has_all(fo, &["name", "sha256", "address"]) {
            return Err("Malformed FileInfo".into());
        }
        if let Some(c) = fo.get("condition") {
            let c = c.to_text();
            if !c.starts_with("==") && !c.starts_with('>') {
                return Err("Malformed Condition".into());
            }
        }
        // a QMap keyed by the name: a repeated name keeps the later entry
        map.insert(get(fo, "name").to_qstring(), get(fo, "sha256").clone());
    }
    Ok((version, map))
}

/// Reads `core2_firmware.tgz` (`RadioManifestHelper`): `core2_firmware/Manifest.json` and the image it names.
pub fn radio_image(core2_tgz: &[u8]) -> Result<RadioImage, String> {
    let files = flipper_rpc::restore::read_tgz_files(core2_tgz, MAX_SMALL_TAR, MAX_ENTRIES)
        .map_err(|e| format!("Failed to uncompress archive file: {}", e.0))?;
    let text = files.get("core2_firmware/Manifest.json").map(Vec::as_slice).unwrap_or_default();
    let bad = |m: &str| format!("Failed to read radio manifest: {m}");
    if text.is_empty() {
        return Err(bad("JSON text is empty"));
    }
    let doc = qjson::from_json(text).map_err(|_| bad("Expected RadioManifest to be a JSON object"))?;
    let obj = doc.as_object().ok_or_else(|| bad("Expected RadioManifest to be a JSON object"))?;
    if !has_all(obj, &["manifest", "copro"]) {
        return Err(bad("Malformed RadioManifest"));
    }
    let header = object(get(obj, "manifest"), "Manifest header").map_err(|e| bad(&e))?;
    if !header.contains("version") {
        return Err(bad("Failed to read manifest version"));
    }
    if !header.contains("timestamp") {
        return Err(bad("Failed to read manifest timestamp"));
    }
    let copro = object(get(obj, "copro"), "FirmwareInfo").map_err(|e| bad(&e))?;
    if !has_all(copro, &["fus", "radio"]) {
        return Err(bad("Malformed FirmwareInfo"));
    }
    section(get(copro, "fus")).map_err(|e| bad(&e))?;
    let (version, radio_files) = section(get(copro, "radio")).map_err(|e| bad(&e))?;
    let ver = get(copro, "radio").as_object().map(|o| get(o, "version")).and_then(Value::as_object).expect("checked by section()");
    if !has_all(ver, &["type", "branch", "release"]) {
        return Err(bad("Malformed section version (RadioSection)"));
    }
    let stack_type = get(ver, "type").to_int(0);
    let (name, sha) = radio_files.iter().next().expect("checked by section()");
    let file = String::from_utf16_lossy(name);
    // new in both apps: the image must be there and be what the manifest says
    let data = files
        .get(&format!("core2_firmware/{file}"))
        .filter(|d| !d.is_empty())
        .ok_or_else(|| format!("the wireless stack archive lacks {file:?}, which its manifest names"))?;
    let want =
        directory::checked_sha256(&sha.to_text()).ok_or_else(|| format!("the wireless stack manifest gives no SHA-256 for {file:?}"))?;
    let got = hex::encode(Sha256::digest(data));
    if got != want {
        return Err(format!("the wireless stack image {file:?} does not match its manifest (SHA-256 {got}, the manifest says {want})"));
    }
    Ok(RadioImage { data: data.clone(), version, stack_type, file })
}

/// Reads `scripts.tgz` (`ScriptsHelper`): `scripts/ob.data`.
pub fn option_bytes(scripts_tgz: &[u8]) -> Result<Vec<u8>, String> {
    let files = flipper_rpc::restore::read_tgz_files(scripts_tgz, MAX_SMALL_TAR, MAX_ENTRIES)
        .map_err(|e| format!("Failed to uncompress archive file: {}", e.0))?;
    files.get("scripts/ob.data").filter(|d| !d.is_empty()).cloned().ok_or_else(|| "the scripts archive has no scripts/ob.data".into())
}

/// Everything a repair from the server needs, fetched and read.
#[derive(Clone, Debug)]
pub struct RepairFiles {
    /// The version the files are of.
    pub version: String,
    pub firmware: Vec<u8>,
    pub radio: RadioImage,
    pub option_bytes: Vec<u8>,
    /// `resources.tgz`, as fetched (read with `flipper_rpc::assets::read_bundle`).
    pub resources: Vec<u8>,
}

/// The device's hardware target (Flipper Zero is target 7).
pub const TARGET: &str = "f7";

/// `FirmwareHelper`: the files of the latest version of `channel` in `dir`, fetched with `client` in the C++ app's
/// order (firmware, wireless stack, scripts, resources), each read before the next is fetched.
pub fn fetch(client: &Client, dir: &Directory, channel: &str) -> Result<RepairFiles, String> {
    let version = dir.channel(channel).and_then(|c| c.latest()).ok_or_else(|| format!("the update server has no {channel} version"))?;
    let need = |kind: &str, target: &str| {
        version.file(kind, target).ok_or_else(|| format!("version {}: File info invalid (missing target?): {kind}", version.number))
    };
    let get = |f| client.get_file(f, MAX_FILE).map_err(|e| format!("Failed to fetch file: {e}"));
    let firmware = get(need("full_dfu", TARGET)?)?;
    let radio = radio_image(&get(need("core2_firmware_tgz", "any")?)?)?;
    let option_bytes = option_bytes(&get(need("scripts_tgz", "any")?)?)?;
    let resources_file = version.file("resources_tgz", TARGET).map_or_else(|| need("resources_tgz", "any"), Ok)?;
    let resources = get(resources_file)?;
    Ok(RepairFiles { version: version.number.clone(), firmware, radio, option_bytes, resources })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_wireless_stack_and_the_option_bytes() {
        let r = radio_image(&flipper_fake::firmware::core2_archive()).unwrap();
        assert_eq!((r.version.as_str(), r.stack_type, r.file.as_str()), ("1.17.3", 3, "radio.bin"));
        assert_eq!(r.data, flipper_fake::firmware::radio());
        assert_eq!(option_bytes(&flipper_fake::firmware::scripts_archive()).unwrap(), flipper_fake::firmware::OB_DATA.as_bytes());
    }

    #[test]
    fn an_image_that_is_not_its_manifests_is_refused() {
        let e = radio_image(&flipper_fake::firmware::core2_archive_with(true)).unwrap_err();
        assert!(e.contains("does not match its manifest"), "{e}");
    }
}
