//! `argus-cli recovery ...` against the fake DFU bootloader (flipper_fake::dfu) on a Unix socket.
//! Nothing here can reach USB: argus-cli has no USB recovery transport, and the only device is the
//! fake behind `--dfu-socket`.

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use flipper_dfu::wb55::OptionBytes;
use flipper_fake::dfu::{self, flipper_option_bytes, FakeDfu, Faults};
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_argus-cli");

fn tempdir() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("r{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Run {
    status: i32,
    json: Value,
    fake: FakeDfu,
}

/// Runs argus-cli with the fake on `--dfu-socket`; files are (name, bytes) written next to it.
fn run(faults: Faults, files: &[(&str, &[u8])], args: &[&str]) -> Run {
    let dir = tempdir();
    let sock = dir.join("dfu.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let server = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        dfu::serve(FakeDfu::new(faults), s)
    });
    for (name, data) in files {
        std::fs::write(dir.join(name), data).unwrap();
    }
    let args: Vec<String> = args.iter().map(|a| a.replace("{dir}", dir.to_str().unwrap())).collect();
    let out = Command::new(BIN).arg("--dfu-socket").arg(&sock).args(&args).output().unwrap();
    // if the command never connected, let the server go
    let _ = UnixStream::connect(&sock);
    let fake = server.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let json = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("not one JSON object ({e}): {stdout}"));
    Run { status: out.status.code().unwrap(), json, fake }
}

fn firmware(data: &[u8]) -> Vec<u8> {
    // a DfuSe file with one element at the start of the flash (the tests' own writer)
    let mut body = Vec::new();
    body.extend(0x0800_0000u32.to_le_bytes());
    body.extend((data.len() as u32).to_le_bytes());
    body.extend(data);
    let mut target = b"Target\0".to_vec();
    target.extend(1u32.to_le_bytes());
    target.extend([0u8; 255]);
    target.extend((body.len() as u32).to_le_bytes());
    target.extend(1u32.to_le_bytes());
    target.extend(body);
    let mut f = b"DfuSe\x01".to_vec();
    f.extend(((11 + target.len()) as u32).to_le_bytes());
    f.push(1);
    f.extend(target);
    f.extend([0xff, 0xff, 0x11, 0xdf, 0x83, 0x04, 0x1a, 0x01]);
    f.extend(b"UFD\x10");
    let crc = flipper_dfu::dfuse::crc32(&f);
    f.extend(crc.to_le_bytes());
    f
}

#[test]
fn nothing_changes_without_yes() {
    let fw = firmware(&[1; 100]);
    for args in [
        &["recovery", "exit"][..],
        &["recovery", "boot", "dfu"],
        &["recovery", "firmware", "{dir}/fw.dfu"],
        &["recovery", "radio", "{dir}/radio.bin"],
        &["recovery", "fus", "delete"],
        &["recovery", "option-bytes", "{dir}/ob.data"],
    ] {
        let r = run(Faults::default(), &[("fw.dfu", &fw), ("radio.bin", &[1; 10]), ("ob.data", b"BOR_LEV:0x3:rw\n")], args);
        assert_eq!(r.status, 2, "{args:?}");
        assert_eq!(r.json["error"]["code"], "confirm");
        assert_eq!(r.json["checked"], true);
        assert!(r.fake.log.is_empty(), "{args:?} reached the device");
    }
}

