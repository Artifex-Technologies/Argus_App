mod common;

use argus_tools::api::{self, FirmwareApi, Status};
use argus_tools::fap;
use common::*;

const FW: FirmwareApi = FirmwareApi { major: 88, minor: 2, target: 7 };

#[test]
fn reads_a_real_manifest() {
    let info = fap::inspect(&read_fixture("snake_game.fap")).unwrap();
    let m = &info.manifest;
    assert!(m.is_valid());
    assert_eq!((m.api_major, m.api_minor, m.hardware_target), (88, 2, 7));
    assert_eq!(m.name, "Snake Game");
    assert_eq!(m.stack_size, 1024);
    assert_eq!(m.app_version_text, "1.0");
    assert!(m.has_icon);
    assert_eq!(m.section_size, 85);
    assert!(info.imports.contains(&"canvas_draw_box".to_string()));
}

#[test]
fn real_apps_are_compatible_with_their_own_firmware() {
    let table = api::builtin_table(&FW).expect("88.2 table is built in");
    assert!(table.len() > 2000);
    for f in ["snake_game.fap", "subghz_jammer_detect.fap"] {
        let info = fap::inspect(&read_fixture(f)).unwrap();
        let c = api::check(&info, &FW, Some(&table));
        assert_eq!(c.status, Status::Compatible, "{f}: {}", c.reason);
        assert!(c.symbols_checked);
    }
}

#[test]
fn the_loader_rule_major_must_match() {
    let base = read_fixture("snake_game.fap");
    let table = api::builtin_table(&FW);
    let check = |d: Vec<u8>| api::check(&fap::inspect(&d).unwrap(), &FW, table.as_ref());
    assert_eq!(check(with_api(&base, 87, 2, 7)).code, "api_too_old");
    assert_eq!(check(with_api(&base, 89, 0, 7)).code, "api_too_new");
    assert_eq!(check(with_api(&base, 88, 2, 8)).code, "target_mismatch");
    // an older minor with the same major loads (the firmware does not compare minors)
    assert_eq!(check(with_api(&base, 88, 0, 7)).status, Status::Compatible);
    // a newer minor: with the symbol table the answer is exact; this app uses nothing new
    assert_eq!(check(with_api(&base, 88, 9, 7)).status, Status::Compatible);
}

#[test]
fn newer_minor_without_a_table_is_unknown() {
    let fw = FirmwareApi { major: 88, minor: 1, target: 7 };
    assert!(api::builtin_table(&fw).is_none());
    let info = fap::inspect(&read_fixture("snake_game.fap")).unwrap();
    let c = api::check(&info, &fw, None);
    assert_eq!((c.status, c.code), (Status::Unknown, "minor_newer_unchecked"));
    let fw0 = FirmwareApi { major: 88, minor: 3, target: 7 };
    assert_eq!(api::check(&info, &fw0, None).status, Status::Compatible);
}

#[test]
fn a_symbol_the_firmware_lacks_makes_it_incompatible() {
    // as if built for a fork that exports one more function under the same API version
    let d = rename_symbol(&read_fixture("snake_game.fap"), "canvas_draw_box", "canvas_draw_bxx");
    let info = fap::inspect(&d).unwrap();
    let c = api::check(&info, &FW, api::builtin_table(&FW).as_ref());
    assert_eq!((c.status, c.code), (Status::Incompatible, "missing_symbols"));
    assert_eq!(c.missing_symbols, vec!["canvas_draw_bxx".to_string()]);
}

#[test]
fn plugins_may_import_from_their_host() {
    let info = fap::inspect(&read_fixture("js_gui__loading.fal")).unwrap();
    assert!(info.manifest.is_plugin());
    let c = api::check(&info, &FW, api::builtin_table(&FW).as_ref());
    assert_eq!((c.status, c.code), (Status::Unknown, "plugin_host_symbols"));
}

#[test]
fn an_oversized_manifest_is_blocked() {
    // the loader would copy the whole section into its 85-byte struct
    let mut d = read_fixture("snake_game.fap");
    let (_, sh) = fapmeta_offsets(&d);
    d[sh + 20..sh + 24].copy_from_slice(&200u32.to_le_bytes());
    let info = fap::inspect(&d).unwrap();
    let c = api::check(&info, &FW, None);
    assert_eq!(c.code, "manifest_oversized");
    assert!(c.blocked);
}

#[test]
fn bad_magic_and_short_manifest() {
    let mut d = read_fixture("snake_game.fap");
    let (off, sh) = fapmeta_offsets(&d);
    d[off] ^= 0xff;
    let c = api::check(&fap::inspect(&d).unwrap(), &FW, None);
    assert_eq!(c.code, "invalid_manifest");
    let mut d = read_fixture("snake_game.fap");
    d[sh + 20..sh + 24].copy_from_slice(&40u32.to_le_bytes());
    assert_eq!(fap::inspect(&d).unwrap_err().code, "manifest_short");
}

#[test]
fn malformed_elf_is_an_error_not_a_panic() {
    let d = read_fixture("snake_game.fap");
    for cut in [0, 3, 51, 52, 100, 1000, d.len() - 1] {
        let _ = fap::inspect(&d[..cut]);
    }
    let mut bad = d.clone();
    bad[0x20..0x24].copy_from_slice(&u32::MAX.to_le_bytes()); // section table offset
    assert!(fap::inspect(&bad).is_err());
    let mut bad = d.clone();
    bad[0x30..0x32].copy_from_slice(&u16::MAX.to_le_bytes()); // section count
    assert!(fap::inspect(&bad).is_err());
    assert_eq!(fap::inspect(b"not an elf at all").unwrap_err().code, "not_elf");
}

#[test]
fn firmware_api_parsing() {
    assert_eq!(FirmwareApi::parse("88.2"), Some(FW));
    assert_eq!(FirmwareApi::parse("88.2/f7"), Some(FW));
    assert_eq!(FirmwareApi::parse("86.0/18").unwrap().target, 18);
    assert!(FirmwareApi::parse("88").is_none());
    assert!(FirmwareApi::parse("x.y").is_none());
}
