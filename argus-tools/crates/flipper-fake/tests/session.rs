//! The Rust session and operations against the fake Flipper over a socket pair: the happy
//! paths, the exact bytes put on the wire, and every fault the fake can produce.

use std::os::unix::net::UnixStream;
use std::thread;
use std::time::Duration;

use flipper_fake::{FakeFlipper, Fault, Transcript};
use flipper_rpc::frame::encode_frame;
use flipper_rpc::ops::{FileKind, InfoSource};
use flipper_rpc::proto::pb::main::Content;
use flipper_rpc::proto::pb::{CommandStatus, Main};
use flipper_rpc::proto::pb_storage;
use flipper_rpc::{RpcError, Session};

const TIMEOUT: Duration = Duration::from_millis(500);

/// Runs `client` against `fake` and returns its result with what the fake received.
fn run<R: Send + 'static>(fake: FakeFlipper, client: impl FnOnce(UnixStream) -> R + Send + 'static) -> (R, Transcript) {
    let (a, mut b) = UnixStream::pair().unwrap();
    let server = thread::spawn(move || fake.serve(&mut b));
    let r = client(a); // dropping the client's end lets the fake return
    let t = server.join().unwrap().unwrap_or_default();
    (r, t)
}

fn with_fault(f: Fault) -> FakeFlipper {
    FakeFlipper { fault: f, ..FakeFlipper::standard() }
}

#[test]
fn handshake_sends_exactly_start_rpc_session() {
    let (r, t) = run(FakeFlipper::standard(), |s| Session::start(s, TIMEOUT).map(|_| ()));
    r.unwrap();
    assert_eq!(t.received, b"start_rpc_session\r");
}

#[test]
fn protobuf_version_and_devinfo() {
    let ((v, (src, info)), t) = run(FakeFlipper::standard(), |s| {
        let mut s = Session::start(s, TIMEOUT).unwrap();
        let v = s.protobuf_version().unwrap();
        (v, s.device_info(v).unwrap())
    });
    assert_eq!(v, (0, 25));
    assert_eq!(src, InfoSource::Property);
    assert_eq!(info, FakeFlipper::standard().devinfo);
    // command ids count from 1, as in the C++ app
    let ids: Vec<u32> = t.requests.iter().map(|m| m.command_id).collect();
    assert_eq!(ids, vec![1, 2]);
}

#[test]
fn legacy_firmware_gets_the_old_request() {
    let fake = FakeFlipper { protobuf_version: (0, 13), ..FakeFlipper::standard() };
    let ((src, info), t) = run(fake, |s| {
        let mut s = Session::start(s, TIMEOUT).unwrap();
        let v = s.protobuf_version().unwrap();
        s.device_info(v).unwrap()
    });
    assert_eq!(src, InfoSource::Legacy);
    assert_eq!(info[0], ("hardware_name".to_string(), "Fakeflip".to_string()));
    assert!(matches!(t.requests[1].content, Some(Content::SystemDeviceInfoRequest(_))));
}

#[test]
fn list_follows_has_next_across_frames() {
    let (files, _) = run(FakeFlipper::standard(), |s| Session::start(s, TIMEOUT).unwrap().storage_list("/ext/subghz").unwrap());
    assert_eq!(files.len(), 19);
    assert_eq!(files[0].name, "signal_00.sub");
    assert_eq!(files[18].size, 118);
}

#[test]
fn list_root_empty_and_non_ascii() {
    let ((root, empty, nfc), _) = run(FakeFlipper::standard(), |s| {
        let mut s = Session::start(s, TIMEOUT).unwrap();
        (s.storage_list("/").unwrap(), s.storage_list("/ext/empty").unwrap(), s.storage_list("/ext/nfc").unwrap())
    });
    assert_eq!(root.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["any", "int", "ext"]);
    assert!(root.iter().all(|e| e.kind == FileKind::Dir));
    assert!(empty.is_empty());
    // the firmware leaves non-ASCII names out of listings
    assert_eq!(nfc.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["badge.nfc"]);
}

#[test]
fn list_missing_path_is_a_device_error() {
    let (r, _) = run(FakeFlipper::standard(), |s| Session::start(s, TIMEOUT).unwrap().storage_list("/ext/nope"));
    assert!(matches!(r, Err(RpcError::Device(CommandStatus::ErrorStorageNotExist))));
}

