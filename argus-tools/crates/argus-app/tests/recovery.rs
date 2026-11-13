//! Recovery mode against the fake STM32WB55 bootloader (flipper_fake::dfu) on a Unix socket, across the chip's resets:
//! the repair from the update server (flipper_fake::update_server on 127.0.0.1 with the test's CA) and from local files,
//! then the steps on the firmware once the chip has started it (a fake Flipper on another socket that "appears" when
//! the chip boots its firmware), and leaving recovery mode, each behind its dialog (cancelled: not one request reaches
//! the bootloader, nothing is downloaded), damaged files refused before anything is opened, the partial success when
//! the wireless stack is not installed, and a device in recovery mode on USB, which the app cannot reach (nothing
//! starts). No USB, nothing real flashed.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use argus_app::app::Stage;
use argus_app::discovery::{Found, Kind};
use argus_app::net::Net;
use common::*;
use flipper_fake::dfu::{self, FakeDfu, Faults};
use flipper_fake::update_server::{TestCa, UpdateServer};
use flipper_fake::FakeFlipper;

struct Bootloader {
    sock: PathBuf,
    stop: Arc<AtomicBool>,
    server: JoinHandle<FakeDfu>,
}

impl Bootloader {
    fn start(dir: &Path, faults: Faults) -> Bootloader {
        let sock = dir.join("dfu.sock");
        let stop = Arc::new(AtomicBool::new(false));
        let (path, flag) = (sock.clone(), stop.clone());
        let server = std::thread::spawn(move || dfu::serve_with_resets(FakeDfu::new(faults), &path, Duration::from_millis(20), &flag));
        while !sock.exists() {
            std::thread::sleep(Duration::from_millis(2));
        }
        Bootloader { sock, stop, server }
    }

    /// The chip across its lives: listed as recovery mode while its bootloader is up, gone during a reset, and as
    /// `firmware` (the fake Flipper's socket) once it has started its firmware.
    fn start_lives(dir: &Path, faults: Faults, devices: Shared, firmware: Found) -> Bootloader {
        let sock = dir.join("dfu.sock");
        let stop = Arc::new(AtomicBool::new(false));
        let (path, flag) = (sock.clone(), stop.clone());
        let server = std::thread::spawn(move || {
            let enter = AtomicBool::new(true);
            dfu::serve_lives(FakeDfu::new(faults), &path.clone(), Duration::from_millis(20), &flag, &enter, &mut |life| match life {
                dfu::Life::Bootloader => devices.set(vec![Found { kind: Kind::RecoverySocket(path.clone()) }]),
                dfu::Life::Gone => devices.set(Vec::new()),
                dfu::Life::Firmware => devices.set(vec![firmware.clone()]),
            })
        });
        while !sock.exists() {
            std::thread::sleep(Duration::from_millis(2));
        }
        Bootloader { sock, stop, server }
    }

    fn finish(self) -> FakeDfu {
        self.stop.store(true, Ordering::SeqCst);
        self.server.join().unwrap()
    }
}

/// The made-up update folder of the fake: firmware.dfu, ob.data, radio.bin.
fn files(dir: &Path) -> PathBuf {
    let fw = dir.join("firmware");
    flipper_fake::firmware::write(&fw, false).unwrap();
    fw
}

fn recovery(name: &str, faults: Faults) -> (Harness, Bootloader, PathBuf) {
    let dir = tempdir(name);
    let boot = Bootloader::start(&dir, faults);
    let fw = files(&dir);
    let mut h = Harness::new(&dir, vec![Found { kind: Kind::RecoverySocket(boot.sock.clone()) }], Options::default());
    h.wait("recovery mode", |h| h.has("REPAIR"));
    (h, boot, fw)
}

/// Opens the repair with local files (Actions, "Repair from files…") and names the files.
fn fill(h: &mut Harness, fw: &Path, radio: bool) {
    h.click("Actions");
    h.click("Repair from files…");
    h.set_field(0, fw.join("firmware.dfu").to_str().unwrap());
    h.set_field(1, fw.join("ob.data").to_str().unwrap());
    if radio {
        h.set_field(2, fw.join("radio.bin").to_str().unwrap());
    }
}

/// The wireless stack running, as on a Flipper in normal use.
fn running() -> Faults {
    Faults { fus_model: true, fus: Some([0xFF, 0xFE]), ..Faults::default() }
}

