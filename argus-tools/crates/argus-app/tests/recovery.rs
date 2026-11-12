//! Recovery mode against the fake STM32WB55 bootloader (flipper_fake::dfu) on a Unix socket, across the chip's resets:
//! the repair with local files and leaving recovery mode, each behind its dialog (cancelled: not one request reaches
//! the bootloader), a damaged file refused in the dialog, the partial success when the wireless stack is not installed,
//! and a device in recovery mode on USB, which the app cannot reach (nothing starts). No USB, nothing real flashed.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use argus_app::app::Stage;
use argus_app::discovery::{Found, Kind};
use common::*;
use flipper_fake::dfu::{self, FakeDfu, Faults};

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

fn fill(h: &mut Harness, fw: &Path, radio: bool) {
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
    h.click("REPAIR");
    fill(&mut h, &fw, true);
    h.click("Cancel");
    h.click("Actions");
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
    h.click("REPAIR");
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
    h.click("REPAIR");
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
    h.click("REPAIR");
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
    h.click("REPAIR");
    fill(&mut h, &fw, false);
    h.click("Repair");
    h.settle();
    assert!(h
        .app
        .dialog()
        .is_some_and(|d| d.error.as_deref().is_some_and(|e| e.contains("cannot reach a device in recovery mode over USB"))));
    assert_eq!(h.app.stage(), &Stage::Ready, "nothing started");
}
