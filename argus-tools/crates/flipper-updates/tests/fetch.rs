//! `flipper_updates::fetch` against the local fake update server (flipper-fake's `update_server`): HTTPS on
//! 127.0.0.1 with a CA made for the test, never any other host. The directory and region bundle are read
//! through it, every file's SHA-256 is checked, and every way a server can misbehave ends in an error, within
//! the limits.

use std::time::Duration;

use flipper_fake::update_server::{TestCa, UpdateServer};
use flipper_updates::fetch::{Client, Error};
use flipper_updates::{directory, directory_url, region, region_url};

fn setup() -> (tempfile::TempDir, UpdateServer, Client) {
    let dir = tempfile::tempdir().unwrap();
    let fw = dir.path().join("fw");
    flipper_fake::firmware::write(&fw, false).unwrap();
    let ca = TestCa::new();
    let server = UpdateServer::start(&fw, &ca).unwrap();
    let root = ureq::tls::Certificate::from_der(&ca.ca_der).to_owned();
    let client = Client::with_roots(vec![root], Duration::from_secs(3));
    (dir, server, client)
}

#[test]
fn directory_and_files_through_https() {
    let (_dir, server, client) = setup();
    let text = client.get(&directory_url(&server.base()), directory::MAX_DIRECTORY).unwrap();
    let d = directory::parse(&text).unwrap();
    let latest = d.channel("release").unwrap().latest().unwrap();
    assert_eq!(latest.number, "9.9.9");
    let f = latest.file("full_dfu", "f7").unwrap();
    assert!(f.url.starts_with(&server.base()), "{}", f.url);
    let data = client.get_file(f, flipper_updates::fetch::MAX_FILE).unwrap();
    assert_eq!(data, flipper_fake::firmware::firmware());
    // the same file, changed by one bit on the way: refused
    let mut wrong = f.clone();
    wrong.url = format!("{}/test/wrong-flipper-z-f7-full-test.dfu", server.base());
    assert!(matches!(client.get_file(&wrong, flipper_updates::fetch::MAX_FILE), Err(Error::Integrity { .. })));
    // and a file larger than its limit
    assert!(matches!(client.get_file(f, 100), Err(Error::TooLarge { .. })));
    let log = server.requests.lock().unwrap().clone();
    assert_eq!(log[0], "GET /firmware/directory.json");
}

#[test]
fn region_bundle_through_https() {
    let (_dir, server, client) = setup();
    let text = client.get(&region_url(&server.base()), region::MAX_BUNDLE).unwrap();
    let d = region::region_data(&text, b"").unwrap();
    assert_eq!(d.country, b"DE");
    assert_eq!(d.bands.len(), 2);
}

#[test]
fn misbehaving_servers_end_in_errors() {
    let (_dir, server, client) = setup();
    let b = server.base();
    // a body without end, and a length of 1 TB: refused at the limit
    assert!(matches!(client.get(&format!("{b}/test/endless"), 1 << 20), Err(Error::TooLarge { .. })));
    assert!(matches!(client.get(&format!("{b}/test/huge-length"), 1 << 20), Err(Error::TooLarge { .. })));
    // headers, then silence: the client's time runs out
    let t = std::time::Instant::now();
    assert!(matches!(client.get(&format!("{b}/test/stall"), 1 << 20), Err(Error::Network(_))));
    assert!(t.elapsed() < Duration::from_secs(10));
    // redirects to plain http, or for ever
    assert!(matches!(client.get(&format!("{b}/test/redirect-http"), 1 << 20), Err(Error::Network(_))));
    assert!(matches!(client.get(&format!("{b}/test/redirect-loop"), 1 << 20), Err(Error::Network(_))));
    assert!(matches!(client.get(&format!("{b}/firmware/nothing-here"), 1 << 20), Err(Error::Network(_))));
    // never anything but https
    let plain = b.replace("https://", "http://");
    assert!(matches!(client.get(&format!("{plain}/firmware/directory.json"), 1 << 20), Err(Error::Setup(_))));
    // a server whose certificate this client does not trust
    let other = Client::with_roots(vec![ureq::tls::Certificate::from_der(&TestCa::new().ca_der).to_owned()], Duration::from_secs(3));
    assert!(matches!(other.get(&directory_url(&b), 1 << 20), Err(Error::Network(_))));
    let log = server.requests.lock().unwrap().clone();
    // the redirect to http was not followed (https only); the untrusted client never sent its request
    assert!(!log.iter().any(|l| l == "GET /firmware/directory.json"), "{log:?}");
}