#[test]
fn recovery_mode_shows_and_cancelled_dialogs_send_nothing() {
    let (mut h, boot, fw) = recovery("rec-cancel", running());
    h.settle();
    assert!(h.shows("Recovery mode") && h.shows("socket dfu.sock"));
    assert!(!h.has("Files"), "no file manager in recovery mode");
    h.snapshot("recovery");
    // the main button: the repair from the update server
    h.click("REPAIR");
    assert!(h.shows("Repair Device?") && h.shows_text("release channel will be downloaded"));
    h.click("Cancel");
    fill(&mut h, &fw, true);
    h.click("Cancel");
    h.click("Leave recovery mode");
    h.click("Cancel");
    h.settle();
    assert_eq!(h.app.stage(), &Stage::Ready);
    let fake = boot.finish();
    assert!(fake.log.is_empty(), "not one request reached the bootloader: {:?}", fake.log);
    assert_eq!(fake.flash, FakeDfu::new(running()).flash);
}

#[test]
fn a_damaged_firmware_file_is_refused_in_the_dialog() {
    let (mut h, boot, fw) = recovery("rec-damaged", running());
    let bad = fw.join("bad.dfu");
    let mut data = std::fs::read(fw.join("firmware.dfu")).unwrap();
    data[20] ^= 0xff;
    std::fs::write(&bad, data).unwrap();
    fill(&mut h, &fw, false);
    h.set_field(0, bad.to_str().unwrap());
    h.click("Repair");
    h.settle();
    assert!(h.app.dialog().is_some_and(|d| d.error.as_deref().is_some_and(|e| e.contains("firmware file is refused"))));
    assert!(boot.finish().log.is_empty());
}

#[test]
fn the_repair_writes_the_firmware_across_the_resets() {
    let (mut h, boot, fw) = recovery("rec-repair", running());
    fill(&mut h, &fw, true);
    h.click("Repair");
    h.wait("the end", |h| matches!(h.app.stage(), Stage::Finished { .. } | Stage::Error { .. }));
    assert_eq!(h.app.stage(), &Stage::Finished { warning: None }, "a whole repair");
    let fake = boot.finish();
    assert!(fake.booted_firmware, "the chip starts its firmware at the end");
    assert_eq!(fake.fus_commands, [0x52, 0x53], "the wireless stack deleted and installed");
}

#[test]
fn a_wireless_stack_that_is_not_installed_is_a_partial_success() {
    // a FUS stuck in a state the flow does not know: the repair goes on without the wireless stack
    let (mut h, boot, fw) = recovery("rec-partial", Faults { fus_model: true, fus: Some([0x30, 0x02]), ..Faults::default() });
    fill(&mut h, &fw, true);
    h.click("Repair");
    h.wait("the end", |h| matches!(h.app.stage(), Stage::Finished { .. } | Stage::Error { .. }));
    let Stage::Finished { warning: Some(w) } = h.app.stage().clone() else { panic!("{:?}", h.app.stage()) };
    assert!(w.starts_with("repaired, but the radio stack was not installed: "), "{w}");
    h.settle();
    assert!(h.shows("Done, with a warning") && h.shows(&w));
    assert!(h.shows("Operation has finished, but not all of it: see the warning."));
    h.snapshot("finished-warning");
    assert!(boot.finish().booted_firmware);
}

#[test]
fn leaving_recovery_mode_needs_its_dialog() {
    let (mut h, boot, _fw) = recovery("rec-exit", running());
    h.click("Actions");
    h.click("Leave recovery mode");
    h.click("Leave");
    h.wait("the end", |h| matches!(h.app.stage(), Stage::Finished { .. } | Stage::Error { .. }));
    assert_eq!(h.app.stage(), &Stage::Finished { warning: None });
    assert!(boot.finish().left, "the chip left its bootloader");
}

#[test]
fn a_device_in_recovery_mode_on_usb_is_never_reached() {
    let dir = tempdir("rec-usb");
    let fw = files(&dir);
    let mut h = Harness::new(&dir, vec![Found { kind: Kind::RecoveryUsb("1-4".into()) }], Options::default());
    h.wait("recovery mode", |h| h.has("REPAIR"));
    let refused = |h: &Harness| {
        h.app.dialog().is_some_and(|d| d.error.as_deref().is_some_and(|e| e.contains("cannot reach a device in recovery mode over USB")))
    };
    // from the server: refused before anything is fetched
    h.click("REPAIR");
    h.click("Repair");
    h.settle();
    assert!(refused(&h));
    h.click("Cancel");
    fill(&mut h, &fw, false);
    h.click("Repair");
    h.settle();
    assert!(refused(&h));
    assert_eq!(h.app.stage(), &Stage::Ready, "nothing started");
}