#[test]
fn bad_files_are_refused_before_anything_is_opened() {
    let mut fw = firmware(&[1; 100]);
    fw[300] ^= 1;
    for (files, args) in [
        (vec![("fw.dfu", fw.as_slice())], &["recovery", "firmware", "{dir}/fw.dfu", "--yes"][..]),
        (vec![("ob.data", b"RDP:0x1CC:r\n".as_slice())], &["recovery", "option-bytes", "{dir}/ob.data", "--yes"]),
        (vec![("ob.data", b"RDP:0xAA:rw\n".as_slice())], &["recovery", "option-bytes", "{dir}/ob.data", "--yes"]),
        (vec![("radio.bin", b"".as_slice())], &["recovery", "radio", "{dir}/radio.bin", "--yes"]),
        (vec![], &["recovery", "firmware", "{dir}/missing.dfu", "--yes"]),
    ] {
        let r = run(Faults::default(), &files, args);
        assert_eq!(r.status, 1, "{args:?}");
        assert_eq!(r.json["error"]["code"], "file", "{args:?}");
        assert!(r.fake.log.is_empty());
    }
    let r = run(Faults::default(), &[], &["recovery", "bogus", "--yes"]);
    assert_eq!(r.status, 2);
    // no --dfu-socket: nothing to talk to (--port is never used for recovery)
    let out = Command::new(BIN).args(["--port", "/nonexistent/port", "recovery", "info"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn info_reads_only() {
    let r = run(Faults::default(), &[], &["recovery", "info"]);
    assert_eq!(r.status, 0, "{}", r.json);
    assert_eq!(r.json["option_bytes"]["RDP"], 0xAA);
    assert_eq!(r.json["option_bytes"]["SFSA"], 0xD7);
    assert_eq!(r.json["fus"]["status"], 0);
    assert_eq!(r.json["wireless"], "fus_running");
    assert_eq!(r.json["versions"]["wireless"], "1.17.3");
    assert!(r.fake.log.iter().all(|l| !l.starts_with("OUT 1 ") || l.starts_with("OUT 1 0 21")), "only set-address downloads");
    assert_eq!(r.fake.option_bytes, flipper_option_bytes());
}

#[test]
fn changes_with_yes() {
    let data: Vec<u8> = (0..5000).map(|i| (i * 7) as u8).collect();
    let r = run(Faults::default(), &[("fw.dfu", &firmware(&data))], &["recovery", "firmware", "{dir}/fw.dfu", "--yes"]);
    assert_eq!(r.status, 0, "{}", r.json);
    assert_eq!(&r.fake.flash[..data.len()], &data[..]);
    assert_eq!(r.fake.erased_pages.len(), 2);

    let r = run(Faults::default(), &[], &["recovery", "boot", "dfu", "--yes"]);
    assert_eq!(r.status, 0, "{}", r.json);
    let ob = OptionBytes::from_bytes(&r.fake.option_bytes).unwrap();
    assert_eq!((ob.value("nBOOT0").unwrap(), ob.value("nSWBOOT0").unwrap(), ob.value("RDP").unwrap()), (0, 0, 0xAA));
    assert!(r.fake.left);

    let r = run(
        Faults::default(),
        &[("ob.data", b"BOR_LEV:0x3:rw\nSFSA:0xD0:r\nRDP:0xAA:r\n")],
        &["recovery", "option-bytes", "{dir}/ob.data", "--yes"],
    );
    assert_eq!(r.status, 0, "{}", r.json);
    assert_eq!(r.json["written"], true);
    assert_eq!(r.json["fixed"]["BOR_LEV"], 3);
    assert_eq!(r.json["read_only_differ"]["SFSA"], 0xD0);
    let ob = OptionBytes::from_bytes(&r.fake.option_bytes).unwrap();
    assert_eq!((ob.value("BOR_LEV").unwrap(), ob.value("SFSA").unwrap()), (3, 0xD7));

    let r = run(Faults::default(), &[("radio.bin", &[9; 4097])], &["recovery", "radio", "{dir}/radio.bin", "--yes"]);
    assert_eq!(r.status, 0, "{}", r.json);
    assert_eq!(r.json["address"], "0x080d5000");
    assert_eq!(&r.fake.flash[0xD_5000..0xD_5000 + 4097], &[9; 4097][..]);

    let r = run(Faults { fus: Some([0xFF, 0xFE]), ..Faults::default() }, &[], &["recovery", "fus", "start", "--yes"]);
    assert_eq!(r.status, 0, "{}", r.json);
    let r = run(Faults::default(), &[], &["recovery", "fus", "delete", "--yes"]);
    assert_eq!(r.fake.fus_commands, [0x52]);
    let r = run(Faults::default(), &[], &["recovery", "exit", "--yes"]);
    assert_eq!(r.status, 0);
    assert!(r.fake.left);
}

#[test]
fn device_errors_exit_1() {
    let r = run(Faults { stuck_busy: true, ..Faults::default() }, &[], &["recovery", "boot", "normal", "--yes"]);
    assert_eq!(r.status, 1);
    assert_eq!(r.json["error"]["code"], "device");
    let r = run(
        Faults { empty_upload: true, ..Faults::default() },
        &[("ob.data", b"BOR_LEV:0x3:rw\n")],
        &["recovery", "option-bytes", "{dir}/ob.data", "--yes"],
    );
    assert_eq!(r.status, 1);
    assert!(r.fake.log.iter().all(|l| !l.starts_with("OUT 1 2 ")), "nothing written over unread option bytes");
}

const OB_DATA: &[u8] = b"RDP:0xAA:r\nBOR_LEV:0x4:rw\nnBOOT0:0x1:rw\nnSWBOOT0:0x1:rw\nSFSA:0xD7:r\n";

/// A repair against the fake across its resets: the socket goes away after each reset and comes
/// back while the chip is in its bootloader.
fn repair(faults: Faults, args: &[&str], files: &[(&str, &[u8])]) -> Run {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    let dir = tempdir();
    let sock = dir.join("dfu.sock");
    let stop = Arc::new(AtomicBool::new(false));
    let (path, flag) = (sock.clone(), stop.clone());
    let server = thread::spawn(move || dfu::serve_with_resets(FakeDfu::new(faults), &path, std::time::Duration::from_millis(20), &flag));
    while !sock.exists() {
        thread::sleep(std::time::Duration::from_millis(2));
    }
    for (name, data) in files {
        std::fs::write(dir.join(name), data).unwrap();
    }
    let args: Vec<String> = args.iter().map(|a| a.replace("{dir}", dir.to_str().unwrap())).collect();
    let out = Command::new(BIN).arg("--dfu-socket").arg(&sock).args(["--poll-ms", "10"]).args(&args).output().unwrap();
    stop.store(true, Ordering::SeqCst);
    let fake = server.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let json = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("not one JSON object ({e}): {stdout}"));
    Run { status: out.status.code().unwrap(), json, fake }
}

#[test]
fn full_repair_across_resets() {
    let fw: Vec<u8> = (0..9000).map(|i| (i * 13) as u8).collect();
    let radio = vec![0x5A; 3 * 4096 + 5];
    let files: [(&str, &[u8]); 3] = [("fw.dfu", &firmware(&fw)), ("ob.data", OB_DATA), ("radio.bin", &radio)];
    let args = ["recovery", "repair", "{dir}/fw.dfu", "{dir}/ob.data", "{dir}/radio.bin", "--yes"];

    // the wireless stack running, as on a Flipper in normal use
    let r = repair(Faults { fus_model: true, fus: Some([0xFF, 0xFE]), ..Faults::default() }, &args, &files);
    assert_eq!(r.status, 0, "{}", r.json);
    assert_eq!(r.json["radio"]["installed"], true, "{}", r.json);
    assert_eq!(r.json["option_bytes_written"], true);
    assert_eq!(r.json["fixed"]["nBOOT0"], 1);
    let f = &r.fake;
    assert_eq!(&f.flash[..fw.len()], &fw[..]);
    assert_eq!(&f.flash[0xD_3000..0xD_3000 + radio.len()], &radio[..]);
    assert_eq!(f.fus_commands, [0x52, 0x53]);
    assert!(f.booted_firmware, "the chip boots its firmware at the end");
    // boot mode, FUS start, upgrade, option bytes
    assert_eq!(f.resets, 4);
    let ob = OptionBytes::from_bytes(&f.option_bytes).unwrap();
    assert_eq!((ob.value("nBOOT0").unwrap(), ob.value("nSWBOOT0").unwrap(), ob.value("RDP").unwrap()), (1, 1, 0xAA));

    // a FUS stuck in a state the flow does not know: the wireless stack step fails, the repair goes on
    // and the result is a partial success, never plain success (exit 3)
    let r = repair(Faults { fus_model: true, fus: Some([0x30, 0x02]), ..Faults::default() }, &args, &files);
    assert_eq!(r.status, 3, "{}", r.json);
    assert_eq!((r.json["ok"].as_bool(), r.json["partial"].as_bool()), (Some(false), Some(true)), "{}", r.json);
    assert_eq!(r.json["error"]["code"], "radio_not_installed");
    assert!(r.json["error"]["message"].as_str().unwrap().starts_with("repaired, but the radio stack was not installed: "), "{}", r.json);
    assert_eq!(r.json["radio"]["installed"], false);
    assert!(r.json["radio"]["error"].as_str().unwrap().contains("FUS"), "{}", r.json);
    assert_eq!(&r.fake.flash[..fw.len()], &fw[..]);
    assert!(r.fake.booted_firmware);

    // without a radio image; and nothing at all without --yes
    let r = repair(Faults::default(), &args[..4].iter().copied().chain(["--yes"]).collect::<Vec<_>>(), &files);
    assert_eq!(r.status, 0, "{}", r.json);
    assert!(r.json["radio"].is_null());
    let r = repair(Faults::default(), &args[..5], &files);
    assert_eq!(r.status, 2);
    assert!(r.fake.log.is_empty());
}
