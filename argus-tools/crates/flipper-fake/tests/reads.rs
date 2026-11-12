//! Phase-2 reads against the fake Flipper: MD5, file reads (sizes 0, under, at and over the
//! 512-byte chunk, short and long answers), date/time, the file tree, pull and backup, with
//! hostile names from the device.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use flipper_fake::{FakeFlipper, Fault, Node, Transcript, FAKE_DATETIME};
use flipper_rpc::ops::FileKind;
use flipper_rpc::proto::pb::main::Content;
use flipper_rpc::proto::pb::CommandStatus;
use flipper_rpc::transfer::Limits;
use flipper_rpc::{RpcError, Session};

const TIMEOUT: Duration = Duration::from_millis(500);

fn run<R: Send + 'static>(fake: FakeFlipper, client: impl FnOnce(Session<UnixStream>) -> R + Send + 'static) -> (R, Transcript) {
    let (a, mut b) = UnixStream::pair().unwrap();
    let server = thread::spawn(move || fake.serve(&mut b));
    let r = client(Session::start(a, TIMEOUT).unwrap());
    (r, server.join().unwrap().unwrap_or_default())
}

fn node(path: &str) -> Node {
    FakeFlipper::standard().nodes.into_iter().find(|n| n.path == path).unwrap()
}

