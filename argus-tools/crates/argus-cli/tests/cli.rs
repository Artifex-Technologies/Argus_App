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
