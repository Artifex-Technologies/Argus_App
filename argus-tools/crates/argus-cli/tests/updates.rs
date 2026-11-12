//! argus-cli's update server commands and the region step of `recovery finish`, against the local fake update
//! server (flipper-fake's `update_server`: HTTPS on 127.0.0.1 with a CA made for the test, trusted through
//! `SSL_CERT_FILE`) and, for `finish`, the fake Flipper on a Unix socket. No other host is ever contacted.

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use flipper_fake::update_server::{TestCa, UpdateServer};
use flipper_fake::FakeFlipper;
use md5::{Digest as _, Md5};
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_argus-cli");

fn tempdir() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("u{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Setup {
    dir: PathBuf,
    server: UpdateServer,
    ca: PathBuf,
}

fn setup() -> Setup {
    let dir = tempdir();
    let fw = dir.join("fw");
    flipper_fake::firmware::write(&fw, false).unwrap();
    let ca = TestCa::new();
    let server = UpdateServer::start(&fw, &ca).unwrap();
    let ca_file = dir.join("ca.pem");
    std::fs::write(&ca_file, ca.ca_pem()).unwrap();
    Setup { dir, server, ca: ca_file }
}

/// argus-cli with the fake server and its CA (or, `trust: false`, the system's roots, which do not trust it).
fn cli(s: &Setup, trust: bool, args: &[&str]) -> (i32, Value) {
    let mut c = Command::new(BIN);
    c.env_remove("SSL_CERT_DIR").args(["--update-server", &s.server.base()]).args(args);
    if trust {
        c.env("SSL_CERT_FILE", &s.ca);
    } else {
        c.env_remove("SSL_CERT_FILE");
    }
    let out = c.output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let json = serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("not one JSON object ({e}): {text}"));
    (out.status.code().unwrap(), json)
}

#[test]
fn directory_and_fetch() {
    let s = setup();
    let (st, j) = cli(&s, true, &["updates", "directory"]);
    assert_eq!(st, 0, "{j}");
    assert_eq!(j["channels"][0]["id"], "release");
    assert_eq!(j["channels"][0]["latest"]["version"], "9.9.9");
    let out = s.dir.join("fw.dfu");
    let (st, j) = cli(&s, true, &["updates", "fetch", "release", "full_dfu", "f7", out.to_str().unwrap()]);
    assert_eq!(st, 0, "{j}");
    assert_eq!(std::fs::read(&out).unwrap(), flipper_fake::firmware::firmware());
    // never over an existing file
    let (st, j) = cli(&s, true, &["updates", "fetch", "release", "full_dfu", "f7", out.to_str().unwrap()]);
    assert_eq!((st, j["error"]["code"].as_str()), (1, Some("exists")));
    let (st, j) = cli(&s, true, &["updates", "fetch", "release", "full_dfu", "f18", s.dir.join("x").to_str().unwrap()]);
    assert_eq!((st, j["error"]["code"].as_str()), (1, Some("not_found")));
    assert!(!s.dir.join("x").exists());
}

#[test]
fn region_file() {
    let s = setup();
    let out = s.dir.join("region_data");
    let (st, j) = cli(&s, true, &["region", out.to_str().unwrap()]);
    assert_eq!(st, 0, "{j}");
    assert_eq!(j["region_data"]["country"], "DE");
    let want = flipper_updates::region::region_data(flipper_fake::firmware::region_bundle(Some("DE")).as_bytes(), b"").unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), want.bytes);
    // no country from the server: --country, else the default bands
    std::fs::write(s.dir.join("fw/bundle"), flipper_fake::firmware::region_bundle(None)).unwrap();
    let us = s.dir.join("us");
    let (st, j) = cli(&s, true, &["--country", "US", "region", us.to_str().unwrap()]);
    assert_eq!((st, j["region_data"]["country"].as_str(), j["region_data"]["bands"].as_array().map(Vec::len)), (0, Some("US"), Some(3)));
    let none = s.dir.join("none");
    let (st, j) = cli(&s, true, &["region", none.to_str().unwrap()]);
    assert_eq!((st, j["region_data"]["country"].as_str(), j["region_data"]["bands"].as_array().map(Vec::len)), (0, Some(""), Some(2)));
}

#[test]
fn untrusted_or_plain_servers_are_refused() {
    let s = setup();
    // the system's roots do not trust the test CA: the TLS handshake fails, nothing is written
    let out = s.dir.join("r");
    let (st, j) = cli(&s, false, &["region", out.to_str().unwrap()]);
    // (a system with no root certificates at all stops before connecting: "setup")
    assert!(st == 1 && matches!(j["error"]["code"].as_str(), Some("network" | "setup")), "{j}");
    assert!(!out.exists());
    let plain = s.server.base().replace("https://", "http://");
    let out = Command::new(BIN).args(["--update-server", &plain, "updates", "directory"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(s.server.requests.lock().unwrap().is_empty(), "nothing reached the server");
}

/// `recovery finish` on a device of region 1 (not the development region): the bundle is fetched before the
/// device is touched, and its data lands in /int/.region_data after the resources, before the restart. A bundle
/// that cannot be fetched changes nothing on the device.
#[test]
fn finish_writes_region_data() {
    let s = setup();
    let bundle = s.dir.join("fw/flipper-z-any-resources-test.tgz");
    let run = |fake: FakeFlipper| -> (i32, Value, String) {
        let sock = s.dir.join("f.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let r = listener.accept().ok().map(|(mut c, _)| fake.serve(&mut c).unwrap_or_default());
            (r, fake.dump())
        });
        let (st, j) = cli(
            &s,
            true,
            &["--socket", sock.to_str().unwrap(), "--timeout", "2000", "recovery", "finish", bundle.to_str().unwrap(), "--yes"],
        );
        let (_, dump) = server.join().unwrap();
        (st, j, dump)
    };
    let region1 = || {
        let mut f = FakeFlipper::standard();
        for (k, v) in f.devinfo.iter_mut() {
            if k == "hardware.region.builtin" {
                *v = "1".into();
            }
        }
        f
    };
    let (st, j, dump) = run(region1());
    assert_eq!(st, 0, "{j}");
    assert_eq!(j["firmware"]["region"], 1);
    assert_eq!(j["firmware"]["region_data"]["country"], "DE");
    let want = flipper_updates::region::region_data(flipper_fake::firmware::region_bundle(Some("DE")).as_bytes(), b"").unwrap();
    let line = format!("f /int/.region_data {} {}", want.bytes.len(), hex::encode(Md5::digest(&want.bytes)));
    assert!(dump.lines().any(|l| l == line), "{line} not in\n{dump}");
    // the server has no bundle: nothing on the device changes
    std::fs::remove_file(s.dir.join("fw/bundle")).unwrap();
    let before = region1().dump();
    let (st, j, dump) = run(region1());
    assert_eq!((st, j["error"]["code"].as_str()), (1, Some("network")), "{j}");
    assert_eq!(dump, before);
}
