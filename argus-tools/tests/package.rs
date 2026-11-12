mod common;

use std::io::Write;

use argus_tools::api::{FirmwareApi, Status};
use argus_tools::heatshrink::{self, Params};
use argus_tools::package::{self, Limits};
use common::*;

const FW: FirmwareApi = FirmwareApi { major: 88, minor: 2, target: 7 };

fn scan_bytes(name: &str, data: &[u8], limits: Limits) -> (tempfile::TempDir, argus_tools::Result<package::Report>) {
    let root = tempfile::tempdir().unwrap();
    let pkg = root.path().join(name);
    std::fs::write(&pkg, data).unwrap();
    let out = root.path().join("out");
    let r = package::scan(&pkg, &out, limits, Some(FW));
    (root, r)
}

/// Nothing but the package itself and `<out>/<sha256>.fap` files may exist afterwards.
fn assert_contained(root: &std::path::Path, pkg: &str) {
    for f in files_below(root) {
        let ok = f == pkg || (f.starts_with("out/") && f.ends_with(".fap") && f.len() == "out/".len() + 64 + 4);
        assert!(ok, "unexpected file written: {f}");
    }
}

#[test]
fn heatshrink_decodes_the_c_encoder_output() {
    use std::io::Read;
    let ths = read_fixture("resources-c.ths");
    let p = heatshrink::parse_header(&ths).unwrap();
    assert_eq!(p, Params { window_bits: 13, lookahead_bits: 6 });
    let mut out = Vec::new();
    heatshrink::Decoder::new(&ths[7..], p).read_to_end(&mut out).unwrap();
    assert_eq!(out, read_fixture("resources-plain.tar"));
}

#[test]
fn heatshrink_round_trips() {
    use std::io::Read;
    let data: Vec<u8> = (0..20_000u32).map(|i| ((i * 7) % 251) as u8 ^ if i % 97 < 40 { 0 } else { 0x55 }).collect();
    for p in [
        Params { window_bits: 8, lookahead_bits: 4 },
        Params { window_bits: 10, lookahead_bits: 5 },
        Params { window_bits: 4, lookahead_bits: 3 },
    ] {
        let enc = heatshrink::encode(&data, p);
        let mut dec = Vec::new();
        heatshrink::Decoder::new(enc.as_slice(), p).read_to_end(&mut dec).unwrap();
        assert_eq!(dec, data, "{p:?}");
    }
}

#[test]
fn real_style_update_package() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("out");
    let r = package::scan(&fixture("update-f7-test.tgz"), &out, Limits::default(), Some(FW)).unwrap();
    assert_eq!(r.format, "gzip");
    assert!(!r.incomplete, "{:?}", r.warnings);
    let mut apps: Vec<_> = r.apps.iter().map(|a| (a.category.as_str(), a.file_name.as_str())).collect();
    apps.sort();
    assert_eq!(apps, vec![("Games", "snake_game.fap"), ("Sub-GHz", "subghz_jammer_detect.fap")]);
    let snake = r.apps.iter().find(|a| a.file_name == "snake_game.fap").unwrap();
    assert_eq!(snake.manifest.as_ref().unwrap().name, "Snake Game");
    assert_eq!(snake.compat.as_ref().unwrap().status, Status::Compatible);
    assert_eq!(snake.sha256, argus_tools::sha256_hex(&read_fixture("snake_game.fap")));
    assert_eq!(std::fs::read(&snake.stored).unwrap(), read_fixture("snake_game.fap"));
    assert!(snake.package_path.contains("resources.ths > apps/Games/snake_game.fap"), "{}", snake.package_path);
}

#[test]
fn zip_and_plain_tar_and_single_fap() {
    let snake = read_fixture("snake_game.fap");
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default();
    z.start_file("Momentum/apps/Games/snake_game.fap", o).unwrap();
    z.write_all(&snake).unwrap();
    z.start_file("Momentum/f7-update.tgz", o).unwrap();
    z.write_all(&read_fixture("update-f7-test.tgz")).unwrap();
    let zbytes = z.finish().unwrap().into_inner();
    let (_d, r) = scan_bytes("pkg.zip", &zbytes, Limits::default());
    let r = r.unwrap();
    assert_eq!(r.format, "zip");
    assert_eq!(r.apps.len(), 3, "{:?}", r.skipped);

    let (_d, r) = scan_bytes("res.tar", &read_fixture("resources-plain.tar"), Limits::default());
    assert_eq!(r.unwrap().apps.len(), 2);

    let (_d, r) = scan_bytes("snake_game.fap", &snake, Limits::default());
    let r = r.unwrap();
    assert_eq!((r.apps.len(), r.apps[0].category.as_str()), (1, "Misc"));
}

#[test]
fn path_traversal_names_never_become_paths() {
    let snake = read_fixture("snake_game.fap");
    let tar = tar_of(&[("../../../../tmp/evil.fap", &snake), ("/etc/evil2.fap", &snake), ("apps/../../x/evil3.fap", &snake)]);
    let (d, r) = scan_bytes("evil.tar", &tar, Limits::default());
    let r = r.unwrap();
    assert_eq!(r.apps.len(), 3);
    for a in &r.apps {
        assert!(a.path_suspicious);
        assert_eq!(a.category, "Misc");
        assert!(a.stored.starts_with(d.path().join("out")));
    }
    assert_contained(d.path(), "evil.tar");
    assert!(!std::path::Path::new("/tmp/evil.fap").exists() || std::fs::read("/tmp/evil.fap").unwrap() != snake);
}