fn update_server(dir: &Path, core2_mismatch: bool) -> (UpdateServer, Net) {
    let fw = dir.join("server");
    flipper_fake::firmware::write_with(&fw, false, core2_mismatch).unwrap();
    let ca = TestCa::new();
    let server = UpdateServer::start(&fw, &ca).unwrap();
    let root = ureq::tls::Certificate::from_der(&ca.ca_der).to_owned();
    let client = flipper_updates::fetch::Client::with_roots(vec![root], Duration::from_secs(5));
    let net = Net::new(server.base(), Some(client));
    (server, net)
}

/// The chip in its bootloader, the fake Flipper its firmware becomes (on its own socket, listed once the chip has
/// started it), and the app finding them as they come and go.
fn lives(name: &str, faults: Faults, firmware: FakeFlipper, net: Option<Net>) -> (Harness, Bootloader, Fake) {
    let dir = tempdir(name);
    let fake = Fake::start(&dir, firmware);
    let devices = Shared::default();
    let boot = Bootloader::start_lives(&dir, faults, devices.clone(), socket_device(&fake.sock));
    let mut h = Harness::with_discovery(&dir, Box::new(devices), Options { net, ..Options::default() });
    h.wait("recovery mode", |h| h.has("REPAIR"));
    (h, boot, fake)
}

fn region_1() -> FakeFlipper {
    let mut device = FakeFlipper::standard();
    for (k, v) in device.devinfo.iter_mut() {
        if k == "hardware.region.builtin" {
            *v = "1".into();
        }
    }
    device
}

#[test]
fn the_repair_from_the_server_then_the_steps_on_the_firmware() {
    let dir = tempdir("rec-server-files");
    let (server, net) = update_server(&dir, false);
    let (mut h, boot, fake) = lives("rec-server", running(), region_1(), Some(net));
    h.click("REPAIR");
    h.click("Cancel");
    h.settle();
    assert!(server.requests.lock().unwrap().is_empty(), "nothing is fetched before the dialog is confirmed");
    h.click("REPAIR");
    h.click("Repair");
    h.wait("the end", |h| matches!(h.app.stage(), Stage::Finished { .. } | Stage::Error { .. }));
    assert_eq!(h.app.stage(), &Stage::Finished { warning: None }, "{:?}", h.app.stage());
    // the files of the release channel's latest version, the C++ FirmwareHelper's, then the region bundle
    let got = server.requests.lock().unwrap().clone();
    for f in
        ["directory.json", "full-test.dfu", "core2_firmware-test.tgz", "scripts-test.tgz", "resources-test.tgz", "/regions/api/v0/bundle"]
    {
        assert!(got.iter().any(|r| r.contains(f)), "{f} fetched: {got:?}");
    }
    assert!(!got.iter().any(|r| r.contains("update-test.tgz")), "no update package: {got:?}");
    let chip = boot.finish();
    assert!(chip.booted_firmware, "the chip started its firmware");
    assert_eq!(chip.fus_commands, [0x52, 0x53], "the wireless stack deleted and installed");
    assert_ne!(chip.flash, FakeDfu::new(running()).flash, "the firmware is written");
    // on the firmware: the resources onto the card, the region data, a restart last
    let changes = fake.changes();
    assert!(changes.iter().any(|c| c.contains("StorageWriteRequest /ext/Readme.txt")), "{changes:?}");
    assert!(changes.iter().any(|c| c.contains("StorageWriteRequest /int/.region_data")), "{changes:?}");
    assert!(changes.last().is_some_and(|c| c.contains("SystemRebootRequest")), "{changes:?}");
    h.settle();
    assert!(h.shows("Success!"));
}

#[test]
fn a_damaged_wireless_stack_archive_is_refused_before_the_bootloader_is_opened() {
    let dir = tempdir("rec-core2-files");
    let (_server, net) = update_server(&dir, true);
    let (mut h, boot, fake) = lives("rec-core2", running(), FakeFlipper::standard(), Some(net));
    h.click("REPAIR");
    h.click("Repair");
    h.wait("the end", |h| matches!(h.app.stage(), Stage::Finished { .. } | Stage::Error { .. }));
    let Stage::Error { text, .. } = h.app.stage().clone() else { panic!("{:?}", h.app.stage()) };
    assert!(text.contains("does not match its manifest"), "{text}");
    assert!(boot.finish().log.is_empty(), "not one request reached the bootloader");
    assert_eq!(fake.sessions(), 0);
}

