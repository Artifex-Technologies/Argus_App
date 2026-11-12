//! The argus-cli binary against the fake Flipper on a Unix socket: stdout, exit status, and
//! the bytes it put on the wire compared with the frames the C++ app's own encoder produces
//! (crates/flipper-rpc/tests/fixtures/nanopb_frames.txt).

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use flipper_fake::{FakeFlipper, Fault, Transcript};
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_argus-cli");

struct Run {
    status: i32,
    json: Value,
    raw: Vec<u8>,
    stderr: String,
    wire: Vec<u8>,
}

fn tempdir() -> PathBuf {
    // A private directory under the target dir for the socket (short: socket paths are limited
    // to 107 bytes); no shared /tmp names to race on.
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let dir = base.join(format!("c{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_with(fake: FakeFlipper, args: &[&str]) -> Run {
    let dir = tempdir();
    let sock = dir.join("flipper.sock");
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).unwrap();
    let server = thread::spawn(move || listener.accept().ok().map(|(mut s, _)| fake.serve(&mut s).unwrap_or_default()));
    let mut full = vec!["--socket", sock.to_str().unwrap(), "--timeout", "500"];
    full.extend_from_slice(args);
    let out = Command::new(BIN).args(&full).output().unwrap();
    let t: Transcript = server.join().unwrap().unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    parse(out, t.received)
}

fn parse(out: Output, wire: Vec<u8>) -> Run {
    let stdout = String::from_utf8(out.stdout.clone()).unwrap();
    let json = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("stdout is not one JSON object ({e}): {stdout}"));
    Run { status: out.status.code().unwrap(), json, raw: out.stdout, stderr: String::from_utf8(out.stderr).unwrap(), wire }
}

