#![allow(dead_code)]

use std::path::{Path, PathBuf};

use argus_tools::elf::Elf;

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name)
}

pub fn read_fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixture(name)).unwrap()
}

/// File offset of a FAP's .fapmeta section, and of its section header.
pub fn fapmeta_offsets(data: &[u8]) -> (usize, usize) {
    let elf = Elf::parse(data).unwrap();
    let idx = elf.sections.iter().position(|s| s.name == ".fapmeta").unwrap();
    let shoff = u32::from_le_bytes(data[0x20..0x24].try_into().unwrap()) as usize;
    (elf.sections[idx].offset, shoff + idx * 40)
}

/// A copy of a real FAP with its manifest's API version and target changed.
pub fn with_api(data: &[u8], major: u16, minor: u16, target: u16) -> Vec<u8> {
    let mut d = data.to_vec();
    let (off, _) = fapmeta_offsets(&d);
    d[off + 8..off + 10].copy_from_slice(&minor.to_le_bytes());
    d[off + 10..off + 12].copy_from_slice(&major.to_le_bytes());
    d[off + 12..off + 14].copy_from_slice(&target.to_le_bytes());
    d
}

/// Rename an imported symbol in place (same length), as if built against another firmware.
pub fn rename_symbol(data: &[u8], from: &str, to: &str) -> Vec<u8> {
    assert_eq!(from.len(), to.len());
    let mut d = data.to_vec();
    let needle = format!("\0{from}\0");
    let pos = d.windows(needle.len()).position(|w| w == needle.as_bytes()).expect("symbol present");
    d[pos + 1..pos + 1 + to.len()].copy_from_slice(to.as_bytes());
    d
}

pub fn tar_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (name, data) in entries {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::Regular);
        // set the raw name so that hostile names ("../x") are kept as they are
        let raw = name.as_bytes();
        if raw.len() < 100 {
            h.as_old_mut().name[..raw.len()].copy_from_slice(raw);
            h.set_cksum();
            b.append(&h, *data).unwrap();
        } else {
            b.append_data(&mut h, name, *data).unwrap();
        }
    }
    b.into_inner().unwrap()
}

pub fn gzip(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// Every file below `dir` (recursively), as paths relative to it.
pub fn files_below(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let ft = e.file_type().unwrap();
            if ft.is_dir() {
                stack.push(e.path());
            } else {
                out.push(e.path().strip_prefix(dir).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}
