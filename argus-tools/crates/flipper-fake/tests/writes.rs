//! Phase-4 writes against the fake Flipper: the firmware's rules for write, mkdir, remove and
//! rename, writes with status pings, reboot and factory reset, and restores (the archive
//! checks with every hostile archive, a conflict refused before writing, a restore that brings
//! /int back).

use std::os::unix::net::UnixStream;
use std::thread;
use std::time::Duration;

use flipper_fake::{archives, FakeFlipper, Transcript};
use flipper_rpc::proto::pb::main::Content;
use flipper_rpc::proto::pb::CommandStatus;
use flipper_rpc::restore::{read_backup, ArchiveLimits};
use flipper_rpc::write::RebootMode;
use flipper_rpc::{RpcError, Session};

const TIMEOUT: Duration = Duration::from_millis(2000);

/// Runs `client` against `fake`; returns its result, what was sent, and the storage afterwards.
fn run<R: Send + 'static>(fake: FakeFlipper, client: impl FnOnce(Session<UnixStream>) -> R + Send + 'static) -> (R, Transcript, String) {
    let (a, mut b) = UnixStream::pair().unwrap();
    let server = thread::spawn(move || {
        let t = fake.serve(&mut b).unwrap_or_default();
        (t, fake.dump())
    });
    let r = client(Session::start(a, TIMEOUT).unwrap());
    let (t, dump) = server.join().unwrap();
    (r, t, dump)
}

fn device_err(r: Result<(), RpcError>) -> CommandStatus {
    match r {
        Err(RpcError::Device(s)) => s,
        other => panic!("expected a device error, got {other:?}"),
    }
}

fn line<'a>(dump: &'a str, path: &str) -> Option<&'a str> {
    dump.lines().find(|l| l.split(' ').nth(1) == Some(path))
}

#[test]
fn firmware_rules_for_mkdir_remove_rename() {
    let ((), _, dump) = run(FakeFlipper::standard(), |mut s| {
        use CommandStatus::*;
        assert_eq!(device_err(s.storage_mkdir("/int")), ErrorStorageExist, "/int always exists (alias of /ext/.int)");
        assert_eq!(device_err(s.storage_mkdir("/int/sub")), ErrorStorageExist);
        assert_eq!(device_err(s.storage_mkdir("/int/no/such")), ErrorStorageNotExist);
        assert_eq!(device_err(s.storage_mkdir("/elsewhere")), ErrorStorageInvalidName);
        s.storage_mkdir("/int/new").unwrap();
        assert_eq!(device_err(s.storage_remove("/int", false)), ErrorStorageDirNotEmpty);
        assert_eq!(device_err(s.storage_remove("/int/sub", false)), ErrorStorageDirNotEmpty);
        s.storage_remove("/int/missing.txt", false).unwrap();
        s.storage_remove("/int/empty.txt", false).unwrap();
        s.storage_remove("/int/new", false).unwrap();
        s.storage_rename("/int/favorites.txt", "/int/renamed.txt").unwrap();
        assert_eq!(device_err(s.storage_rename("/int/favorites.txt", "/int/x")), ErrorStorageNotExist);
        assert_eq!(device_err(s.storage_rename("/int/renamed.txt", "/int/.bt.settings")), ErrorStorageExist);
        s.storage_rename("/int/sub", "/int/moved").unwrap();
        s.storage_remove("/int/moved", true).unwrap();
        // refused before anything is sent
        assert!(matches!(s.storage_mkdir("/int/caf\u{e9}"), Err(RpcError::Invalid(_))));
        assert!(matches!(s.storage_rename("/int/a", "/int/../b"), Err(RpcError::Invalid(_))));
        // the session is still usable after all those errors
        assert_eq!(s.protobuf_version().unwrap(), (0, 25));
    });
    assert!(line(&dump, "/int/empty.txt").is_none());
    assert!(line(&dump, "/int/renamed.txt").is_some_and(|l| l.contains(" 37 ")));
    assert!(line(&dump, "/int/favorites.txt").is_none());
    assert!(!dump.contains("/int/sub") && !dump.contains("/int/moved"));
    assert!(line(&dump, "/int").is_some());
}

#[test]
fn writes_replace_files_and_ping_like_the_cpp_app() {
    let big: Vec<u8> = (0..300 * 1024).map(|i| (i % 253) as u8).collect();
    let big2 = big.clone();
    let ((), t, dump) = run(FakeFlipper::standard(), move |mut s| {
        s.storage_write("/int/favorites.txt", b"new").unwrap();
        s.storage_write("/int/zero.txt", b"").unwrap();
        s.storage_write("/ext/big.bin", &big2).unwrap();
        assert_eq!(device_err(s.storage_write("/int/no/such.txt", b"x")), CommandStatus::ErrorStorageNotExist);
        assert_eq!(device_err(s.storage_write("/int/sub", b"x")), CommandStatus::ErrorStorageDenied);
        let mut back = Vec::new();
        s.storage_read("/ext/big.bin", &mut back, 1 << 20).unwrap();
        assert_eq!(back, big2);
    });
    assert!(line(&dump, "/int/favorites.txt").unwrap().contains(" 3 "));
    assert!(line(&dump, "/int/zero.txt").unwrap().contains(" 0 "));
    assert!(line(&dump, "/ext/big.bin").unwrap().contains(&format!(" {} ", big.len())));
    let pings = t.requests.iter().filter(|m| matches!(m.content, Some(Content::SystemPingRequest(_)))).count();
    assert_eq!(pings, 99, "a ping every 3 chunks of 300, none after the last");
    let empty =
        t.requests.iter().find(|m| matches!(&m.content, Some(Content::StorageWriteRequest(w)) if w.path == "/int/zero.txt")).unwrap();
    assert!(matches!(&empty.content, Some(Content::StorageWriteRequest(w)) if w.file.is_none()), "an empty file is sent without a file");
}

