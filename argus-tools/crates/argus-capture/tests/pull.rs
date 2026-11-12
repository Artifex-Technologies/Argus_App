//! Capture pull against the fake Flipper: only the capture folders are read, `assets` and
//! secrets never; each capture is recorded once; a repeat pull stores nothing new.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use argus_capture::store::Store;
use argus_capture::sync::{pull, PullError, CAPTURE_ROOTS};
use flipper_fake::{describe, FakeFlipper, Fault};
use flipper_rpc::transfer::Limits;
use flipper_rpc::Session;

fn tempdir() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cap{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Pulls `roots` from `fake` into the datastore at `db`; returns the result and the requests.
fn run(
    fake: FakeFlipper,
    db: PathBuf,
    roots: &'static [&'static str],
) -> (Result<argus_capture::sync::PullReport, PullError>, Vec<String>) {
    let (a, mut b) = UnixStream::pair().unwrap();
    let server = thread::spawn(move || fake.serve(&mut b).unwrap_or_default());
    let r = {
        let mut s = Session::start(a, Duration::from_millis(500)).unwrap();
        let store = Store::open(&db).unwrap();
        pull(&mut s, &store, roots, "test", &Limits::default())
    };
    let t = server.join().unwrap();
    (r, t.requests.iter().map(describe).collect())
}

#[test]
fn pull_all_reads_only_capture_folders_and_records_each_once() {
    let dir = tempdir();
    let db = dir.join("chymaera.db");
    let (r, requests) = run(FakeFlipper::standard().with_captures(), db.clone(), CAPTURE_ROOTS);
    let r = r.unwrap();
    // /ext/nfc: badge.nfc (pattern), office.nfc, full.nfc; /ext/subghz: 19 signals + garage.sub;
    // infrared, lfrfid, wifi_scan: one each. The assets dictionary and /ext/u2f are never read.
    assert_eq!(r.new, 3 + 20 + 3, "{r:?}");
    assert!(r.failed.is_empty());
    assert!(requests.iter().all(|q| !q.contains("/assets/") || q.contains("StorageListRequest")), "{requests:?}");
    assert!(requests.iter().all(|q| !q.contains("u2f")));
    assert_eq!(
        r.missing_roots,
        ["/ext/ibutton", "/ext/apps_data/marauder/pcaps", "/ext/apps_data/marauder/logs", "/ext/rf_monitor", "/ext/ld2450"]
    );

    let store = Store::open(&db).unwrap();
    let caps = store.captures().unwrap();
    assert_eq!(caps.len(), 26);
    let sub = caps.iter().find(|c| c.source_path == "/ext/subghz/garage.sub").unwrap();
    assert_eq!(sub.kind, "subghz");
    assert!(sub.meta.contains(r#""kind":"subghz""#));
    assert_eq!(sub.sha256.len(), 64);
    assert!(std::path::Path::new(&sub.local_path).is_file());
    assert_eq!(caps.iter().find(|c| c.source_path.ends_with("scan_1.csv")).unwrap().kind, "csv");
    assert_eq!(caps.iter().find(|c| c.source_path.ends_with("tv.ir")).unwrap().kind, "generic");

    // a second pull stores nothing new and leaves no staging folder behind
    let (r2, _) = run(FakeFlipper::standard().with_captures(), db.clone(), CAPTURE_ROOTS);
    let r2 = r2.unwrap();
    assert_eq!((r2.new, r2.duplicates), (0, 26));
    assert!(!r2.staging.exists());
    assert_eq!(Store::open(&db).unwrap().captures().unwrap().len(), 26);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn only_capture_folders_may_be_pulled() {
    let dir = tempdir();
    for bad in [&["/int"][..], &["/ext/u2f"], &["/ext/nfc/../u2f"], &["/ext/apps_data/totp"]] {
        let roots: &'static [&'static str] = Box::leak(bad.to_vec().into_boxed_slice());
        let (r, requests) = run(FakeFlipper::standard().with_captures(), dir.join("x.db"), roots);
        assert!(matches!(r, Err(PullError::NotAllowed(_))), "{bad:?}");
        assert!(requests.is_empty(), "nothing asked of the device for {bad:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn failures_are_reported_and_the_pull_goes_on() {
    let dir = tempdir();
    let fake = FakeFlipper { fault: Fault::ShortRead, ..FakeFlipper::standard().with_captures() };
    let (r, _) = run(fake, dir.join("c.db"), &["/ext/nfc"]);
    let r = r.unwrap();
    assert_eq!(r.new, 0);
    assert_eq!(r.failed.len(), 3, "every short file fails, each reported");
    assert!(Store::open(&dir.join("c.db")).unwrap().captures().unwrap().is_empty());

    let fake = FakeFlipper::standard().with_captures().with_hostile_names();
    let mut hostile = fake.clone();
    hostile.hostile = vec![("/ext/nfc".into(), "../../escape.nfc".into(), false)];
    let (r, requests) = run(hostile, dir.join("h.db"), &["/ext/nfc"]);
    let r = r.unwrap();
    assert_eq!(r.failed.len(), 1);
    assert_eq!(r.new, 3);
    assert!(requests.iter().all(|q| !q.contains("escape") || q.contains("List")));
    let _ = std::fs::remove_dir_all(&dir);
}
