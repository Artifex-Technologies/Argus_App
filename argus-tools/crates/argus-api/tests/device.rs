//! A Flipper attached to the Rust bridge, the fake standing in for it over a socket pair (no
//! device, no port): device.status, an asynchronous pull-all whose steps reach log.tail, a second
//! pull refused while the first runs, and a device that goes away.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use argus_api::bridge::Bridge;
use argus_api::device::Device;
use argus_api::qjson::{self, Object, Value};
use flipper_fake::FakeFlipper;

fn req(bridge: &mut Bridge, cmd: &str, extra: &[(&str, Value)]) -> String {
    let mut o = Object::new().with("cmd", Value::from(cmd));
    for (k, v) in extra {
        o.insert(k, v.clone());
    }
    String::from_utf8(qjson::to_compact(&bridge.handle(&o).into())).unwrap()
}

fn tempdir(tag: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("dev-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Polls the device until `done` holds for the log, for at most 20 s.
fn wait_for(bridge: &mut Bridge, done: impl Fn(&str) -> bool) -> String {
    let start = Instant::now();
    loop {
        bridge.poll_device();
        let tail = req(bridge, "log.tail", &[("limit", Value::Int(1000))]);
        if done(&tail) {
            return tail;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "timed out; log: {tail}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn pull_all_runs_in_the_background_and_lands_in_the_datastore() {
    let dir = tempdir("pull");
    let db = dir.join("chymaera.db");
    let mut bridge = Bridge::new(&db, dir.join("captures"));
    assert_eq!(req(&mut bridge, "device.status", &[]), r#"{"connected":false,"name":"","ok":true}"#);

    let (a, mut b) = UnixStream::pair().unwrap();
    let fake = FakeFlipper::standard().with_captures();
    let server = std::thread::spawn(move || fake.serve(&mut b));
    bridge.attach_device(Device::connect(a, Duration::from_millis(2000), db.clone()).unwrap());
    assert_eq!(req(&mut bridge, "device.status", &[]), r#"{"connected":true,"name":"Fakeflip","ok":true}"#);

    let started = req(&mut bridge, "capture.pullAll", &[]);
    assert!(started.starts_with(r#"{"ok":true,"paths":["/ext/nfc","#) && started.ends_with(r#""started":true}"#), "{started}");
    // a second pull while the first runs is refused, as in the C++ app
    let again = req(&mut bridge, "capture.pull", &[("path", Value::from("/ext/nfc"))]);
    assert_eq!(again, r#"{"error":"no device attached or staging failed","ok":false}"#);
    let tail = wait_for(&mut bridge, |t| t.contains("Pulled all capture folders: "));
    assert!(tail.contains("Cannot pull /ext/nfc: a pull is already running"), "{tail}");
    assert!(tail.contains(r#""message":"Pulling all capture folders from device…""#));
    assert!(tail.contains("Recorded capture: NFC"));
    let list = req(&mut bridge, "capture.list", &[("limit", Value::Int(100))]);
    assert!(list.contains(r#""source_path":"/ext/nfc/office.nfc""#) && !list.contains("u2f"), "{list}");

    // the same again: nothing new
    assert!(req(&mut bridge, "capture.pull", &[("path", Value::from("/ext/nfc"))]).contains(r#""started":true"#));
    let tail = wait_for(&mut bridge, |t| t.contains("Pulled /ext/nfc: 0 new"));
    assert!(tail.contains("already stored"));

    drop(bridge); // closes the session; the fake's serve returns
    server.join().unwrap().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_device_that_goes_away_is_detached() {
    let dir = tempdir("gone");
    let db = dir.join("chymaera.db");
    let mut bridge = Bridge::new(&db, dir.join("captures"));
    let (a, b) = UnixStream::pair().unwrap();
    let mut b2 = b.try_clone().unwrap();
    let fake = FakeFlipper::standard();
    let server = std::thread::spawn(move || fake.serve(&mut b2));
    bridge.attach_device(Device::connect(a, Duration::from_millis(500), db).unwrap());
    // unplug
    b.shutdown(std::net::Shutdown::Both).unwrap();
    let tail = wait_for(&mut bridge, |t| t.contains("Device detached"));
    assert!(tail.contains(r#""message":"Device offline""#), "{tail}");
    assert_eq!(req(&mut bridge, "device.status", &[]), r#"{"connected":false,"name":"","ok":true}"#);
    let r = req(&mut bridge, "capture.pullAll", &[]);
    assert!(r.contains("no device attached"), "{r}");
    let _ = server.join();
    let _ = std::fs::remove_dir_all(&dir);
}
