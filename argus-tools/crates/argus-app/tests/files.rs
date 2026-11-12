//! The file manager against the fake: browsing, the Qt app's names ("SD Card", "Internal Flash"), hidden files, and
//! every change (upload, new folder, rename, delete) behind its dialog: cancelled once, nothing reaches the device;
//! confirmed, exactly the request and the change on the fake. Downloads land in a new local file only.

mod common;

use common::*;
use flipper_fake::FakeFlipper;

fn files(name: &str) -> (Harness, Fake) {
    let dir = tempdir(name);
    let fake = Fake::start(&dir, FakeFlipper::standard());
    let mut h = Harness::new(&dir, vec![socket_device(&fake.sock)], Options::default());
    h.wait("the device's name", |h| h.shows("Fakeflip"));
    h.click("Files");
    h.wait("the storages", |h| h.has("SD Card"));
    (h, fake)
}

fn open_sd(h: &mut Harness) {
    h.double_click("SD Card");
    h.wait("the SD card's folders", |h| h.has("subghz") && h.has("Manifest"));
}

/// Cancels the open dialog and checks that nothing reached the device.
fn cancel(h: &mut Harness, fake: &Fake) {
    let before = fake.changes();
    h.click("Cancel");
    for _ in 0..20 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(h.dialog().is_none(), "the dialog closed");
    assert_eq!(fake.changes(), before, "cancelling sent nothing that changes the device");
}

trait Dialogs {
    fn dialog(&self) -> Option<argus_app::app::Dialog>;
}

impl Dialogs for Harness {
    fn dialog(&self) -> Option<argus_app::app::Dialog> {
        self.app.dialog().cloned()
    }
}

#[test]
fn browsing_shows_the_storages_and_folders_as_the_qt_app_names_them() {
    let (mut h, fake) = files("browse");
    assert!(h.has("Internal Flash") && !h.shows("any"));
    open_sd(&mut h);
    h.settle();
    assert!(h.shows("SD Card/"));
    h.snapshot("files-sd");
    h.double_click("subghz");
    h.wait("the signals", |h| h.has("signal_18.sub"));
    h.click("<");
    h.wait("back on the card", |h| h.has("Manifest"));
    h.click(">");
    h.wait("forward again", |h| h.has("signal_00.sub"));
    assert!(fake.changes().is_empty());
}

#[test]
fn hidden_files_show_only_when_asked() {
    let (mut h, _fake) = files("hidden");
    h.double_click("Internal Flash");
    h.wait("/int", |h| h.has("favorites.txt"));
    assert!(!h.shows(".bt.settings"));
    h.click("Hidden");
    h.wait("the hidden file", |h| h.has(".bt.settings"));
}

#[test]
fn delete_needs_its_dialog() {
    let (mut h, fake) = files("delete");
    open_sd(&mut h);
    h.click("nfc");
    h.click("Delete…");
    assert!(h.shows("Delete \"nfc\"?"));
    cancel(&mut h, &fake);
    assert!(h.has("nfc"));
    h.click("Delete…");
    h.click("Delete");
    h.wait("the folder to go", |h| !h.shows("nfc") && h.has("subghz"));
    assert_eq!(fake.changes().len(), 1);
    assert!(fake.changes()[0].contains("StorageDeleteRequest /ext/nfc"), "{:?}", fake.changes());
}

#[test]
fn new_folder_needs_its_dialog_and_a_valid_name() {
    let (mut h, fake) = files("mkdir");
    open_sd(&mut h);
    h.click("New folder");
    cancel(&mut h, &fake);
    h.click("New folder");
    h.set_field(0, "a/b");
    h.click("Create");
    h.settle();
    assert!(h.dialog().is_some_and(|d| d.error.is_some()), "a name with / is refused in the dialog");
    assert!(fake.changes().is_empty());
    h.set_field(0, "Scans");
    h.click("Create");
    h.wait("the new folder", |h| h.has("Scans"));
    assert_eq!(fake.changes().len(), 1);
    assert!(fake.changes()[0].contains("StorageMkdirRequest /ext/Scans"));
}

#[test]
fn rename_needs_its_dialog() {
    let (mut h, fake) = files("rename");
    open_sd(&mut h);
    h.click("empty");
    h.click("Rename");
    assert_eq!(h.field_value(0), "empty");
    cancel(&mut h, &fake);
    h.click("Rename");
    h.set_field(0, "full");
    h.click("Rename");
    h.wait("the new name", |h| h.has("full") && !h.shows("empty"));
    assert_eq!(fake.changes().len(), 1);
    assert!(fake.changes()[0].contains("StorageRenameRequest /ext/empty"));
}

#[test]
fn upload_needs_its_dialog() {
    let (mut h, fake) = files("upload");
    let local = h.dir.join("home/notes.txt");
    std::fs::write(&local, b"hello from the app\n").unwrap();
    open_sd(&mut h);
    h.click("Upload…");
    h.set_field(0, local.to_str().unwrap());
    cancel(&mut h, &fake);
    assert!(!h.shows("notes.txt"));
    h.click("Upload…");
    h.set_field(0, local.to_str().unwrap());
    h.click("Upload");
    h.wait("the uploaded file", |h| h.has("notes.txt"));
    assert!(fake.changes().iter().all(|c| c.contains("StorageWriteRequest /ext/notes.txt")), "{:?}", fake.changes());
    // the bytes on the fake, after the session
    drop(h);
    fake.wait_ended(1);
    let dump = fake.last_dump().unwrap();
    assert!(dump.lines().any(|l| l.starts_with("f /ext/notes.txt 19 ")), "{dump}");
}

#[test]
fn download_makes_a_new_local_file_and_changes_nothing() {
    let (mut h, fake) = files("download");
    open_sd(&mut h);
    h.click("Manifest");
    let out = h.dir.join("home/Manifest.copy");
    h.click("Download…");
    h.set_field(0, out.to_str().unwrap());
    h.click("Cancel");
    h.settle();
    assert!(!out.exists());
    h.click("Download…");
    h.set_field(0, out.to_str().unwrap());
    h.click("Download");
    h.wait("the file", |_| out.exists() && std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0) == 48_213);
    // an existing file is never overwritten: the dialog says so and stays
    h.wait("the download to finish", |h| h.has("Download…"));
    h.click("Download…");
    h.set_field(0, out.to_str().unwrap());
    h.click("Download");
    h.settle();
    assert!(h.dialog().is_some_and(|d| d.error.as_deref().is_some_and(|e| e.contains("exists"))));
    assert!(fake.changes().is_empty());
}