#[test]
fn stat_and_info() {
    let ((m, missing, ext, int), _) = run(FakeFlipper::standard(), |s| {
        let mut s = Session::start(s, TIMEOUT).unwrap();
        (
            s.storage_stat("/ext/Manifest").unwrap(),
            s.storage_stat("/ext/missing").unwrap(),
            s.storage_info("/ext").unwrap(),
            s.storage_info("/int").unwrap(),
        )
    });
    assert_eq!(m.unwrap().size, 48_213);
    assert!(missing.is_none());
    assert_eq!(ext.unwrap().total, 31_902_400_512);
    assert_eq!(int.unwrap().free, 200_704);
    let fake = FakeFlipper { sd_card: false, ..FakeFlipper::standard() };
    let (none, _) = run(fake, |s| Session::start(s, TIMEOUT).unwrap().storage_info("/ext").unwrap());
    assert!(none.is_none());
}

#[test]
fn bad_paths_never_reach_the_wire() {
    let (r, t) = run(FakeFlipper::standard(), |s| Session::start(s, TIMEOUT).unwrap().storage_list("/ext/../int"));
    assert!(matches!(r, Err(RpcError::Invalid(_))));
    assert!(t.requests.is_empty());
}

#[test]
fn list_request_bytes() {
    // command_id 1, storage_list_request (field 7) { path "/ext" }
    let (_, t) = run(FakeFlipper::standard(), |s| Session::start(s, TIMEOUT).unwrap().storage_list("/ext").unwrap());
    let rpc = &t.received[b"start_rpc_session\r".len()..];
    assert_eq!(rpc, [0x0a, 0x08, 0x01, 0x3a, 0x06, 0x0a, 0x04, b'/', b'e', b'x', b't']);
}

#[test]
fn faults_end_in_errors_not_hangs() {
    let cases: Vec<(Fault, &str)> = vec![
        (Fault::NoPrompt, "timeout"),
        (Fault::Silent, "timeout"),
        (Fault::GarbageFrame, "frame"),
        (Fault::OversizeFrame, "frame"),
        (Fault::Status(CommandStatus::ErrorBusy as i32), "device"),
        (Fault::Status(99), "device"),
        (Fault::FloodOtherIds(2000), "limit"),
        (Fault::WrongType, "unexpected"),
        (Fault::CloseMidFrame, "closed"),
    ];
    for (fault, code) in cases {
        let label = format!("{fault:?}");
        let (r, _) = run(with_fault(fault), |s| Session::start(s, TIMEOUT).and_then(|mut s| s.storage_list("/ext")));
        match r {
            Err(e) => assert_eq!(e.code(), code, "{label}: {e}"),
            Ok(v) => panic!("{label}: expected an error, got {v:?}"),
        }
    }
}

#[test]
fn flood_below_the_limit_is_skipped() {
    let (r, _) = run(with_fault(Fault::FloodOtherIds(100)), |s| Session::start(s, TIMEOUT).unwrap().storage_list("/ext"));
    assert_eq!(r.unwrap().len(), 5);
}

#[test]
fn rpc_bytes_glued_to_the_echo_are_kept() {
    // A device that sends its first frame in the same write as the echo must not lose it.
    let (a, mut b) = UnixStream::pair().unwrap();
    let server = thread::spawn(move || {
        use std::io::{Read, Write};
        b.write_all(b"banner\r\n\r\n>: ").unwrap();
        let mut cmd = [0u8; 18];
        b.read_exact(&mut cmd).unwrap();
        let resp = Main {
            command_id: 1,
            content: Some(Content::StorageListResponse(pb_storage::ListResponse { file: vec![] })),
            ..Default::default()
        };
        let mut out = b"start_rpc_session\r\n".to_vec();
        out.extend(encode_frame(&resp).unwrap());
        b.write_all(&out).unwrap();
        let mut sink = Vec::new();
        let _ = b.read_to_end(&mut sink);
    });
    let mut s = Session::start(a, TIMEOUT).unwrap();
    assert!(s.storage_list("/ext").unwrap().is_empty());
    drop(s);
    server.join().unwrap();
}