fn cpp_frame(name: &str) -> Vec<u8> {
    let text = include_str!("../../flipper-rpc/tests/fixtures/nanopb_frames.txt");
    let hex = text.lines().find_map(|l| l.strip_prefix(name).and_then(|r| r.strip_prefix(' '))).unwrap();
    (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn info_reads_devinfo_and_sends_what_the_cpp_app_sends() {
    let r = run_with(FakeFlipper::standard(), &["info"]);
    assert_eq!(r.status, 0, "{}", r.json);
    assert_eq!(r.json["ok"], true);
    assert_eq!(r.json["protobuf"]["minor"], 25);
    assert_eq!(r.json["source"], "property");
    assert_eq!(r.json["info"]["hardware.name"], "Fakeflip");
    assert_eq!(r.json["info"]["firmware.origin.fork"], "Argus");
    assert_eq!(r.json["info"].as_object().unwrap().len(), FakeFlipper::standard().devinfo.len());
    assert!(r.stderr.is_empty());
    // The C++ app's VCPDeviceInfoHelper starts with the same two requests, ids 1 and 2. (It then
    // goes on to storage info, stat, and reads and SETS the clock; the Rust command stops here.)
    let mut want = b"start_rpc_session\r".to_vec();
    want.extend(cpp_frame("req_protobuf_version"));
    want.extend(cpp_frame("req_property_devinfo"));
    assert_eq!(r.wire, want);
}

#[test]
fn info_on_old_firmware_uses_the_legacy_request() {
    let fake = FakeFlipper { protobuf_version: (0, 13), ..FakeFlipper::standard() };
    let r = run_with(fake, &["info"]);
    assert_eq!(r.status, 0);
    assert_eq!(r.json["source"], "legacy");
    assert_eq!(r.json["info"]["hardware_name"], "Fakeflip");
}

#[test]
fn ls_lists_and_sends_one_list_request() {
    let r = run_with(FakeFlipper::standard(), &["ls", "/ext"]);
    assert_eq!(r.status, 0, "{}", r.json);
    let names: Vec<&str> = r.json["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["apps", "nfc", "subghz", "empty", "Manifest"]);
    assert_eq!(r.json["entries"][4]["type"], "file");
    assert_eq!(r.json["entries"][4]["size"], 48213);
    let mut want = b"start_rpc_session\r".to_vec();
    want.extend([0x0a, 0x08, 0x01, 0x3a, 0x06, 0x0a, 0x04]);
    want.extend(b"/ext");
    assert_eq!(r.wire, want);
}

#[test]
fn ls_of_a_long_directory_follows_has_next() {
    let r = run_with(FakeFlipper::standard(), &["ls", "/ext/subghz"]);
    assert_eq!(r.json["entries"].as_array().unwrap().len(), 19);
}

#[test]
fn device_errors_exit_1_with_the_cpp_wording() {
    let r = run_with(FakeFlipper::standard(), &["ls", "/ext/nope"]);
    assert_eq!(r.status, 1);
    assert_eq!(r.json["ok"], false);
    assert_eq!(r.json["error"]["code"], "device");
    assert_eq!(r.json["error"]["message"], "Device replied with error: File/directory does not exist");
}

#[test]
fn faults_exit_1_with_a_code() {
    for (fault, code) in
        [(Fault::Silent, "timeout"), (Fault::GarbageFrame, "frame"), (Fault::OversizeFrame, "frame"), (Fault::CloseMidFrame, "closed")]
    {
        let label = format!("{fault:?}");
        let r = run_with(FakeFlipper { fault, ..FakeFlipper::standard() }, &["info"]);
        assert_eq!(r.status, 1, "{label}");
        assert_eq!(r.json["error"]["code"], code, "{label}: {}", r.json);
    }
}

#[test]
fn hostile_names_cannot_reach_the_terminal_raw() {
    let mut fake = FakeFlipper::standard();
    // The firmware drops non-ASCII names; DEL and ESC are ASCII and get through.
    fake.nodes.push(flipper_fake::Node { path: "/ext/empty/x\u{1b}[2J\u{7f}".into(), dir: false, size: 1 });
    let r = run_with(fake, &["ls", "/ext/empty"]);
    assert_eq!(r.status, 0);
    assert_eq!(r.json["entries"][0]["name"], "x\u{1b}[2J\u{7f}");
    // The JSON decodes to the name, but the printed text carries no control bytes.
    assert!(r.raw.iter().all(|b| (*b >= 0x20 && *b != 0x7f) || *b == b'\n'), "{:?}", String::from_utf8_lossy(&r.raw));
}

fn usage(args: &[&str]) -> Run {
    let out = Command::new(BIN).args(args).output().unwrap();
    parse(out, Vec::new())
}

#[test]
fn usage_errors_exit_2_and_touch_nothing() {
    for args in [
        &["info"][..],
        &["ls"],
        &["frobnicate"],
        &["--socket"],
        &["--port", "/dev/null", "--socket", "/x", "info"],
        &["--timeout", "0", "info"],
        &["--bogus", "info"],
    ] {
        let r = usage(args);
        assert_eq!(r.status, 2, "{args:?}");
        assert_eq!(r.json["error"]["code"], "usage", "{args:?}");
    }
}

#[test]
fn only_a_serial_device_or_socket_is_opened() {
    // A regular file is neither a serial device nor a socket; nothing is opened.
    let dir = tempdir();
    let file = dir.join("not-a-port");
    std::fs::write(&file, b"").unwrap();
    let r = usage(&["--port", file.to_str().unwrap(), "info"]);
    assert_eq!(r.status, 1);
    assert_eq!(r.json["error"]["code"], "invalid");
    let r = usage(&["--socket", file.to_str().unwrap(), "info"]);
    assert_eq!(r.json["error"]["code"], "invalid");
    let missing: &Path = &dir.join("missing");
    let r = usage(&["--socket", missing.to_str().unwrap(), "info"]);
    assert_eq!(r.json["error"]["code"], "io");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn version_is_the_app_version() {
    let r = usage(&["version"]);
    assert_eq!(r.status, 0);
    assert_eq!(r.json["version"], include_str!("../../../../VERSION").trim());
}

fn node(path: &str) -> flipper_fake::Node {
    FakeFlipper::standard().nodes.into_iter().find(|n| n.path == path).unwrap()
}

#[test]
fn stat_df_md5_date() {
    let r = run_with(FakeFlipper::standard(), &["stat", "/ext/Manifest"]);
    assert_eq!((r.status, &r.json["exists"], &r.json["entry"]["size"]), (0, &Value::Bool(true), &Value::from(48213)));
    let r = run_with(FakeFlipper::standard(), &["stat", "/ext/nope"]);
    assert_eq!((r.status, &r.json["exists"]), (0, &Value::Bool(false)));
    let r = run_with(FakeFlipper::standard(), &["df", "/ext"]);
    assert_eq!(r.json["total"], 31_902_400_512u64);
    let r = run_with(FakeFlipper { sd_card: false, ..FakeFlipper::standard() }, &["df", "/ext"]);
    assert_eq!(r.json["present"], false);
    let r = run_with(FakeFlipper::standard(), &["md5", "/int/favorites.txt"]);
    assert_eq!(r.json["md5"].as_str().unwrap().len(), 32);
    let r = run_with(FakeFlipper::standard(), &["date"]);
    assert_eq!(r.json["datetime"], "2026-11-09T12:34:56");
    let r = run_with(FakeFlipper::standard(), &["md5", "/ext/nope"]);
    assert_eq!((r.status, &r.json["error"]["code"]), (1, &Value::from("device")));
}

#[test]
fn pull_and_backup_write_new_files_only() {
    let dir = tempdir();
    let dest = dir.join("int");
    let r = run_with(FakeFlipper::standard(), &["pull", "/int", dest.to_str().unwrap()]);
    assert_eq!(r.status, 0, "{}", r.json);
    assert_eq!(r.json["files"], 4);
    assert_eq!(std::fs::read(dest.join("sub/deep.txt")).unwrap(), node("/int/sub/deep.txt").content());
    // a second pull to the same place is refused and changes nothing
    let r = run_with(FakeFlipper::standard(), &["pull", "/int", dest.to_str().unwrap()]);
    assert_eq!((r.status, &r.json["error"]["code"]), (1, &Value::from("local")));

    let out = dir.join("backup.tgz");
    let r = run_with(FakeFlipper::standard(), &["backup", out.to_str().unwrap()]);
    assert_eq!(r.status, 0, "{}", r.json);
    assert_eq!((r.json["files"].as_u64(), r.json["dirs"].as_u64()), (Some(4), Some(2)));
    assert!(out.is_file());
    let r = run_with(FakeFlipper::standard(), &["backup", out.to_str().unwrap()]);
    assert_eq!(r.status, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pull_reports_hostile_names() {
    let dir = tempdir();
    let mut fake = FakeFlipper::standard();
    fake.hostile.push(("/int".into(), "../../outside".into(), false));
    let dest = dir.join("int");
    let r = run_with(fake, &["pull", "/int", dest.to_str().unwrap()]);
    assert_eq!(r.status, 0);
    assert_eq!(r.json["refused"][0], "/int/\"../../outside\"");
    assert!(!dir.join("outside").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn new_commands_check_their_arguments() {
    for args in [&["stat"][..], &["df"], &["md5"], &["pull", "/int"], &["backup"], &["date", "x"]] {
        let r = usage(args);
        assert_eq!(r.status, 2, "{args:?}");
    }
}

/// argus-cli against the fake over a real pseudo-terminal, as a serial port (`--port`).
fn run_pty(args: &[&str]) -> (Run, Vec<String>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let fake = FakeFlipper::standard();
        let (ts, r) = flipper_fake::pty::serve_pty(&fake, 1, |name| tx.send(name.to_string()).unwrap());
        r.unwrap();
        ts.iter().flat_map(|t| t.requests.iter().map(flipper_fake::describe)).collect::<Vec<_>>()
    });
    let port = rx.recv().unwrap();
    assert!(port.starts_with("/dev/pts/"), "only our own pseudo-terminal is opened: {port}");
    let mut full = vec!["--port", port.as_str(), "--timeout", "3000"];
    full.extend_from_slice(args);
    let out = Command::new(BIN).args(&full).output().unwrap();
    let requests = server.join().unwrap();
    (parse(out, Vec::new()), requests)
}

#[test]
fn over_a_pseudo_terminal() {
    let (r, requests) = run_pty(&["md5", "/int/favorites.txt"]);
    assert_eq!(r.status, 0, "{} {}", r.json, r.stderr);
    assert_eq!(requests, ["1 StorageMd5sumRequest /int/favorites.txt"]);

    let dir = tempdir();
    let out = dir.join("b.tgz");
    let (r, requests) = run_pty(&["backup", out.to_str().unwrap()]);
    assert_eq!(r.status, 0, "{}", r.json);
    assert_eq!(requests.first().map(String::as_str), Some("1 StorageListRequest /int"));
    assert!(out.is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn capture_pull_into_a_datastore() {
    let dir = tempdir();
    let db = dir.join("chymaera.db");
    let fake = FakeFlipper::standard().with_captures();
    let r = run_with(fake.clone(), &["--datastore", db.to_str().unwrap(), "capture", "pull", "/ext/nfc"]);
    assert_eq!(r.status, 0, "{}", r.json);
    assert_eq!(r.json["new"], 3);
    assert_eq!(r.json["duplicates"], 0);
    let r = run_with(fake.clone(), &["--datastore", db.to_str().unwrap(), "capture", "pull-all"]);
    assert_eq!(r.status, 0, "{}", r.json);
    assert_eq!((r.json["new"].as_u64(), r.json["duplicates"].as_u64()), (Some(23), Some(3)));
    assert_eq!(r.json["missing_roots"].as_array().unwrap().len(), 5);

    // refused before the device is opened: no socket is even connected
    let r = usage(&["--socket", "/nonexistent", "--datastore", db.to_str().unwrap(), "capture", "pull", "/int"]);
    assert_eq!((r.status, &r.json["error"]["code"]), (1, &Value::from("not_allowed")));
    let r = usage(&["capture", "frob"]);
    assert_eq!(r.status, 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn screen_writes_pbm_frames_into_a_new_folder() {
    let dir = tempdir();
    let out = dir.join("shots");
    let fake = FakeFlipper { screen_frames: 2, fault: flipper_fake::Fault::ShortFrame, ..FakeFlipper::standard() };
    let r = run_with(fake, &["--frames", "2", "screen", out.to_str().unwrap()]);
    assert_eq!(r.status, 0, "{}", r.json);
    assert_eq!((r.json["frames"].as_u64(), r.json["dropped"].as_u64()), (Some(2), Some(1)));
    let pbm = std::fs::read(out.join("frame-0002.pbm")).unwrap();
    let frame = flipper_rpc::screen::Frame { data: flipper_fake::screen_frame(1), orientation: 0 };
    assert_eq!(pbm, frame.to_pbm());
    // the folder exists now: refused before the device is opened
    let r = usage(&["--socket", "/nonexistent.sock", "screen", out.to_str().unwrap()]);
    assert_eq!((r.status, &r.json["error"]["code"]), (1, &Value::from("exists")));
    let r = usage(&["--socket", "/nonexistent.sock", "--frames", "0", "screen", "x"]);
    assert_eq!(r.status, 2);
    let _ = std::fs::remove_dir_all(&dir);
}
