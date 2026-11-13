//! A local stand-in for Flipper's update server, for the repair comparison (`compare-repair.sh`):
//! `flipper-fake --write-firmware DIR` writes a `directory.json` in the server's format and the files it
//! names, so the C++ app's `FirmwareHelper` (through its test hook `ARGUS_TEST_FIRMWARE_DIR`) fetches them
//! from DIR instead of the network, and the same contents as plain files for `argus-cli recovery repair`.
//! Every byte is made up; nothing comes from Flipper's server.

use std::io;
use std::path::Path;

use md5::{Digest as _, Md5};
use sha2::Sha256;

use crate::archives::{gzip, tar, Raw};

/// Flipper's `scripts/ob.data`, as its firmware repository ships it.
pub const OB_DATA: &str = "\
RDP:0xAA:r
BOR_LEV:0x4:rw
nBOOT0:0x1:rw
nBOOT1:0x1:rw
nSWBOOT0:0x1:rw
SRAM2RST:0x0:rw
SRAM2PE:0x1:rw
nRST_STOP:0x1:rw
nRST_STDBY:0x1:rw
nRSTSHDW:0x1:rw
WWDGSW:0x1:rw
IWGDSTDBY:0x1:rw
IWDGSTOP:0x1:rw
IWDGSW:0x1:rw
IPCCDBA:0x0:rw
ESE:0x1:r
SFSA:0xD7:r
FSD:0x0:r
DDS:0x1:r
C2OPT:0x1:r
NBRSD:0x0:r
SNBRSA:0xB:r
BRSD:0x0:r
SBRSA:0x12:r
SBRV:0x35C00:r
PCROP1A_STRT:0x1FF:r
PCROP1A_END:0x0:r
PCROP_RDP:0x1:rw
PCROP1B_STRT:0x1FF:r
PCROP1B_END:0x0:r
WRP1A_STRT:0xFF:r
WRP1A_END:0x0:r
WRP1B_STRT:0xFF:r
WRP1B_END:0x0:r
";

fn pattern(n: usize, seed: usize) -> Vec<u8> {
    (0..n).map(|i| ((i * 31 + seed * 7 + i / 251) % 256) as u8).collect()
}

/// A DfuSe file: one target (alternate setting 0) with these elements.
pub fn dfuse_file(elements: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (address, data) in elements {
        body.extend(address.to_le_bytes());
        body.extend((data.len() as u32).to_le_bytes());
        body.extend(data);
    }
    let mut target = b"Target\0".to_vec();
    target.extend(1u32.to_le_bytes());
    let mut name = b"Fake firmware".to_vec();
    name.resize(255, 0);
    target.extend(name);
    target.extend((body.len() as u32).to_le_bytes());
    target.extend((elements.len() as u32).to_le_bytes());
    target.extend(body);
    let mut f = b"DfuSe\x01".to_vec();
    f.extend(((11 + target.len()) as u32).to_le_bytes());
    f.push(1);
    f.extend(target);
    f.extend([0xff, 0xff, 0x11, 0xdf, 0x83, 0x04, 0x1a, 0x01]);
    f.extend(b"UFD\x10");
    let crc = flipper_dfu::dfuse::crc32(&f);
    f.extend(crc.to_le_bytes());
    f
}

/// The firmware: two elements (the vector table page and the rest, with a gap between).
pub fn firmware() -> Vec<u8> {
    dfuse_file(&[(0x0800_0000, pattern(5000, 1)), (0x0800_3000, pattern(37 * 1024 + 11, 2))])
}

