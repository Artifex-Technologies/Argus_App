//! The Argus tools window (TOOLS in the footer) against the fake Flipper: apps from a firmware package installed to
//! `/ext/apps` behind their dialog (cancelled: nothing written; confirmed: exactly the apps, checked by MD5; an app
//! already there only when ticked), the check-up of the card's apps, snapshots (taken, compared, a restore planned
//! and run behind its dialog, deleted behind its dialog), the capture library (read from the card, searched,
//! explained, tagged), and a rebuild that never reaches Limbo here. The built-in file picker fills the fields. All of
//! the tools' data lives in the test's own folder.

mod common;

use argus_app::tools::Tab;
use common::*;
use flipper_fake::FakeFlipper;

const PACKAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/update-f7-test.tgz");

fn tools(name: &str, fake: FakeFlipper) -> (Harness, Fake) {
    let dir = tempdir(name);
    let fake = Fake::start(&dir, fake);
    let mut h = Harness::new(&dir, vec![socket_device(&fake.sock)], Options::default());
    h.wait("the device's name", |h| h.shows("Fakeflip"));
    h.click("TOOLS");
    h.wait("the tools", |h| h.has("Apps"));
    (h, fake)
}

fn idle(h: &mut Harness, what: &str) {
    h.wait(what, |h| h.app.tools_state().busy.is_none());
    h.step();
}

fn message(h: &Harness) -> String {
    match &h.app.tools_state().message {
        Some(Ok(m)) => m.clone(),
        Some(Err(e)) => format!("ERROR {e}"),
        None => String::new(),
    }
}

/// The files written to the card, one entry per write (a write goes out in chunks of one request id).
fn app_writes(fake: &Fake) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in fake.changes().into_iter().filter(|c| c.contains("StorageWriteRequest")) {
        let w: Vec<&str> = c.split_whitespace().collect();
        let key = format!("{} {}", w[0], w[2]);
        if !out.contains(&key) {
            out.push(key);
        }
    }
    out
}

#[test]
fn apps_from_a_package_are_installed_only_after_their_dialog() {
    let (mut h, fake) = tools("tools-apps", FakeFlipper::standard());
    assert!(h.shows("API 88.2 (device)"), "the device's firmware API");
    std::fs::copy(PACKAGE, h.dir.join("home/update.tgz")).unwrap();
    // the picker: home, the file, Choose
    h.click("Browse…");
    h.wait("the picker", |h| h.app.picker_open());
    h.click("update.tgz");
    h.click("Choose");
    h.settle();
    assert!(!h.app.picker_open());
    assert!(h.app.tools_state().package.ends_with("home/update.tgz"));
    h.click("Open");
    idle(&mut h, "the package");
    assert_eq!(message(&h), "2 apps in the package, 2 compatible with API 88.2");
    h.settle();
    assert!(h.shows("Snake Game"));
    h.click("Check the card");
    idle(&mut h, "the plan");
    assert_eq!(message(&h), "2 new, 0 already on the card (asks before overwriting), 0 refused");
    assert!(fake.changes().is_empty(), "planning only looks");
    h.click("Install…");
    assert!(h.shows("Install apps?"));
    h.click("Cancel");
    h.settle();
    assert!(fake.changes().is_empty(), "cancelled: nothing written");
    h.click("Install…");
    h.click("Install");
    idle(&mut h, "the install");
    assert_eq!(message(&h), "Installed 2, kept 0 existing, refused 0, failed 0");
    let writes = app_writes(&fake);
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert!(writes.iter().all(|w| w.contains("/ext/apps/")), "{writes:?}");
    assert!(fake.changes().iter().all(|c| c.contains("/ext/apps")), "{:?}", fake.changes());

    // again: both are there now; nothing is overwritten unless ticked
    h.click("Check the card");
    idle(&mut h, "the plan again");
    assert_eq!(message(&h), "0 new, 2 already on the card (asks before overwriting), 0 refused");
    assert!(!h.is_enabled("Install…"), "nothing to do until an overwrite is ticked");
    h.click("on the card: overwrite");
    h.click("Install…");
    assert!(h.shows("Install apps?") && h.shows_text("1 of them over an app already there"));
    h.click("Install");
    idle(&mut h, "the second install");
    assert_eq!(message(&h), "Installed 1, kept 1 existing, refused 0, failed 0");
    assert_eq!(app_writes(&fake).len(), 3);

    // the check-up reads them back
    h.click("Check-up");
    h.click("Check the apps on the card");
    idle(&mut h, "the check-up");
    assert_eq!(message(&h), "2 apps on the card: 0 will not load on API 88.2, 0 unknown");
    h.settle();
    h.snapshot("tools-checkup");
}