fn tempdir() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("r{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn md5_hex(data: &[u8]) -> String {
    use md5::Digest;
    md5::Md5::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn md5_and_datetime() {
    let ((md5, dt), _) = run(FakeFlipper::standard(), |mut s| (s.storage_md5("/ext/Manifest").unwrap(), s.system_datetime().unwrap()));
    assert_eq!(md5, md5_hex(&node("/ext/Manifest").content()));
    let (y, mo, d, h, mi, se, wd) = FAKE_DATETIME;
    assert_eq!((dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second, dt.weekday), (y, mo, d, h, mi, se, wd));
    assert_eq!(dt.iso(), "2026-11-09T12:34:56");
}

#[test]
fn reads_of_every_size_come_back_whole() {
    for path in ["/int/empty.txt", "/int/favorites.txt", "/ext/nfc/badge.nfc", "/int/sub/deep.txt", "/ext/Manifest"] {
        let (got, t) = run(FakeFlipper::standard(), move |mut s| {
            let mut buf = Vec::new();
            let n = s.storage_read(path, &mut buf, u64::MAX).unwrap();
            (n, buf)
        });
        assert_eq!(got.1, node(path).content(), "{path}");
        assert_eq!(got.0, node(path).content().len() as u64);
        // as the C++ app: a stat and a read request under one command id
        assert_eq!(t.requests.len(), 2);
        assert!(matches!(t.requests[0].content, Some(Content::StorageStatRequest(_))));
        assert!(matches!(t.requests[1].content, Some(Content::StorageReadRequest(_))));
        assert_eq!(t.requests[0].command_id, t.requests[1].command_id);
    }
}

#[test]
fn read_wire_bytes_equal_the_cpp_pair() {
    // The C++ StorageReadOperation's two frames for /int/favorites.txt, id 13
    // (tests/fixtures/nanopb_frames.txt: req_read_pair_stat, req_read_pair_read).
    let text = include_str!("../../flipper-rpc/tests/fixtures/nanopb_frames.txt");
    let frame = |name: &str| -> Vec<u8> {
        let hex = text.lines().find_map(|l| l.strip_prefix(name).and_then(|r| r.strip_prefix(' '))).unwrap();
        (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
    };
    let (_, t) = run(FakeFlipper::standard(), |mut s| {
        for _ in 0..12 {
            s.protobuf_version().unwrap(); // ids 1..=12
        }
        s.storage_read("/int/favorites.txt", &mut Vec::new(), 1 << 20).unwrap();
    });
    let mut want = frame("req_read_pair_stat");
    want.extend(frame("req_read_pair_read"));
    assert!(t.received.ends_with(&want));
}

#[test]
fn read_limits_and_mismatches() {
    let (r, _) = run(FakeFlipper::standard(), |mut s| {
        let mut buf = Vec::new();
        let r = s.storage_read("/ext/Manifest", &mut buf, 1000);
        (r, buf.len(), s.protobuf_version())
    });
    assert!(matches!(r.0, Err(RpcError::Limit(_))));
    assert_eq!(r.1, 0, "nothing written before the size check");
    assert!(matches!(r.2, Err(RpcError::Broken)), "a stream left mid-file is not reused");

    for (fault, what) in [(Fault::ShortRead, "short"), (Fault::LongRead, "long")] {
        let (r, _) =
            run(FakeFlipper { fault, ..FakeFlipper::standard() }, |mut s| s.storage_read("/int/favorites.txt", &mut Vec::new(), 1 << 20));
        assert!(matches!(r, Err(RpcError::Unexpected(_))), "{what}: {r:?}");
    }
}

#[test]
fn a_missing_file_is_a_clean_device_error() {
    let (r, _) = run(FakeFlipper::standard(), |mut s| {
        let missing = s.storage_read("/int/nope", &mut Vec::new(), 1 << 20);
        // The read's own error frame is still on its way and must not confuse the next request.
        (missing, s.storage_md5("/int/favorites.txt"))
    });
    assert!(matches!(r.0, Err(RpcError::Device(CommandStatus::ErrorStorageNotExist))));
    assert_eq!(r.1.unwrap(), md5_hex(&node("/int/favorites.txt").content()));
}

#[test]
fn walk_is_breadth_first_like_the_cpp_file_tree() {
    let (tree, t) = run(FakeFlipper::standard(), |mut s| s.walk("/int", &Limits::default()).unwrap());
    let rel: Vec<&str> = tree.entries.iter().map(|e| e.relative.as_str()).collect();
    assert_eq!(rel, [".bt.settings", "favorites.txt", "empty.txt", "sub", "sub/deep.txt", "sub/inner"]);
    let listed: Vec<String> = t
        .requests
        .iter()
        .map(|m| match &m.content {
            Some(Content::StorageListRequest(r)) => r.path.clone(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(listed, ["/int", "/int/sub", "/int/sub/inner"]);
}

#[test]
fn walk_limits() {
    let (r, _) = run(FakeFlipper::standard(), |mut s| s.walk("/int", &Limits { max_depth: 1, ..Limits::default() }));
    assert!(matches!(r, Err(RpcError::Limit(_))));
    let (r, _) = run(FakeFlipper::standard(), |mut s| s.walk("/ext", &Limits { max_entries: 10, ..Limits::default() }));
    assert!(matches!(r, Err(RpcError::Limit(_))));
    let (r, _) = run(FakeFlipper::standard(), |mut s| s.walk("/int", &Limits { max_total: 100, ..Limits::default() }));
    assert!(matches!(r, Err(RpcError::Limit(_))));
}

fn hostile_fake() -> FakeFlipper {
    let mut f = FakeFlipper::standard();
    for name in ["..", ".", "../../escape", "a/b", "back\\slash", "nul\0x", "line\nfeed", "esc\u{1b}[2J", ""] {
        f.hostile.push(("/int".into(), name.into(), false));
    }
    f.hostile.push(("/int".into(), "..".into(), true));
    f
}

#[test]
fn hostile_names_are_refused_and_never_touch_the_disk() {
    let dir = tempdir();
    let dest = dir.join("int");
    let d2 = dest.clone();
    let (report, t) = run(hostile_fake(), move |mut s| s.pull("/int", &d2, &Limits::default()).unwrap());
    assert_eq!(report.refused.len(), 10);
    assert_eq!(report.files, 4);
    // nothing outside dest, and no hostile name was ever read
    let mut outside: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    outside.sort();
    assert_eq!(outside, ["int"]);
    assert!(t.requests.iter().all(|m| !matches!(&m.content, Some(Content::StorageReadRequest(r)) if r.path.contains(".."))));
    let _ = std::fs::remove_dir_all(&dir);
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn pull_a_folder_and_a_file() {
    let dir = tempdir();
    let dest = dir.join("int");
    let d2 = dest.clone();
    let (report, _) = run(FakeFlipper::standard(), move |mut s| s.pull("/int", &d2, &Limits::default()).unwrap());
    assert_eq!((report.files, report.dirs), (4, 3));
    for rel in [".bt.settings", "favorites.txt", "empty.txt", "sub/deep.txt"] {
        let local = dest.join(rel);
        assert_eq!(std::fs::read(&local).unwrap(), node(&format!("/int/{rel}")).content(), "{rel}");
        assert_eq!(mode(&local), 0o600);
    }
    assert_eq!(mode(&dest), 0o700);
    assert_eq!(mode(&dest.join("sub/inner")), 0o700);

    let file = dir.join("manifest");
    let f2 = file.clone();
    let (report, _) = run(FakeFlipper::standard(), move |mut s| s.pull("/ext/Manifest", &f2, &Limits::default()).unwrap());
    assert_eq!(report.bytes, 48_213);
    assert_eq!(std::fs::read(&file).unwrap(), node("/ext/Manifest").content());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pull_never_overwrites_and_cleans_up_a_failed_file() {
    let dir = tempdir();
    let existing = dir.join("there");
    std::fs::write(&existing, b"mine").unwrap();
    let e2 = existing.clone();
    let (r, t) = run(FakeFlipper::standard(), move |mut s| s.pull("/int", &e2, &Limits::default()));
    assert!(matches!(r, Err(RpcError::Local(_))));
    assert!(t.requests.is_empty(), "refused before asking the device");
    assert_eq!(std::fs::read(&existing).unwrap(), b"mine");

    let target = dir.join("short");
    let t2 = target.clone();
    let (r, _) = run(FakeFlipper { fault: Fault::ShortRead, ..FakeFlipper::standard() }, move |mut s| {
        s.pull("/int/favorites.txt", &t2, &Limits::default())
    });
    assert!(r.is_err());
    assert!(!target.exists(), "a partial file is removed");

    let (r, _) = run(FakeFlipper::standard(), {
        let d = dir.join("nothing");
        move |mut s| s.pull("/int/nope", &d, &Limits::default())
    });
    assert!(matches!(r, Err(RpcError::Device(CommandStatus::ErrorStorageNotExist))));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The C++ app's restore index (backend/tararchive.cpp, TarArchive::readTarFile) in Rust:
/// what it would find in an archive. Returns (name, is_dir, data) per entry.
fn cpp_tar_index(tar: &[u8]) -> Vec<(String, bool, Vec<u8>)> {
    let mut out = Vec::new();
    let mut pos = 0;
    let mut empty = 0;
    while pos < tar.len() {
        let h = &tar[pos..pos + 512];
        pos += 512;
        if h.iter().all(|&b| b == 0) {
            empty += 1;
            if empty == 2 {
                break;
            }
            continue;
        }
        let name_end = h[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = String::from_utf8(h[..name_end].to_vec()).unwrap();
        let size_text: String = h[124..136].iter().take_while(|&&b| b != 0 && b != b' ').map(|&b| b as char).collect();
        let size = usize::from_str_radix(&size_text, 8).unwrap();
        // the C++ reader also checks nothing else; verify the checksum the way tar(1) would
        let stored = u32::from_str_radix(std::str::from_utf8(&h[148..154]).unwrap(), 8).unwrap();
        let sum: u32 = 256 + h[..148].iter().chain(&h[156..]).map(|&b| u32::from(b)).sum::<u32>();
        assert_eq!(stored, sum, "checksum of {name}");
        match h[156] {
            b'0' => out.push((name, false, tar[pos..pos + size].to_vec())),
            b'5' => out.push((name.trim_end_matches('/').to_string(), true, Vec::new())),
            t => panic!("type {t} would stop the C++ restore"),
        }
        pos += size + if size.is_multiple_of(512) { 0 } else { 512 - size % 512 };
    }
    out
}

#[test]
fn backup_is_what_the_cpp_restore_reads() {
    let dir = tempdir();
    let out = dir.join("backup.tgz");
    let o2 = out.clone();
    let (report, _) = run(FakeFlipper::standard(), move |mut s| s.backup(&o2, &Limits::backup(), 1_762_700_000).unwrap());
    assert_eq!((report.files, report.dirs), (4, 2));
    assert_eq!(mode(&out), 0o600);
    assert!(!dir.join("backup.tgz.part").exists());

    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(&out).unwrap()).read_to_end(&mut tar).unwrap();
    let index = cpp_tar_index(&tar);
    let names: Vec<&str> = index.iter().map(|(n, _, _)| n.as_str()).collect();
    assert_eq!(names, ["int", "int/.bt.settings", "int/favorites.txt", "int/empty.txt", "int/sub", "int/sub/deep.txt", "int/sub/inner"]);
    for (name, is_dir, data) in &index {
        if !is_dir {
            assert_eq!(data, &node(&format!("/{name}")).content(), "{name}");
        }
    }
    // and a standard tar reader agrees
    let mut a = tar::Archive::new(&tar[..]);
    let std_names: Vec<String> = a.entries().unwrap().map(|e| e.unwrap().path().unwrap().display().to_string()).collect();
    assert_eq!(std_names.len(), 7);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn backup_refusals() {
    let dir = tempdir();
    let out = dir.join("b.tgz");
    std::fs::write(&out, b"old").unwrap();
    let o2 = out.clone();
    let (r, _) = run(FakeFlipper::standard(), move |mut s| s.backup(&o2, &Limits::backup(), 0));
    assert!(matches!(r, Err(RpcError::Local(_))));
    assert_eq!(std::fs::read(&out).unwrap(), b"old");

    // a failure mid-way leaves neither the archive nor its .part
    let out = dir.join("c.tgz");
    let o2 = out.clone();
    let (r, _) = run(FakeFlipper { fault: Fault::LongRead, ..FakeFlipper::standard() }, move |mut s| s.backup(&o2, &Limits::backup(), 0));
    assert!(r.is_err());
    assert!(!out.exists() && !dir.join("c.tgz.part").exists());

    // a name the C++ writer would have truncated is refused
    let mut fake = FakeFlipper::standard();
    fake.nodes.push(Node { path: format!("/int/{}", "n".repeat(96)), dir: false, size: 1 });
    let out = dir.join("d.tgz");
    let o2 = out.clone();
    let (r, _) = run(fake, move |mut s| s.backup(&o2, &Limits::backup(), 0));
    assert!(matches!(r, Err(RpcError::Local(ref m)) if m.contains("cannot store")), "{r:?}");
    assert!(!out.exists());

    // hostile names are reported, not archived
    let out = dir.join("e.tgz");
    let o2 = out.clone();
    let (report, _) = run(hostile_fake(), move |mut s| s.backup(&o2, &Limits::backup(), 0).unwrap());
    assert_eq!(report.refused.len(), 10);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn listing_kinds_are_kept() {
    let (tree, _) = run(FakeFlipper::standard(), |mut s| s.walk("/int", &Limits::default()).unwrap());
    assert_eq!(tree.entries.iter().filter(|e| e.kind == FileKind::Dir).count(), 2);
}

#[test]
fn the_empty_file_answer_is_the_firmwares_bytes() {
    let text = include_str!("../../flipper-rpc/tests/fixtures/nanopb_frames.txt");
    let hex = text.lines().find_map(|l| l.strip_prefix("resp_read_empty_file ")).unwrap();
    let want: Vec<u8> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
    assert_eq!(flipper_fake::empty_file_read_response(9), want);
}

#[test]
fn an_empty_file_without_its_data_field_reads_as_empty() {
    let (r, _) = run(FakeFlipper { fault: Fault::OmitEmptyData, ..FakeFlipper::standard() }, |mut s| {
        let mut buf = Vec::new();
        (s.storage_read("/int/empty.txt", &mut buf, 1 << 20), buf)
    });
    assert_eq!(r.0.unwrap(), 0);
    assert!(r.1.is_empty());
}

#[test]
fn the_hostile_preset_is_refused_by_pull() {
    let dir = tempdir();
    let dest = dir.join("int");
    let d2 = dest.clone();
    let (report, t) = run(FakeFlipper::standard().with_hostile_names(), move |mut s| s.pull("/int", &d2, &Limits::default()).unwrap());
    assert_eq!(report.refused.len(), flipper_fake::HOSTILE_NAMES.len() + 1);
    assert_eq!(report.files, 4);
    assert!(t
        .requests
        .iter()
        .all(|m| !matches!(&m.content, Some(Content::StorageReadRequest(r)) if !r.path.starts_with("/int/") || r.path.contains(".."))));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn screen_frames_arrive_whole_and_the_stream_is_stopped() {
    let fake = FakeFlipper { screen_frames: 3, ..FakeFlipper::standard() };
    let (r, t) = run(fake, |mut s| s.screen_frames(3).unwrap());
    assert_eq!(r.dropped, 0);
    assert_eq!(r.frames.iter().map(|f| f.data.clone()).collect::<Vec<_>>(), (0..3).map(flipper_fake::screen_frame).collect::<Vec<_>>());
    let kinds: Vec<_> = t.requests.iter().map(flipper_fake::describe).collect();
    assert!(kinds[0].contains("GuiStartScreenStreamRequest") && kinds[1].contains("GuiStopScreenStreamRequest"), "{kinds:?}");
    assert_eq!(kinds.len(), 2);
}

#[test]
fn screen_frames_of_the_wrong_length_are_dropped() {
    for fault in [Fault::ShortFrame, Fault::LongFrame] {
        let fake = FakeFlipper { screen_frames: 2, fault: fault.clone(), ..FakeFlipper::standard() };
        let (r, _) = run(fake, |mut s| s.screen_frames(2).unwrap());
        assert_eq!((r.frames.len(), r.dropped), (2, 1), "{fault:?}");
        assert_eq!(r.frames[0].data, flipper_fake::screen_frame(0));
    }
    // only frames without data: nothing usable arrives, the wait ends at the timeout
    let fake = FakeFlipper { fault: Fault::FrameWithoutData, ..FakeFlipper::standard() };
    let (r, _) = run(fake, |mut s| s.screen_frames(1));
    assert!(matches!(r, Err(RpcError::Timeout(_))), "{r:?}");
    let (r, _) = run(FakeFlipper::standard(), |mut s| s.screen_frames(0));
    assert!(matches!(r, Err(RpcError::Invalid(_))));
}