#[test]
fn another_request_interrupts_a_write_as_the_firmware_does() {
    // Frames of an unfinished write, then a list: the firmware answers the write's id with
    // "continuous command interrupted" and keeps what was written.
    let ((), t, dump) = run(FakeFlipper::standard(), |mut s| {
        let id = s.new_id().unwrap();
        let frames = flipper_rpc::write::write_frames("/int/part.bin", &[9u8; 2048]);
        let first = frames.into_iter().next().flatten().unwrap();
        s.send_frames(id, vec![first]).unwrap();
        // the list after it; the interrupted write's answer comes first
        s.storage_list("/int").unwrap();
    });
    assert!(line(&dump, "/int/part.bin").unwrap().contains(" 1024 "));
    assert_eq!(t.requests.len(), 2);
}

#[test]
fn reboot_and_factory_reset_end_the_session() {
    let (gone, t, dump) = run(FakeFlipper::standard(), |mut s| {
        s.factory_reset().unwrap();
        s.wait_until_gone(Duration::from_secs(5))
    });
    assert!(gone);
    assert!(matches!(t.requests.last().unwrap().content, Some(Content::SystemFactoryResetRequest(_))));
    assert!(!dump.lines().any(|l| l.split(' ').nth(1).is_some_and(|p| p.starts_with("/int/"))), "/int is formatted");
    let (gone, t, dump2) = run(FakeFlipper::standard(), |mut s| {
        s.system_reboot(RebootMode::Os).unwrap();
        s.wait_until_gone(Duration::from_secs(5))
    });
    assert!(gone && t.requests.len() == 1);
    assert_eq!(dump2, FakeFlipper::standard().dump(), "a reboot changes nothing");
}

#[test]
fn every_hostile_archive_is_refused_and_the_good_ones_read() {
    for (name, why, bytes) in archives::hostile() {
        let r = read_backup(&bytes[..], &ArchiveLimits::default());
        assert!(r.is_err(), "{name} ({why}) was accepted");
    }
    let bomb = archives::bomb();
    assert!(bomb.len() < 1 << 20, "the bomb is small: {}", bomb.len());
    assert!(read_backup(&bomb[..], &ArchiveLimits::default()).unwrap_err().0.contains("decompressed"));
    for (name, bytes) in archives::good() {
        let a = read_backup(&bytes[..], &ArchiveLimits::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        let paths: Vec<&str> = a.entries.iter().map(|e| e.path.as_str()).collect();
        // breadth first, each folder's names in byte order
        let mut want = vec!["/int/.bt.settings", "/int/empty.txt", "/int/favorites.txt"];
        if name == "large-file" {
            want.push("/int/large.bin");
        }
        want.extend(["/int/sub", "/int/sub/deep.txt", "/int/sub/inner"]);
        assert_eq!(paths, want, "{name}");
    }
}

#[test]
fn restore_brings_int_back_and_refuses_a_conflict_before_writing() {
    let archive = read_backup(&archives::gzip(&archives::tar(&archives::standard_int(), true))[..], &ArchiveLimits::default()).unwrap();
    let a2 = archive.clone();
    let (r, _, dump) = run(FakeFlipper::standard().with_changed_int(), move |mut s| s.restore(&a2));
    let r = r.unwrap();
    assert_eq!((r.dirs_made, r.dirs_kept, r.files), (0, 2, 4));
    let expected: Vec<String> = FakeFlipper::standard().dump().lines().map(str::to_string).collect();
    let got: Vec<String> = dump.lines().filter(|l| !l.starts_with("f /int/extra.txt ")).map(str::to_string).collect();
    assert_eq!(got, expected);
    assert!(line(&dump, "/int/extra.txt").is_some(), "files not in the backup stay");

    // /int/sub is a file on the device: nothing is written at all
    let start = FakeFlipper::standard().with_int_conflict().dump();
    let (r, t, dump) = run(FakeFlipper::standard().with_int_conflict(), move |mut s| s.restore(&archive));
    assert!(matches!(r, Err(RpcError::Invalid(ref m)) if m.contains("/int/sub")), "{r:?}");
    assert_eq!(dump, start);
    assert!(t.requests.iter().all(|m| matches!(m.content, Some(Content::StorageListRequest(_)))));

    // a restore into an empty /int makes the folders
    let archive = read_backup(&archives::gzip(&archives::tar(&archives::standard_int(), true))[..], &ArchiveLimits::default()).unwrap();
    let mut empty = FakeFlipper::standard();
    empty.nodes.retain(|n| !n.path.starts_with("/int/"));
    let (r, _, dump) = run(empty, move |mut s| s.restore(&archive));
    assert_eq!(r.unwrap().dirs_made, 2);
    assert_eq!(dump, FakeFlipper::standard().dump());
}