#[test]
fn snapshots_are_taken_compared_restored_and_deleted() {
    let (mut h, fake) = tools("tools-snap", FakeFlipper::standard());
    h.click("Snapshots");
    idle(&mut h, "the list");
    assert!(h.shows("No snapshots yet"));
    h.set_field(0, "before");
    h.click("Take a snapshot");
    idle(&mut h, "the snapshot");
    assert!(message(&h).starts_with("Snapshot "), "{}", message(&h));
    idle(&mut h, "the list again");
    let snaps = h.app.tools_state().snapshots.clone();
    assert_eq!(snaps.len(), 1);
    assert!(fake.changes().is_empty(), "a snapshot only reads");
    let row = h.widgets().into_iter().find(|w| w.label.contains(&snaps[0].id)).unwrap().label;
    h.click(&row);
    h.click("Compare with the card");
    idle(&mut h, "the comparison");
    assert!(message(&h).contains(": 0 added, 0 removed, 0 changed"), "{}", message(&h));

    // nothing changed: restoring the card's nfc folder plans nothing to write
    h.set_field(1, "/ext/nfc");
    h.click("Plan a restore");
    idle(&mut h, "the plan");
    assert!(message(&h).starts_with("Restoring would create 0 and overwrite 0 file(s)"), "{}", message(&h));
    assert!(!h.is_enabled("Restore…"));

    // the card's /ext/nfc/badge.nfc goes away (through the file manager, behind its own dialog): now the plan creates it
    h.click("TOOLS");
    h.click("Files");
    h.wait("the storages", |h| h.has("SD Card"));
    h.double_click("SD Card");
    h.wait("the listing", |h| h.shows("nfc"));
    h.double_click("nfc");
    h.wait("the nfc listing", |h| h.shows("badge.nfc"));
    h.click("badge.nfc");
    h.click("Delete…");
    h.click("Delete");
    h.wait("the delete", |h| !h.shows("badge.nfc"));
    let before = fake.changes().len();
    assert_eq!(before, 1);
    h.click("TOOLS");
    h.click(&row);
    h.click("Plan a restore");
    idle(&mut h, "the plan");
    assert!(message(&h).starts_with("Restoring would create 1 and overwrite 0 file(s)"), "{}", message(&h));
    h.click("Restore…");
    assert!(h.shows("Restore from the snapshot?"));
    h.click("Cancel");
    h.settle();
    assert_eq!(fake.changes().len(), before, "cancelled: nothing written");
    h.click("Restore…");
    h.click("Restore");
    idle(&mut h, "the restore");
    assert!(message(&h).starts_with("Restored 1 file(s)"), "{}", message(&h));
    let after: Vec<String> = fake.changes()[before..].to_vec();
    assert!(after.iter().any(|c| c.contains("StorageWriteRequest /ext/nfc/badge.nfc")), "{after:?}");
    assert!(after.iter().all(|c| c.contains("/ext/nfc")), "only the restored folder: {after:?}");

    // deleting the snapshot asks
    h.click(&row);
    h.click("Delete…");
    assert!(h.shows(&format!("Delete snapshot {}?", snaps[0].id)));
    h.click("Cancel");
    h.settle();
    assert_eq!(h.app.tools_state().snapshots.len(), 1);
    h.click("Delete…");
    h.click("Delete");
    idle(&mut h, "the delete");
    idle(&mut h, "the list");
    assert!(h.app.tools_state().snapshots.is_empty());
}