#[test]
fn the_repair_from_files_with_resources_goes_on_to_the_firmware() {
    let (mut h, boot, fake) = lives("rec-files-res", running(), FakeFlipper::standard(), None);
    let fw = files(&h.dir.clone());
    fill(&mut h, &fw, true);
    h.set_field(3, fw.join("flipper-z-any-resources-test.tgz").to_str().unwrap());
    h.click("Repair");
    h.wait("the end", |h| matches!(h.app.stage(), Stage::Finished { .. } | Stage::Error { .. }));
    assert_eq!(h.app.stage(), &Stage::Finished { warning: None }, "{:?}", h.app.stage());
    assert!(boot.finish().booted_firmware);
    let changes = fake.changes();
    // the development region: no region data, and nothing fetched (the test's server address answers nothing)
    assert!(!changes.iter().any(|c| c.contains(".region_data")), "{changes:?}");
    assert!(changes.iter().any(|c| c.contains("StorageWriteRequest /ext/Manifest")), "{changes:?}");
    assert!(changes.last().is_some_and(|c| c.contains("SystemRebootRequest")), "{changes:?}");
}

fn developer(name: &str) -> (Harness, Bootloader, PathBuf) {
    let dir = tempdir(name);
    let boot = Bootloader::start(&dir, running());
    let fw = files(&dir);
    let opts = Options { settings: "developer = true\n".into(), ..Options::default() };
    let mut h = Harness::new(&dir, vec![Found { kind: Kind::RecoverySocket(boot.sock.clone()) }], opts);
    h.wait("recovery mode", |h| h.has("REPAIR"));
    h.click("Developer");
    (h, boot, fw)
}

#[test]
fn the_developer_tab_needs_developer_mode_and_recovery_mode() {
    let (h, boot, _fw) = recovery("dev-hidden", running());
    assert!(!h.shows("Developer"), "no developer tab without developer mode");
    assert!(boot.finish().log.is_empty());
    // a running device: the tab is there, its installs are not
    let dir = tempdir("dev-normal");
    let fake = Fake::start(&dir, FakeFlipper::standard());
    let opts = Options { settings: "developer = true\n".into(), ..Options::default() };
    let mut h = Harness::new(&dir, vec![socket_device(&fake.sock)], opts);
    h.wait("the device", |h| h.shows("Fakeflip"));
    h.click("Developer");
    h.settle();
    assert!(!h.is_enabled("Install wireless fw") && !h.is_enabled("Install FUS firmware"));
    assert!(h.shows_text("put it into recovery mode first"));
}

#[test]
fn the_wireless_stack_install_needs_its_dialog() {
    let (mut h, boot, fw) = developer("dev-radio");
    h.settle();
    h.snapshot("developer");
    h.click("Install wireless fw");
    assert!(h.shows("Install wireless stack?") && h.shows("WARNING! This operation can break your Flipper!"));
    h.set_field(0, fw.join("radio.bin").to_str().unwrap());
    h.click("Cancel");
    h.settle();
    assert_eq!(h.app.stage(), &Stage::Ready);
    h.click("Install wireless fw");
    h.set_field(0, fw.join("radio.bin").to_str().unwrap());
    h.click("Install");
    h.wait("the end", |h| matches!(h.app.stage(), Stage::Finished { .. } | Stage::Error { .. }));
    assert_eq!(h.app.stage(), &Stage::Finished { warning: None }, "{:?}", h.app.stage());
    let chip = boot.finish();
    assert_eq!(chip.fus_commands, [0x52, 0x53], "deleted and installed, nothing else");
    assert!(chip.booted_firmware, "the OS boot mode at the end: the chip starts its firmware");
}

#[test]
fn the_fus_install_writes_at_the_fus_address_after_its_dialog() {
    let (mut h, boot, fw) = developer("dev-fus");
    let image = std::fs::read(fw.join("radio.bin")).unwrap();
    h.click("Install FUS firmware");
    assert!(h.shows("Install FUS?") && h.shows_text("0x080EC000"));
    h.set_field(0, fw.join("radio.bin").to_str().unwrap());
    h.click("Cancel");
    h.settle();
    h.click("Install FUS firmware");
    // an empty file is refused in the dialog
    let empty = h.dir.join("home/empty.bin");
    std::fs::write(&empty, b"").unwrap();
    h.set_field(0, empty.to_str().unwrap());
    h.click("Install");
    h.settle();
    assert!(h.app.dialog().is_some_and(|d| d.error.as_deref().is_some_and(|e| e.contains("is empty"))));
    h.set_field(0, fw.join("radio.bin").to_str().unwrap());
    h.click("Install");
    h.wait("the end", |h| matches!(h.app.stage(), Stage::Finished { .. } | Stage::Error { .. }));
    assert_eq!(h.app.stage(), &Stage::Finished { warning: None }, "{:?}", h.app.stage());
    let chip = boot.finish();
    let at = (flipper_dfu::recovery::FUS_ADDRESS - 0x0800_0000) as usize;
    assert_eq!(&chip.flash[at..at + image.len()], &image[..], "the image at 0x080EC000");
    assert!(chip.booted_firmware);
}