#[test]
fn links_and_specials_are_skipped() {
    let mut b = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(tar::EntryType::Symlink);
    h.set_size(0);
    b.append_link(&mut h, "apps/Games/link.fap", "/etc/passwd").unwrap();
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(tar::EntryType::Link);
    h.set_size(0);
    b.append_link(&mut h, "apps/Games/hard.fap", "apps/Games/link.fap").unwrap();
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(tar::EntryType::Char);
    h.set_size(0);
    h.set_path("apps/Games/dev.fap").unwrap();
    h.set_cksum();
    b.append(&h, std::io::empty()).unwrap();
    let tar = b.into_inner().unwrap();
    let (d, r) = scan_bytes("links.tar", &tar, Limits::default());
    let r = r.unwrap();
    assert!(r.apps.is_empty());
    assert_eq!(r.skipped.len(), 3, "{:?}", r.skipped);
    assert_contained(d.path(), "links.tar");

    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    z.add_symlink("apps/Games/link.fap", "/etc/passwd", zip::write::SimpleFileOptions::default()).unwrap();
    let zbytes = z.finish().unwrap().into_inner();
    let (d, r) = scan_bytes("links.zip", &zbytes, Limits::default());
    let r = r.unwrap();
    assert!(r.apps.is_empty() && r.skipped.len() == 1);
    assert_contained(d.path(), "links.zip");
}

#[test]
fn gzip_bomb_stops_at_the_budget() {
    let zeros = vec![0u8; 64 << 20];
    let tar = tar_of(&[("apps/Games/big.bin", &zeros)]);
    let bomb = gzip(&tar);
    assert!(bomb.len() < 1 << 20);
    let limits = Limits { total_bytes: 8 << 20, ..Limits::default() };
    let (d, r) = scan_bytes("bomb.tgz", &bomb, limits);
    let r = r.unwrap();
    assert!(r.incomplete);
    assert!(r.warnings[0].contains("decompresses to more than"));
    assert_contained(d.path(), "bomb.tgz");
}

#[test]
fn zip_bomb_and_huge_app_are_refused() {
    let zeros = vec![0u8; 40 << 20];
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    z.start_file("apps/Games/huge.fap", o).unwrap();
    z.write_all(&zeros).unwrap();
    z.start_file("apps/Games/snake_game.fap", o).unwrap();
    z.write_all(&read_fixture("snake_game.fap")).unwrap();
    let zbytes = z.finish().unwrap().into_inner();
    assert!(zbytes.len() < 1 << 20);
    let (d, r) = scan_bytes("bomb.zip", &zbytes, Limits::default());
    let r = r.unwrap();
    assert_eq!(r.apps.len(), 1);
    assert!(r.skipped.iter().any(|s| s.package_path.contains("huge.fap") && s.reason.contains("larger than")));
    assert_contained(d.path(), "bomb.zip");
}

#[test]
fn nesting_is_bounded() {
    let mut data = tar_of(&[("apps/Games/snake_game.fap", &read_fixture("snake_game.fap"))]);
    for i in 0..10 {
        data = gzip(&tar_of(&[(&format!("layer{i}.tgz"), &data)]));
    }
    let (_d, r) = scan_bytes("deep.tgz", &data, Limits::default());
    let r = r.unwrap();
    assert!(r.apps.is_empty());
    assert!(r.skipped.iter().any(|s| s.reason == "nested too deep"));
}

#[test]
fn malformed_apps_are_listed_with_an_error() {
    let snake = read_fixture("snake_game.fap");
    let mut junk = b"\x7fELF".to_vec();
    junk.extend(std::iter::repeat_n(0xAA, 300));
    let tar = tar_of(&[("apps/Games/cut.fap", &snake[..700]), ("apps/Games/junk.fap", &junk), ("apps/Games/empty.fap", b"")]);
    let (_d, r) = scan_bytes("bad.tar", &tar, Limits::default());
    let r = r.unwrap();
    assert_eq!(r.apps.len(), 3);
    for a in &r.apps {
        assert!(a.manifest.is_none() && a.error.is_some());
        let c = a.compat.as_ref().unwrap();
        assert_eq!((c.status, c.code, c.blocked), (Status::Incompatible, "not_a_fap", true));
    }
}

#[test]
fn corrupt_streams_do_not_fail_the_whole_scan() {
    let mut tgz = read_fixture("update-f7-test.tgz");
    let n = tgz.len();
    for b in &mut tgz[n / 2..n / 2 + 64] {
        *b ^= 0x5a;
    }
    let (_d, r) = scan_bytes("corrupt.tgz", &tgz, Limits::default());
    assert!(r.is_ok());
    let mut z = b"PK\x03\x04".to_vec();
    z.extend(std::iter::repeat_n(1u8, 100));
    let (_d, r) = scan_bytes("corrupt.zip", &z, Limits::default());
    assert!(r.unwrap().apps.is_empty());
    let (_d, r) = scan_bytes("text.txt", b"hello", Limits::default());
    assert_eq!(r.unwrap_err().code, "unknown_format");
}

#[test]
fn heatshrink_ths_directly() {
    let ths = heatshrink::encode_with_header(&read_fixture("resources-plain.tar"), Params { window_bits: 8, lookahead_bits: 4 });
    let (_d, r) = scan_bytes("resources.ths", &ths, Limits::default());
    assert_eq!(r.unwrap().apps.len(), 2);
}
