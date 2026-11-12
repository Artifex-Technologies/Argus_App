//! The resources install (`flipper_rpc::assets`): the manifest as the C++ `AssetManifest` reads it (with the
//! name rules both versions now have), the plan as `AssetsDownloadOperation::buildFileLists` makes it, and the
//! bundle checks. The requests themselves are compared with the C++ app in compare-repair.sh.

use std::io::Write;

use flipper_rpc::assets::{plan, read_bundle, BundleLimits, DeviceManifest, Item, Manifest};

fn tar_entry(out: &mut Vec<u8>, name: &str, data: Option<&[u8]>) {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    h[100..107].copy_from_slice(b"0000644");
    h[108..115].copy_from_slice(b"0000000");
    h[116..123].copy_from_slice(b"0000000");
    let size = data.map_or(0, <[u8]>::len);
    h[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
    h[136..147].copy_from_slice(b"00000000000");
    h[156] = if data.is_some() { b'0' } else { b'5' };
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    out.extend_from_slice(&h);
    if let Some(d) = data {
        out.extend_from_slice(d);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
}

fn tgz(entries: &[(&str, Option<&[u8]>)]) -> Vec<u8> {
    let mut t = Vec::new();
    for (name, data) in entries {
        tar_entry(&mut t, name, *data);
    }
    t.extend([0u8; 1024]);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&t).unwrap();
    gz.finish().unwrap()
}

fn md5(d: &[u8]) -> String {
    use md5::{Digest, Md5};
    Md5::digest(d).iter().map(|b| format!("{b:02x}")).collect()
}

fn item(path: &str, dir: bool) -> Item {
    Item { path: path.into(), dir }
}

const NEW: &str = "V:0\nT:1700000000\nD:dolphin\nD:nfc\nD:nfc/assets\nF:aa:3:Readme.txt\nF:bb:5:dolphin/a.txt\nF:cc:7:nfc/assets/x.nfc\n";

#[test]
fn manifest_reads_as_the_cpp_app_does() {
    let m = Manifest::parse(NEW.as_bytes()).unwrap();
    assert_eq!((m.version, m.timestamp), (0, 1_700_000_000));
    // breadth first, names in QString order (upper case before lower case)
    assert_eq!(
        m.items(),
        [
            item("Readme.txt", false),
            item("dolphin", true),
            item("nfc", true),
            item("dolphin/a.txt", false),
            item("nfc/assets", true),
            item("nfc/assets/x.nfc", false)
        ]
    );
    assert_eq!(m.file("nfc/assets/x.nfc"), Some((7, "cc")));
    // CRLF and surrounding spaces are trimmed; a trailing slash on a folder is dropped
    assert!(Manifest::parse(b"V:1\r\nT:5\r\n  D:a/  \r\nF:x:1:a/b\r\n").is_ok());
    // incomplete, an empty line, an unknown line, a bad size, a file before its folder
    for bad in [&b"V:1\n"[..], b"V:1\n\nT:5\n", b"V:1\nT:5\nX:1\n", b"V:1\nT:5\nF:x:big:a\n", b"V:1\nT:5\nF:x:1:a/b\n"] {
        assert!(Manifest::parse(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
    }
}

#[test]
fn names_that_leave_ext_make_a_manifest_invalid() {
    // fixed in both: these used to be accepted, and a device's own manifest could name what to remove
    for line in ["F:x:1:../int/.bt.keys", "F:x:1:a/../../int/x", "D:..", "D:./a", "F:x:1:", "F:x:1:a//b", "F:x:1:a\\b", "F:x:1:esc\u{1b}"] {
        let text = format!("V:1\nT:5\nD:a\n{line}\n");
        assert!(Manifest::parse(text.as_bytes()).is_err(), "{line:?}");
    }
    // and nothing can live under a file
    assert!(Manifest::parse(b"V:1\nT:5\nF:x:1:a\nF:y:1:a/b\n").is_err());
}

#[test]
fn a_fresh_install_removes_every_file_first_then_writes_all() {
    let local = Manifest::parse(NEW.as_bytes()).unwrap();
    for device in [DeviceManifest::Missing, DeviceManifest::Invalid] {
        let p = plan(&local, &device);
        assert_eq!(
            p.remove,
            [item("Manifest", false), item("nfc/assets/x.nfc", false), item("dolphin/a.txt", false), item("Readme.txt", false)]
        );
        let mut write = local.items();
        write.push(item("Manifest", false));
        assert_eq!(p.write, write);
    }
    // a manifest that could not be read is compared as an empty one: nothing removed first
    let p = plan(&local, &DeviceManifest::Unread);
    assert_eq!(p.remove, [item("Manifest", false)]);
    assert_eq!(p.write.len(), 7);
}

#[test]
fn an_update_sends_only_the_differences() {
    let local = Manifest::parse(NEW.as_bytes()).unwrap();
    let old =
        "V:0\nT:1600000000\nD:dolphin\nD:nfc\nD:nfc/assets\nD:old\nF:bb:5:dolphin/a.txt\nF:dd:7:nfc/assets/x.nfc\nF:ee:1:old/gone.txt\n";
    let p = plan(&local, &DeviceManifest::Read(Manifest::parse(old.as_bytes()).unwrap()));
    assert_eq!(p.remove, [item("nfc/assets/x.nfc", false), item("Manifest", false), item("old/gone.txt", false)]);
    assert_eq!(p.write, [item("Readme.txt", false), item("Manifest", false), item("nfc/assets/x.nfc", false)]);
    // the same install again: nothing at all
    let p = plan(&local, &DeviceManifest::Read(local.clone()));
    assert!(p.remove.is_empty() && p.write.is_empty());
}

#[test]
fn a_bundle_must_hold_what_its_manifest_says() {
    let (a, b) = (b"hello".as_slice(), b"world!!".as_slice());
    let manifest = |b_md5: &str| format!("V:0\nT:1\nD:d\nF:{}:5:a.txt\nF:{b_md5}:7:d/b.txt\n", md5(a));
    let good = manifest(&md5(b));
    let ok = tgz(&[
        ("resources/", None),
        ("resources/Manifest", Some(good.as_bytes())),
        ("resources/a.txt", Some(a)),
        ("resources/d/", None),
        ("resources/d/b.txt", Some(b)),
    ]);
    let bundle = read_bundle(&ok[..], &BundleLimits::default()).unwrap();
    assert_eq!(bundle.files.keys().collect::<Vec<_>>(), ["Manifest", "a.txt", "d/b.txt"]);

    // fixed in both: a file the manifest names but the bundle lacks was written empty
    let missing = tgz(&[("resources/", None), ("resources/Manifest", Some(good.as_bytes())), ("resources/a.txt", Some(a))]);
    assert!(read_bundle(&missing[..], &BundleLimits::default()).unwrap_err().0.contains("d/b.txt, which the bundle lacks"));
    // and one with other bytes was written as it was
    let wrong = manifest(&md5(b"other!!"));
    let differs = tgz(&[
        ("resources/", None),
        ("resources/Manifest", Some(wrong.as_bytes())),
        ("resources/a.txt", Some(a)),
        ("resources/d/", None),
        ("resources/d/b.txt", Some(b)),
    ]);
    assert!(read_bundle(&differs[..], &BundleLimits::default()).unwrap_err().0.contains("differs"));
    // no manifest at all
    let none = tgz(&[("resources/", None), ("resources/a.txt", Some(a))]);
    assert!(read_bundle(&none[..], &BundleLimits::default()).is_err());
}