#[test]
fn the_capture_library_reads_the_card_and_keeps_tags() {
    let (mut h, fake) = tools("tools-captures", FakeFlipper::standard().with_captures());
    h.click("Captures");
    h.click("Read from the card");
    idle(&mut h, "the read");
    let m = message(&h);
    assert!(m.starts_with("Read ") && m.contains("The library holds"), "{m}");
    idle(&mut h, "the search");
    let found = h.app.tools_state().captures.clone();
    assert!(!found.is_empty());
    assert!(fake.changes().is_empty(), "reading only reads");
    // no secrets, no assets
    let read: Vec<String> = fake
        .requests()
        .iter()
        .filter_map(|m| match &m.content {
            Some(flipper_rpc::proto::pb::main::Content::StorageReadRequest(r)) => Some(r.path.clone()),
            _ => None,
        })
        .collect();
    assert!(!read.iter().any(|p| p.starts_with("/ext/u2f") || p.contains("/assets/")), "{read:?}");
    let garage = found.iter().find(|c| c.name.contains("garage")).expect("garage.sub").clone();
    let row = h.widgets().into_iter().find(|w| w.label.contains(&garage.name)).unwrap().label;
    h.click(&row);
    idle(&mut h, "the explanation");
    assert!(h.app.tools_state().detail.is_some());
    h.set_field(1, "back-gate");
    h.click("Add tags");
    idle(&mut h, "the tag");
    assert!(h.app.tools_state().captures.iter().any(|c| c.tags == ["back-gate"]));
    h.set_field(0, "back-gate");
    h.click("Search");
    idle(&mut h, "the search");
    assert_eq!(h.app.tools_state().captures.len(), 1);
    h.click("Duplicates");
    idle(&mut h, "the duplicates");
    assert!(message(&h).ends_with("group(s) of duplicates"));
}

#[test]
fn the_rebuild_never_reaches_limbo_here_and_the_card_tools_wait_for_a_device() {
    let dir = tempdir("tools-nodev");
    let mut h = Harness::new(&dir, Vec::new(), Options::default());
    h.click("TOOLS");
    h.wait("the tools", |h| h.has("Apps"));
    assert!(h.shows("API 88.2 (default)"));
    // a package opens without a device; installing needs one
    h.set_field(0, PACKAGE);
    h.click("Open");
    idle(&mut h, "the package");
    assert!(!h.is_enabled("Check the card"));
    h.click("Snapshots");
    assert!(!h.is_enabled("Take a snapshot"));
    h.click("Rebuild");
    assert_eq!(h.app.tools_state().tab, Tab::Rebuild);
    h.settle();
    h.snapshot("tools-rebuild");
    let src = h.dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    h.set_field(0, src.to_str().unwrap());
    h.click("Build in Limbo");
    idle(&mut h, "the rebuild");
    assert_eq!(message(&h), "ERROR Limbo is not running here (no agent socket); nothing was sent");
}

#[test]
fn the_picker_fills_a_dialogs_field() {
    let (mut h, _fake) = tools("tools-picker", FakeFlipper::standard());
    h.click("TOOLS");
    h.click("Actions");
    h.click("Backup");
    // a fixed name, so the picture does not change with the clock
    h.set_field(0, h.dir.join("home/backup.tgz").to_str().unwrap());
    let name = "backup.tgz";
    std::fs::create_dir_all(h.dir.join("home/backups")).unwrap();
    h.click("Browse…");
    h.wait("the picker", |h| h.app.picker_open());
    h.settle();
    h.snapshot("picker");
    h.double_click("backups");
    h.wait("the folder", |h| h.shows_text("~/backups"));
    h.click("Save here");
    h.settle();
    assert!(!h.app.picker_open() && h.app.dialog().is_some(), "back in the dialog");
    assert!(h.field_value(0).ends_with(&format!("home/backups/{name}")), "{}", h.field_value(0));
    h.click("Cancel");
}
