//! The console: capture pulls from the fake into the test's own datastore (only the capture folders; secrets never
//! read; nothing changed on the device), the capture list, notes and sessions in the event log, and the Sarina control
//! API started and stopped from the app (on a free port with the test's token, never 44700), answering over loopback,
//! with the app's device attached: `device.status` and `capture.pull` run on the app's own device worker.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

use common::*;
use flipper_fake::FakeFlipper;
use flipper_rpc::proto::pb::main::Content;

fn console(name: &str) -> (Harness, Fake) {
    let dir = tempdir(name);
    let fake = Fake::start(&dir, FakeFlipper::standard().with_captures());
    let opts = Options { settings: "api_port = 0\n".into(), ..Options::default() };
    let mut h = Harness::new(&dir, vec![socket_device(&fake.sock)], opts);
    h.wait("the device's name", |h| h.shows("Fakeflip"));
    h.click("CONSOLE");
    h.wait("the console", |h| h.has("Pull all captures"));
    (h, fake)
}

fn ask(port: u16, line: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(line.as_bytes()).unwrap();
    s.write_all(b"\n").unwrap();
    let mut answer = String::new();
    BufReader::new(s).read_line(&mut answer).unwrap();
    answer
}

#[test]
fn pull_all_reads_only_the_capture_folders_and_lists_what_came() {
    let (mut h, fake) = console("pull");
    assert!(h.shows("No active session") && h.shows("API off"));
    h.click("Start session");
    h.wait("the session", |h| h.has("End session"));
    h.click("Pull all captures");
    h.wait("the pull to end", |h| h.has("Pull all captures") && h.shows_text("Pulled all capture folders"));
    assert!(h.shows("Captures (26)"), "the 26 capture files of the fake (as compare-cli.sh counts them)");
    assert!(h.shows("garage.sub") && h.shows("door.rfid"));
    let read: Vec<String> = fake
        .requests()
        .iter()
        .filter_map(|m| match &m.content {
            Some(Content::StorageReadRequest(r)) => Some(r.path.clone()),
            _ => None,
        })
        .collect();
    assert!(!read.iter().any(|p| p.starts_with("/ext/u2f") || p.contains("/assets/")), "secrets and assets stay: {read:?}");
    assert!(fake.changes().is_empty(), "a pull only reads");
    // again: nothing new
    h.click("Pull NFC");
    h.wait("the second pull", |h| h.shows_text("0 new"));
    assert!(h.shows("Captures (26)"));
}

#[test]
fn a_note_goes_into_the_event_log() {
    let (mut h, _fake) = console("note");
    h.set_field(0, "door at the back is open");
    h.click("Add");
    h.wait("the note", |h| h.shows_text("door at the back is open"));
    h.click("Clear");
    h.wait("an empty log", |h| !h.shows_text("door at the back is open"));
}

#[test]
fn the_control_api_starts_and_stops_from_the_app() {
    let (mut h, _fake) = console("api");
    h.click("Start API");
    h.wait("the API", |h| h.app.api_port().is_some() && h.has("Stop API"));
    let port = h.app.api_port().unwrap();
    assert_ne!(port, 44700, "the tests never take the real port");
    assert!(h.shows(&format!("API :{port}")));
    let pong = ask(port, r#"{"cmd":"ping","id":1,"token":"test-token"}"#);
    assert!(pong.contains("\"pong\":true"), "{pong}");
    let refused = ask(port, r#"{"cmd":"ping","id":2,"token":"wrong"}"#);
    assert!(refused.contains("\"ok\":false"), "{refused}");
    // the app's console and the API share one event log
    h.set_field(0, "seen by Sarina");
    h.click("Add");
    h.step();
    let tail = ask(port, r#"{"cmd":"log.tail","id":3,"token":"test-token","limit":5}"#);
    assert!(tail.contains("seen by Sarina"), "{tail}");
    h.click("Stop API");
    h.wait("the API to stop", |h| h.has("Start API"));
    for _ in 0..50 {
        if TcpStream::connect(("127.0.0.1", port)).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the API still listens on {port}");
}

fn start_api(h: &mut Harness) -> u16 {
    h.click("Start API");
    h.wait("the API", |h| h.app.api_port().is_some() && h.has("Stop API"));
    h.app.api_port().unwrap()
}

#[test]
fn the_api_reaches_the_apps_device() {
    let (mut h, fake) = console("api-device");
    let port = start_api(&mut h);
    let status = ask(port, r#"{"cmd":"device.status","id":1,"token":"test-token"}"#);
    assert!(status.contains(r#""connected":true"#) && status.contains(r#""name":"Fakeflip""#), "{status}");
    // only the capture folders, as everywhere
    let refused = ask(port, r#"{"cmd":"capture.pull","id":2,"token":"test-token","path":"/int"}"#);
    assert!(refused.contains(r#""ok":false"#) && refused.contains("only the capture folders"), "{refused}");
    let started = ask(port, r#"{"cmd":"capture.pull","id":3,"token":"test-token","path":"/ext/nfc"}"#);
    assert!(started.contains(r#""started":true"#), "{started}");
    // the pull runs on the app's worker; the console shows its steps and what came
    h.wait("the API's pull to end", |h| h.shows_text("Pulled /ext/nfc") && !h.app.pulling());
    assert!(h.shows_text("Pulling /ext/nfc from device"));
    let rows = h.widgets().iter().filter(|w| w.label == "nfc").count();
    assert!(rows > 0, "the NFC captures are listed");
    let tail = ask(port, r#"{"cmd":"log.tail","id":4,"token":"test-token","limit":5}"#);
    assert!(tail.contains("Pulled /ext/nfc"), "{tail}");
    // the console's own pull goes through the same device, and the API sees it
    h.click("Pull all captures");
    h.wait("the console's pull", |h| h.shows_text("Pulled all capture folders") && !h.app.pulling());
    assert!(h.shows("Captures (26)"));
    assert!(fake.changes().is_empty(), "pulls only read");
    let list = ask(port, r#"{"cmd":"capture.list","id":5,"token":"test-token","limit":100}"#);
    assert_eq!(list.matches(r#""source_path""#).count(), 26, "{list}");
    h.click("Stop API");
    h.wait("the API to stop", |h| h.has("Start API"));
}

#[test]
fn without_a_device_the_api_says_so() {
    let dir = tempdir("api-nodev");
    let opts = Options { settings: "api_port = 0\n".into(), ..Options::default() };
    let mut h = Harness::new(&dir, Vec::new(), opts);
    h.click("CONSOLE");
    let port = start_api(&mut h);
    let status = ask(port, r#"{"cmd":"device.status","id":1,"token":"test-token"}"#);
    assert!(status.contains(r#""connected":false"#), "{status}");
    let pull = ask(port, r#"{"cmd":"capture.pull","id":2,"token":"test-token","path":"/ext/nfc"}"#);
    assert!(pull.contains(r#""ok":false"#), "{pull}");
    h.step();
    assert!(h.shows_text("Cannot pull /ext/nfc: no device attached"));
}
