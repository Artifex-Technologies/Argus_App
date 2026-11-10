//! Snapshots against a fake SD card: a local folder that plays the device (`<card>/ext/...`).
//! The "device" side of each step (listing, MD5, download) is done here the way the app does it
//! over RPC.

mod common;

use std::path::Path;

use argus_tools::snapshot::{self, Action, ListedFile, Listing, Snapshots};
use common::files_below;

fn card_write(card: &Path, path: &str, data: &[u8]) {
    let p = card.join(path.trim_start_matches('/'));
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, data).unwrap();
}

/// What the app's tree listing gives: every file with its size (and folders).
fn list_card(card: &Path) -> Listing {
    let mut l = Listing { root: "/ext".into(), ..Default::default() };
    for f in files_below(&card.join("ext")) {
        let p = format!("/ext/{f}");
        l.files.push(ListedFile { size: std::fs::metadata(card.join(&p[1..])).unwrap().len(), path: p, md5: None });
        let mut d = format!("/ext/{f}");
        while let Some((parent, _)) = d.rsplit_once('/') {
            if parent.len() <= 4 {
                break;
            }
            l.dirs.push(parent.to_string());
            d = parent.to_string();
        }
    }
    l.dirs.sort();
    l.dirs.dedup();
    l
}

fn device_md5(card: &Path, p: &str) -> String {
    argus_tools::md5_hex(&std::fs::read(card.join(&p[1..])).unwrap())
}

/// The whole app-side dance: plan, MD5 what the plan says, download the rest, create.
fn take(snaps: &Snapshots, card: &Path, label: &str, now: u64) -> (snapshot::Snapshot, usize) {
    let mut listing = list_card(card);
    let plan = snapshot::plan(snaps, &listing, None).unwrap();
    let mut confirmed = std::collections::BTreeSet::new();
    for f in &mut listing.files {
        if let Some(v) = plan.verify_md5.iter().find(|v| v.path == f.path) {
            let md5 = device_md5(card, &f.path);
            if md5 == v.md5 {
                confirmed.insert(f.path.clone());
            }
            f.md5 = Some(md5);
        }
    }
    let staging = tempfile::tempdir().unwrap();
    let mut downloaded = 0;
    for f in &listing.files {
        if !confirmed.contains(&f.path) {
            card_write(staging.path(), &f.path, &std::fs::read(card.join(&f.path[1..])).unwrap());
            downloaded += 1;
        }
    }
    let s = snapshot::create(snaps, &listing, Some(staging.path()), None, label, serde_json::json!({"name": "test"}), now).unwrap();
    (s, downloaded)
}