/// The wireless stack image: 3 pages and a bit, so its place below SFSA is rounded down.
pub fn radio() -> Vec<u8> {
    pattern(3 * 4096 + 5, 3)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn sha256(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

/// `core2_firmware.tgz`: `core2_firmware/Manifest.json` and the image it names.
pub fn core2_archive() -> Vec<u8> {
    core2_archive_with(false)
}

/// [`core2_archive`]; `mismatch`: the image holds other bytes than its SHA-256 in the manifest says (same size), as a
/// damaged archive whose own SHA-256 in `directory.json` is right; both apps must refuse it.
pub fn core2_archive_with(mismatch: bool) -> Vec<u8> {
    let mut image = radio();
    let manifest = format!(
        r#"{{"manifest":{{"version":0,"timestamp":1700000000}},"copro":{{"fus":{{"version":{{"major":1,"minor":2,"sub":0}},"files":[{{"name":"fus.bin","sha256":"{}","address":"0x080EC000"}}]}},"radio":{{"version":{{"type":3,"major":1,"minor":17,"sub":3,"branch":0,"release":7}},"files":[{{"name":"radio.bin","sha256":"{}","address":"0x080D3000"}}]}}}}}}"#,
        sha256(b""),
        sha256(&image)
    );
    if mismatch {
        image[100] ^= 0x01;
    }
    gzip(&tar(
        &[
            Raw::dir("core2_firmware/"),
            Raw::file("core2_firmware/Manifest.json", manifest.as_bytes()),
            Raw::file("core2_firmware/radio.bin", &image),
        ],
        true,
    ))
}

/// `scripts.tgz`: `scripts/ob.data`.
pub fn scripts_archive() -> Vec<u8> {
    gzip(&tar(&[Raw::dir("scripts/"), Raw::file("scripts/ob.data", OB_DATA.as_bytes())], true))
}

/// The resource files a repair puts on the SD card, as (path under /ext, contents).
pub fn resources() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("dolphin/manifest.txt", b"Filetype: Flipper Animation Manifest\nVersion: 1\n".to_vec()),
        ("nfc/assets/aid.nfc", pattern(700, 4)),
        ("Readme.txt", b"made-up resources\n".to_vec()),
    ]
}

/// `resources.tgz`: `resources/Manifest` (the asset manifest the app compares with the device's) and the files.
/// `mismatch`: `Readme.txt` holds other bytes than the manifest's MD5 says (same size); both apps must refuse it.
pub fn resources_archive(mismatch: bool) -> Vec<u8> {
    let mut files = resources();
    let mut manifest = String::from("V:0\nT:1700000000\nD:dolphin\nD:nfc\nD:nfc/assets\n");
    for (path, data) in &files {
        manifest.push_str(&format!("F:{}:{}:{path}\n", hex(&Md5::digest(data)), data.len()));
    }
    if mismatch {
        if let Some((_, data)) = files.iter_mut().find(|(p, _)| *p == "Readme.txt") {
            data[0] ^= 0x20;
        }
    }
    let mut entries = vec![Raw::dir("resources/"), Raw::file("resources/Manifest", manifest.as_bytes())];
    for d in ["dolphin", "nfc", "nfc/assets"] {
        entries.push(Raw::dir(&format!("resources/{d}/")));
    }
    for (path, data) in &files {
        entries.push(Raw::file(&format!("resources/{path}"), data));
    }
    gzip(&tar(&entries, true))
}

/// A region bundle in the format of Flipper's `/regions/api/v0/bundle` (made-up band names and values):
/// three countries, the default bands, and the country the server "saw" (`null` for none).
pub fn region_bundle(country: Option<&str>) -> String {
    let country = country.map_or("null".to_string(), |c| format!("\"{c}\""));
    format!(
        r#"{{"success":{{"bands":{{"BAND_A":{{"start":433050000,"end":434790000,"max_power":10,"duty_cycle":100}},"BAND_B":{{"start":868150000,"end":868550000,"max_power":14,"duty_cycle":1}},"BAND_C":{{"start":902000000,"end":928000000,"max_power":30,"duty_cycle":100}},"BAND_D":{{"start":312000000,"end":316000000,"max_power":-5,"duty_cycle":0}}}},"countries":{{"DE":["BAND_A","BAND_B"],"US":["BAND_C","BAND_D","BAND_A"],"JP":["BAND_D"]}},"country":{country},"default":["BAND_A","BAND_D"]}}}}"#
    )
}

