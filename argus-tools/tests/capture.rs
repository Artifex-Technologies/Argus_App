mod common;

use argus_tools::capture::{self, Builder, Library};
use argus_tools::snapshot::{self, ListedFile, Listing, Snapshots};
use common::*;

fn index_fixtures() -> (tempfile::TempDir, Library, capture::Index) {
    let d = tempfile::tempdir().unwrap();
    let lib = Library::open(&d.path().join("lib")).unwrap();
    let mut b = Builder::default();
    b.add_tree(&fixture("captures"), "pull").unwrap();
    lib.save_index(&b.finish(1)).unwrap();
    let idx = lib.load().unwrap();
    (d, lib, idx)
}

#[test]
fn parses_headers_not_payloads() {
    let (_d, _lib, idx) = index_fixtures();
    let kinds: Vec<&str> = idx.captures.iter().map(|c| c.kind.as_str()).collect();
    assert_eq!(idx.captures.len(), 8, "{kinds:?}"); // garage and garage_backup are one file; assets/ is skipped
    let garage = idx.captures.iter().find(|c| c.name == "garage").unwrap();
    // the walk is in name order, so the first of the two by name (garage.sub) names it on every file system
    let paths: Vec<&str> = garage.locations.iter().map(|l| l.path.as_str()).collect();
    assert_eq!(paths, ["/ext/subghz/garage.sub", "/ext/subghz/garage_backup.sub"]);
    assert_eq!(garage.fields["frequency_hz"], 433_920_000);
    assert_eq!(garage.fields["frequency_mhz"], "433.92");
    assert_eq!(garage.fields["protocol"], "Princeton");
    assert_eq!(garage.fields["preset"], "AM650");
    assert_eq!(garage.fields["bits"], 24);
    assert_eq!(garage.fields["code_type"], "fixed");
    assert_eq!(garage.locations.len(), 2);
    let car = idx.captures.iter().find(|c| c.name == "car").unwrap();
    assert_eq!(car.fields["code_type"], "rolling");
    let raw = idx.captures.iter().find(|c| c.kind == "subghz_raw").unwrap();
    assert_eq!((raw.fields["raw_samples"].as_u64(), raw.fields["raw_duration_ms"].as_u64()), (Some(8), Some(7)));
    assert_eq!(raw.fields["preset"], "FM476");
    let tv = idx.captures.iter().find(|c| c.kind == "infrared").unwrap();
    assert_eq!(tv.fields["signal_count"], 3);
    assert_eq!(tv.fields["protocols"]["NEC"], 2);
    let badge = idx.captures.iter().find(|c| c.name == "badge").unwrap();
    assert_eq!(badge.fields["device_type"], "Mifare Classic");
    assert_eq!(badge.fields["uid"], "4A 1B 2C 3D");
    assert_eq!(badge.fields["blocks_unread"], 1);
    // no payload anywhere in the index
    let json = std::fs::read_to_string(_d.path().join("lib/index.json")).unwrap();
    for secret in ["95 D5 D4", "01 23 45 67 89 AB", "9024 4512", "12 34 56 78 9A", "FF FF FF FF FF FF FF 07", "08 00 00 00"] {
        assert!(!json.contains(secret), "index leaks {secret}");
    }
}