#[test]
fn snapshots_dedupe_diff_and_restore() {
    let root = tempfile::tempdir().unwrap();
    let card = root.path().join("card");
    let snaps = Snapshots::open(&root.path().join("store")).unwrap();
    card_write(&card, "/ext/subghz/garage.sub", b"Filetype: Flipper SubGhz Key File\n");
    card_write(&card, "/ext/nfc/badge.nfc", b"Filetype: Flipper NFC device\n");
    card_write(&card, "/ext/apps/Games/snake_game.fap", &common::read_fixture("snake_game.fap"));
    card_write(&card, "/ext/u2f/key.u2f", b"secret");

    let (s1, n1) = take(&snaps, &card, "first", 1_700_000_000);
    assert_eq!((s1.files.len(), n1), (4, 4));
    assert!(s1.skipped.is_empty());
    let objects_after_first = snaps.store.all_objects().unwrap().len();

    // change one, add one, remove one
    card_write(&card, "/ext/subghz/garage.sub", b"Filetype: Flipper SubGhz Key File\nVersion: 1\n");
    card_write(&card, "/ext/infrared/tv.ir", b"Filetype: IR signals file\n");
    std::fs::remove_file(card.join("ext/nfc/badge.nfc")).unwrap();
    let (s2, n2) = take(&snaps, &card, "second", 1_700_000_100);
    assert_eq!(n2, 2, "only the changed and the new file are downloaded");
    assert_eq!(s2.files.len(), 4);
    assert_eq!(snaps.store.all_objects().unwrap().len(), objects_after_first + 2, "unchanged files are stored once");

    let d = snapshot::diff_snapshots(&s1, &s2);
    assert_eq!(d.added.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), vec!["/ext/infrared/tv.ir"]);
    assert_eq!(d.removed.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), vec!["/ext/nfc/badge.nfc"]);
    assert_eq!(d.changed.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), vec!["/ext/subghz/garage.sub"]);
    assert_eq!(d.unchanged, 2);

    // diff against the card now, with and without MD5s
    card_write(&card, "/ext/u2f/key.u2f", b"SECRET"); // same size, other content
    let mut now = list_card(&card);
    let d = snapshot::diff_listing(&s2, &now);
    assert_eq!(d.unknown.len(), 4, "same sizes and no hashes: cannot tell");
    for f in &mut now.files {
        f.md5 = Some(device_md5(&card, &f.path));
    }
    let d = snapshot::diff_listing(&s2, &now);
    assert_eq!(d.changed.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), vec!["/ext/u2f/key.u2f"]);
    assert_eq!(d.unchanged, 3);

    // restore everything from the first snapshot: lists what it would overwrite
    let p = snapshot::restore_plan(&snaps, &s1, None, Some(&now)).unwrap();
    let by: std::collections::BTreeMap<_, _> = p.items.iter().map(|i| (i.remote.as_str(), i.action)).collect();
    assert_eq!(by["/ext/nfc/badge.nfc"], Action::Create);
    assert_eq!(by["/ext/subghz/garage.sub"], Action::Overwrite);
    assert_eq!(by["/ext/u2f/key.u2f"], Action::Overwrite);
    assert_eq!(by["/ext/apps/Games/snake_game.fap"], Action::Same);
    assert_eq!((p.creates, p.overwrites, p.same), (1, 2, 1));
    assert!(p.mkdirs.contains(&"/ext/nfc".to_string()), "{:?}", p.mkdirs);
    // the restore writes the objects' bytes: carry it out on the card and compare
    for i in &p.items {
        if i.action != Action::Same {
            card_write(&card, &i.remote, &std::fs::read(&i.local).unwrap());
        }
    }
    assert_eq!(std::fs::read(card.join("ext/nfc/badge.nfc")).unwrap(), b"Filetype: Flipper NFC device\n");
    assert_eq!(std::fs::read(card.join("ext/u2f/key.u2f")).unwrap(), b"secret");

    // one file only
    let p = snapshot::restore_plan(&snaps, &s1, Some("/ext/subghz/garage.sub"), None).unwrap();
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].action, Action::OverwriteUnverified);
    // a folder
    let p = snapshot::restore_plan(&snaps, &s1, Some("/ext/subghz"), None).unwrap();
    assert_eq!(p.items.len(), 1);
    assert!(snapshot::restore_plan(&snaps, &s1, Some("/ext/../int"), None).is_err());

    // list, delete, gc
    let list = snaps.list().unwrap();
    assert_eq!(list.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), vec!["first", "second"]);
    assert!(snapshot::summary(&list[0]).created.starts_with("2023-11-14 22:13:20"));
    snaps.delete(&s1.id).unwrap();
    let (removed, _) = snaps.gc().unwrap();
    assert_eq!(removed, 2, "the old garage.sub and badge.nfc are only in the first snapshot");
}

#[test]
fn hostile_listings_and_staging() {
    let root = tempfile::tempdir().unwrap();
    let snaps = Snapshots::open(&root.path().join("store")).unwrap();
    let staging = root.path().join("staging");
    card_write(&staging, "/ext/ok.txt", b"ok");
    std::fs::create_dir_all(root.path().join("outside")).unwrap();
    std::fs::write(root.path().join("outside/secret"), b"host secret").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.path().join("outside/secret"), staging.join("ext/link.txt")).unwrap();
    let listing = Listing {
        root: "/ext".into(),
        files: vec![
            ListedFile { path: "/ext/ok.txt".into(), size: 2, md5: None },
            ListedFile { path: "/ext/../../outside/secret".into(), size: 11, md5: None },
            ListedFile { path: "/int/.bt.keys".into(), size: 1, md5: None },
            ListedFile { path: "/ext/link.txt".into(), size: 11, md5: None },
            ListedFile { path: "/ext/missing".into(), size: 1, md5: Some("00".into()) },
        ],
        dirs: vec!["/ext/../x".into()],
    };
    let p = snapshot::plan(&snaps, &listing, None).unwrap();
    assert_eq!(p.refused.len(), 2);
    let s = snapshot::create(&snaps, &listing, Some(&staging), None, "x", serde_json::Value::Null, 1).unwrap();
    assert_eq!(s.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["/ext/ok.txt"]);
    assert_eq!(s.skipped.len(), 4);
    assert!(s.dirs.is_empty());
    assert!(snaps.load("../../etc/passwd").is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let obj = snaps.store.object_path(&s.files[0].sha256).unwrap();
        assert_eq!(std::fs::metadata(obj).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(snaps.store.root()).unwrap().permissions().mode() & 0o777, 0o700);
    }
}
