//! Backup, restore, erase and the update (from a file and from the update server) through the app, against the fake
//! and the local HTTPS stand-in: each behind its dialog (cancelled: nothing reaches the device; confirmed: exactly
//! the operation), the busy screen, the end screen, and the device coming back after it restarted.

mod common;

use std::time::Duration;

use argus_app::app::Stage;
use argus_app::net::Net;
use common::*;
use flipper_fake::update_server::{TestCa, UpdateServer};
use flipper_fake::{archives, FakeFlipper};

fn connect(name: &str, fake: FakeFlipper, opts: Options) -> (Harness, Fake) {
    let dir = tempdir(name);
    let fake = Fake::start(&dir, fake);
    let mut h = Harness::new(&dir, vec![socket_device(&fake.sock)], opts);
    h.wait("the device's name", |h| h.shows("Fakeflip"));
    (h, fake)
}

fn actions(h: &mut Harness) {
    h.click("Actions");
    h.wait("the actions", |h| h.has("Backup"));
}

fn cancel(h: &mut Harness, fake: &Fake) {
    let before = fake.changes();
    h.click("Cancel");
    for _ in 0..20 {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(h.app.dialog().is_none());
    assert_eq!(h.app.stage(), &Stage::Ready, "cancelling starts nothing");
    assert_eq!(fake.changes(), before, "cancelling sent nothing that changes the device");
}

fn finished(h: &mut Harness) {
    h.wait("the end of the operation", |h| matches!(h.app.stage(), Stage::Finished { .. } | Stage::Error { .. }));
    if let Stage::Error { title, text } = h.app.stage() {
        panic!("{title}: {text}");
    }
}

#[test]
fn backup_writes_int_to_a_new_archive_after_its_dialog() {
    let (mut h, fake) = connect("backup", FakeFlipper::standard(), Options::default());
    actions(&mut h);
    let out = h.dir.join("home/b.tgz");
    h.click("Backup");
    h.set_field(0, out.to_str().unwrap());
    cancel(&mut h, &fake);
    assert!(!out.exists());
    h.click("Backup");
    h.set_field(0, out.to_str().unwrap());
    h.click("Backup");
    finished(&mut h);
    h.settle();
    assert!(h.shows("Success!") && h.shows("Operation has finished successfully."));
    h.snapshot("finished");
    let archive = flipper_rpc::restore::read_backup(std::fs::File::open(&out).unwrap(), &Default::default()).unwrap();
    assert_eq!(archive.files(), 4, "/int's four files");
    assert!(fake.changes().is_empty(), "a backup only reads");
    h.click("CONTINUE");
    h.settle();
    assert_eq!(h.app.stage(), &Stage::Ready);
}

#[test]
fn restore_writes_the_backup_and_restarts_the_device() {
    let (mut h, fake) = connect("restore", FakeFlipper::standard().with_changed_int(), Options::default());
    let file = h.dir.join("home/int.tgz");
    std::fs::write(&file, archives::gzip(&archives::tar(&archives::standard_int(), true))).unwrap();
    actions(&mut h);
    h.click("Restore");
    h.set_field(0, file.to_str().unwrap());
    cancel(&mut h, &fake);
    h.click("Restore");
    h.set_field(0, file.to_str().unwrap());
    h.click("Restore");
    finished(&mut h);
    let changes = fake.changes();
    assert!(changes.iter().any(|c| c.contains("StorageWriteRequest /int/sub/deep.txt")), "{changes:?}");
    assert!(changes.last().unwrap().contains("SystemRebootRequest"), "it restarts last: {changes:?}");
    // the device restarted; Continue waits for it and connects again
    h.click("CONTINUE");
    h.wait("the device back", |h| h.app.stage() == &Stage::Ready && h.shows("Fakeflip"));
    assert_eq!(fake.sessions(), 2);
    let standard = FakeFlipper::standard().dump();
    let int = |d: &str| d.lines().filter(|l| l.contains(" /int/sub")).map(String::from).collect::<Vec<_>>();
    fake.wait_ended(1);
    assert_eq!(int(&fake.last_dump().unwrap()), int(&standard), "the backup's files are back");
}

#[test]
fn a_hostile_backup_is_refused_before_anything_is_sent() {
    let (mut h, fake) = connect("hostile", FakeFlipper::standard(), Options::default());
    let (_, _, data) = archives::hostile().into_iter().next().unwrap();
    let file = h.dir.join("home/bad.tgz");
    std::fs::write(&file, data).unwrap();
    actions(&mut h);
    h.click("Restore");
    h.set_field(0, file.to_str().unwrap());
    h.click("Restore");
    h.wait("the error", |h| matches!(h.app.stage(), Stage::Error { .. }));
    h.settle();
    assert!(h.shows("Restore Failed"));
    assert!(fake.changes().is_empty());
    h.click("Back");
    h.settle();
    assert_eq!(h.app.stage(), &Stage::Ready, "the device is still there");
}

#[test]
fn erase_needs_its_dialog_and_resets_the_device() {
    let (mut h, fake) = connect("erase", FakeFlipper::standard(), Options::default());
    actions(&mut h);
    h.click("Erase");
    assert!(h.shows("Erase device?"));
    cancel(&mut h, &fake);
    h.click("Erase");
    h.click("Erase");
    finished(&mut h);
    assert_eq!(fake.changes().len(), 1);
    assert!(fake.changes()[0].contains("SystemFactoryResetRequest"));
}

#[test]
fn an_update_from_a_file_goes_through_the_devices_updater() {
    let (mut h, fake) = connect("update-file", FakeFlipper::standard(), Options::default());
    let file = h.dir.join("home/update.tgz");
    std::fs::write(&file, archives::update_package(true)).unwrap();
    h.click("Install from file");
    assert!(h.shows("This replaces the Argus firmware (ARG-1.5.1r2) on this device."), "the fork warning");
    h.set_field(0, file.to_str().unwrap());
    cancel(&mut h, &fake);
    h.click("Install from file");
    h.set_field(0, file.to_str().unwrap());
    h.click("Install");
    finished(&mut h);
    let changes = fake.changes();
    assert!(changes.iter().any(|c| c.contains("SystemUpdateRequest /ext/update/f7-update-test/update.fuf")), "{changes:?}");
    assert!(changes.last().unwrap().contains("SystemRebootRequest"), "{changes:?}");
}

#[test]
fn a_package_the_device_refuses_ends_in_an_error_without_a_restart() {
    let (mut h, fake) = connect("update-bad", FakeFlipper::standard(), Options::default());
    let file = h.dir.join("home/update.tgz");
    std::fs::write(&file, archives::update_package(false)).unwrap();
    h.click("Install from file");
    h.set_field(0, file.to_str().unwrap());
    h.click("Install");
    h.wait("the error", |h| matches!(h.app.stage(), Stage::Error { .. }));
    assert!(!fake.changes().iter().any(|c| c.contains("SystemRebootRequest")));
}

fn server(dir: &std::path::Path) -> (UpdateServer, Net) {
    let fw = dir.join("firmware");
    flipper_fake::firmware::write(&fw, false).unwrap();
    let ca = TestCa::new();
    let server = UpdateServer::start(&fw, &ca).unwrap();
    let root = ureq::tls::Certificate::from_der(&ca.ca_der).to_owned();
    let client = flipper_updates::fetch::Client::with_roots(vec![root], Duration::from_secs(5));
    let net = Net::new(server.base(), Some(client));
    (server, net)
}

#[test]
fn an_update_from_the_server_needs_its_dialog_and_writes_the_region_data() {
    let dir = tempdir("update-server");
    let (server, net) = server(&dir);
    // a device of region 1: its region data comes from the server first
    let mut device = FakeFlipper::standard();
    for (k, v) in device.devinfo.iter_mut() {
        if k == "hardware.region.builtin" {
            *v = "1".into();
        }
    }
    let fake = Fake::start(&dir, device);
    let mut h = Harness::new(&dir, vec![socket_device(&fake.sock)], Options { check_updates: true, net: Some(net), ..Options::default() });
    h.wait("the directory", |h| h.has("INSTALL"));
    h.settle();
    assert!(h.shows("Release 9.9.9"));
    h.snapshot("home-install");
    h.click("INSTALL");
    assert!(h.shows("Update firmware?") && h.shows("This replaces the Argus firmware (ARG-1.5.1r2) on this device."));
    cancel(&mut h, &fake);
    let fetched = |s: &UpdateServer| s.requests.lock().unwrap().iter().filter(|r| r.contains("update-test.tgz")).count();
    assert_eq!(fetched(&server), 0, "nothing is downloaded before the dialog is confirmed");
    h.click("INSTALL");
    h.click("Update");
    finished(&mut h);
    assert_eq!(fetched(&server), 1);
    let changes = fake.changes();
    assert!(changes[0].contains("StorageWriteRequest /int/.region_data"), "the region data first: {changes:?}");
    assert!(changes.iter().any(|c| c.contains("SystemUpdateRequest")), "{changes:?}");
}

#[test]
fn an_unreachable_server_says_so_and_can_be_tried_again() {
    let dir = tempdir("no-server");
    let fake = Fake::start(&dir, FakeFlipper::standard());
    // the default test address: nothing listens there
    let mut h = Harness::new(&dir, vec![socket_device(&fake.sock)], Options { check_updates: true, ..Options::default() });
    h.wait("the error", |h| h.has("TRY AGAIN"));
    assert!(h.shows("Cannot connect to update server"));
    h.click("TRY AGAIN");
    h.wait("the error again", |h| h.has("TRY AGAIN"));
    assert!(fake.changes().is_empty());
}