#[test]
fn search_tags_dupes_explain() {
    let (_d, lib, idx) = index_fixtures();
    let names = |q: &str| {
        let mut v: Vec<String> = capture::search(&idx, q, 100).iter().map(|c| c.name.clone()).collect();
        v.sort();
        v
    };
    // garage_backup.sub is the same file as garage.sub, which comes first by name and names it
    assert_eq!(names("freq:433.92 proto:princeton"), vec!["garage", "garage_again"]);
    assert_eq!(names("kind:nfc"), vec!["badge", "badge_full"]);
    assert_eq!(names("uid:4a1b2c3d"), vec!["badge", "badge_full"]);
    assert_eq!(names("vol_up"), vec!["tv"]);
    assert_eq!(names("868"), vec!["gate_raw"]);
    assert_eq!(names("em4100"), vec!["door"]);

    let garage = idx.captures.iter().find(|c| c.name == "garage").unwrap();
    let tags = lib.tag(&garage.sha256, &["Home".into(), "gate".into()], &[]).unwrap();
    assert_eq!(tags, vec!["gate", "home"]);
    assert!(lib.tag(&garage.sha256, &["bad/tag".into()], &[]).is_err());
    let idx = lib.load().unwrap();
    assert_eq!(capture::search(&idx, "tag:home", 10).len(), 1);
    lib.tag(&garage.sha256, &[], &["gate".into()]).unwrap();
    assert_eq!(lib.load().unwrap().captures.iter().find(|c| c.name == "garage").unwrap().tags, vec!["home"]);

    let groups = capture::duplicates(&idx);
    let same_file: Vec<_> = groups.iter().filter(|g| g.kind == "same_file").collect();
    assert_eq!(same_file.len(), 1);
    assert_eq!(same_file[0].paths, vec!["/ext/subghz/garage.sub", "/ext/subghz/garage_backup.sub"]);
    let same_signal: Vec<Vec<String>> = groups.iter().filter(|g| g.kind == "same_signal").map(|g| g.paths.clone()).collect();
    assert!(same_signal.contains(&vec!["/ext/nfc/badge.nfc".to_string(), "/ext/nfc/badge_full.nfc".to_string()]));
    assert!(same_signal.iter().any(|p| p.contains(&"/ext/subghz/garage_again.sub".to_string())));

    let e = capture::explain(idx.captures.iter().find(|c| c.name == "car").unwrap());
    assert!(e.contains("KeeLoq") && e.contains("rolling-code"), "{e}");
    let e = capture::explain(idx.captures.iter().find(|c| c.name == "badge").unwrap());
    assert!(e.contains("UID 4A 1B 2C 3D") && e.contains("1 of them unread"), "{e}");
    assert!(lib.find(&idx, "zz").is_err());
    assert!(lib.find(&idx, &garage.id[..10]).is_ok());
}

#[test]
fn indexes_snapshots_too() {
    let d = tempfile::tempdir().unwrap();
    let snaps = Snapshots::open(&d.path().join("store")).unwrap();
    let staging = d.path().join("staging");
    let mut l = Listing { root: "/ext".into(), ..Default::default() };
    for (path, src) in [
        ("/ext/subghz/garage.sub", "captures/ext/subghz/garage.sub"),
        ("/ext/nfc/assets/mf_classic_dict.nfc", "captures/ext/nfc/badge.nfc"),
        ("/ext/apps/Games/snake_game.fap", "snake_game.fap"),
    ] {
        let p = staging.join(&path[1..]);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::copy(fixture(src), &p).unwrap();
        l.files.push(ListedFile { path: path.into(), size: std::fs::metadata(&p).unwrap().len(), md5: None });
    }
    let s = snapshot::create(&snaps, &l, Some(&staging), None, "", serde_json::Value::Null, 5).unwrap();
    let mut b = Builder::default();
    b.add_snapshots(&snaps).unwrap();
    let idx = b.finish(6);
    assert_eq!(idx.captures.len(), 1, "assets and apps are not captures");
    assert_eq!(idx.captures[0].locations[0].origin, "snapshot");
    assert_eq!(idx.captures[0].locations[0].source, s.id);
}

#[test]
fn hostile_capture_files() {
    let mut b = Builder::default();
    let loc = |p: &str| capture::Location { origin: "file".into(), source: "t".into(), path: p.into() };
    b.add(b"\xff\xfe garbage", loc("/ext/subghz/a.sub"));
    b.add(b"Filetype: Flipper SubGhz Key File\nFrequency: 99999999999999999999999\nBit: -1\n", loc("/ext/subghz/b.sub"));
    let mut big = b"Filetype: Flipper SubGhz RAW File\nRAW_Data:".to_vec();
    for _ in 0..200_000 {
        big.extend_from_slice(b" 9223372036854775807 -9223372036854775808");
    }
    b.add(&big, loc("/ext/subghz/c.sub"));
    b.add(b"Filetype: IR signals file\nname: \x1b[31mred\n", loc("/ext/infrared/d.ir"));
    let idx = b.finish(0);
    assert_eq!(idx.captures.len(), 3);
    for c in &idx.captures {
        let _ = capture::explain(c);
        assert!(!serde_json::to_string(c).unwrap().contains('\u{1b}'));
    }
}