/// Writes the firmware folder into `dir`, which must not exist: `directory.json` (one "release"
/// channel, one version, its five files: the full `.dfu` and the update package for target f7, and three for any) and the files, `bundle` (the region bundle, country
/// `DE`), plus `firmware.dfu`, `radio.bin` and `ob.data` as plain files for argus-cli. `resources_mismatch`: see [`resources_archive`] (its SHA-256 in
/// `directory.json` is right, so only the app's own check can catch it).
pub fn write(dir: &Path, resources_mismatch: bool) -> io::Result<()> {
    write_with(dir, resources_mismatch, false)
}

/// [`write`]; `core2_mismatch`: see [`core2_archive_with`].
pub fn write_with(dir: &Path, resources_mismatch: bool, core2_mismatch: bool) -> io::Result<()> {
    std::fs::create_dir(dir)?;
    let files: [(&str, &str, &str, Vec<u8>); 5] = [
        ("full_dfu", "f7", "flipper-z-f7-full-test.dfu", firmware()),
        ("update_tgz", "f7", "flipper-z-f7-update-test.tgz", crate::archives::update_package(true)),
        ("core2_firmware_tgz", "any", "flipper-z-any-core2_firmware-test.tgz", core2_archive_with(core2_mismatch)),
        ("scripts_tgz", "any", "flipper-z-any-scripts-test.tgz", scripts_archive()),
        ("resources_tgz", "any", "flipper-z-any-resources-test.tgz", resources_archive(resources_mismatch)),
    ];
    let mut list = Vec::new();
    for (kind, target, name, data) in &files {
        std::fs::write(dir.join(name), data)?;
        // the URL's file name is what the test hook reads; the host is never contacted
        list.push(format!(
            r#"{{"type":"{kind}","target":"{target}","url":"https://update.invalid/firmware/{name}","sha256":"{}"}}"#,
            sha256(data)
        ));
    }
    let directory = format!(
        r#"{{"channels":[{{"id":"release","title":"Release","description":"made-up test channel","versions":[{{"version":"9.9.9","changelog":"test","timestamp":1700000000,"files":[{}]}}]}}]}}"#,
        list.join(",")
    );
    std::fs::write(dir.join("directory.json"), directory)?;
    std::fs::write(dir.join("bundle"), region_bundle(Some("DE")))?;
    std::fs::write(dir.join("firmware.dfu"), firmware())?;
    std::fs::write(dir.join("radio.bin"), radio())?;
    std::fs::write(dir.join("ob.data"), OB_DATA)
}

/// [`FakeFlipper`](crate::FakeFlipper) with an older install of [`resources`] on its card and a valid
/// `/ext/Manifest` for it: `dolphin/manifest.txt` as now, `nfc/assets/aid.nfc` older (other bytes), no
/// `Readme.txt`, and `old/gone.txt`, which the new bundle no longer has. An install compares the two
/// manifests and sends only the differences.
pub fn with_old_resources(mut fake: crate::FakeFlipper) -> crate::FakeFlipper {
    use crate::Node;
    let new = resources();
    let old_aid = pattern(650, 9);
    let gone = b"from an older release\n".to_vec();
    let files: Vec<(&str, Vec<u8>)> =
        vec![("dolphin/manifest.txt", new[0].1.clone()), ("nfc/assets/aid.nfc", old_aid), ("old/gone.txt", gone)];
    let mut manifest = String::from("V:0\nT:1600000000\nD:dolphin\nD:nfc\nD:nfc/assets\nD:old\n");
    for (path, data) in &files {
        manifest.push_str(&format!("F:{}:{}:{path}\n", hex(&Md5::digest(data)), data.len()));
    }
    fake.nodes.retain(|n| n.path != "/ext/Manifest");
    for d in ["/ext/dolphin", "/ext/nfc/assets", "/ext/old"] {
        fake.nodes.push(Node { path: d.into(), dir: true, size: 0 });
    }
    for (path, data) in files.into_iter().chain([("Manifest", manifest.into_bytes())]) {
        let path = format!("/ext/{path}");
        fake.nodes.push(Node { path: path.clone(), dir: false, size: data.len() as u32 });
        fake.contents.push((path, data));
    }
    fake
}
