//! Backup archives for restore tests: one that restores, old ones that must still restore, and
//! hostile ones that both apps must refuse before they write anything to the device (or to the
//! local disk). `flipper-fake --write-archives DIR` writes them all; compare-cli.sh gives each
//! to both CLIs.
//!
//! The tar headers are built by hand so that every field can be wrong on purpose.

use std::io::Write;

const BLOCK: usize = 512;

/// One raw tar header and its data. `size` is what the header claims; `data` what follows.
#[derive(Clone)]
pub struct Raw {
    pub name: Vec<u8>,
    pub typeflag: u8,
    pub size_field: Vec<u8>,
    pub data: Vec<u8>,
    pub linkname: Vec<u8>,
    pub prefix: Vec<u8>,
    /// Added to the correct checksum (0 = correct).
    pub checksum_delta: i64,
}

fn octal(value: u64, width: usize) -> Vec<u8> {
    format!("{value:0w$o}", w = width - 1).into_bytes()
}

impl Raw {
    pub fn file(name: &str, data: &[u8]) -> Raw {
        Raw {
            name: name.as_bytes().to_vec(),
            typeflag: b'0',
            size_field: octal(data.len() as u64, 12),
            data: data.to_vec(),
            linkname: Vec::new(),
            prefix: Vec::new(),
            checksum_delta: 0,
        }
    }

    pub fn dir(name: &str) -> Raw {
        let mut r = Raw::file(name, b"");
        if !name.ends_with('/') {
            r.name.push(b'/');
        }
        r.typeflag = b'5';
        r
    }

    pub fn header(&self) -> [u8; BLOCK] {
        let mut h = [0u8; BLOCK];
        let n = self.name.len().min(100);
        h[..n].copy_from_slice(&self.name[..n]);
        h[100..107].copy_from_slice(&octal(if self.typeflag == b'5' { 0o755 } else { 0o664 }, 8));
        h[108..115].copy_from_slice(&octal(1000, 8));
        h[116..123].copy_from_slice(&octal(1000, 8));
        let s = self.size_field.len().min(12);
        h[124..124 + s].copy_from_slice(&self.size_field[..s]);
        h[136..147].copy_from_slice(&octal(1_700_000_000, 12));
        h[156] = self.typeflag;
        let l = self.linkname.len().min(100);
        h[157..157 + l].copy_from_slice(&self.linkname[..l]);
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let p = self.prefix.len().min(155);
        h[345..345 + p].copy_from_slice(&self.prefix[..p]);
        h[148..156].copy_from_slice(b"        ");
        let sum = h.iter().map(|&b| i64::from(b)).sum::<i64>() + self.checksum_delta;
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        h
    }
}

/// A tar stream from raw entries, data padded to whole blocks, with or without the two zero
/// blocks that end it.
pub fn tar(entries: &[Raw], end_blocks: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        out.extend_from_slice(&e.header());
        out.extend_from_slice(&e.data);
        out.resize(out.len() + (BLOCK - e.data.len() % BLOCK) % BLOCK, 0);
    }
    if end_blocks {
        out.resize(out.len() + 2 * BLOCK, 0);
    }
    out
}

pub fn gzip(data: &[u8]) -> Vec<u8> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(data).expect("in memory");
    gz.finish().expect("in memory")
}

/// The `/int` of [`crate::FakeFlipper::standard`] as entries (what a backup of it holds),
/// with the bytes the fake serves.
pub fn standard_int() -> Vec<Raw> {
    let fake = crate::FakeFlipper::standard();
    let mut out = vec![Raw::dir("int/")];
    for n in fake.nodes.iter().filter(|n| n.path.starts_with("/int/")) {
        let name = format!("int/{}", &n.path[5..]);
        out.push(if n.dir { Raw::dir(&name) } else { Raw::file(&name, &fake.data_of(n)) });
    }
    // parents before children, as a backup lists them
    out.sort_by_key(|r| r.name.iter().filter(|&&b| b == b'/').count() - usize::from(r.typeflag == b'5'));
    out
}

fn with(extra: Vec<Raw>) -> Vec<Raw> {
    let mut v = standard_int();
    v.extend(extra);
    v
}

/// Archives that must restore: (name, gzip bytes).
pub fn good() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        // What older Argus/qFlipper builds wrote: no end blocks, a stray zero block after a
        // file that fills its blocks (/int/.bt.settings is 1024 bytes).
        ("old-format", {
            let mut t = Vec::new();
            for e in standard_int() {
                t.extend_from_slice(&tar(std::slice::from_ref(&e), false));
                if !e.data.is_empty() && e.data.len() % BLOCK == 0 {
                    t.extend_from_slice(&[0u8; BLOCK]);
                }
            }
            gzip(&t)
        }),
        // GNU tar pads the archive to 10 KiB records with zeros after the end blocks.
        ("record-padding", {
            let mut t = tar(&standard_int(), true);
            t.resize(t.len().div_ceil(10240) * 10240, 0);
            gzip(&t)
        }),
        // A file of 150 KiB: the write gets status pings (one per 1 KiB chunk x chunksPerPing).
        (
            "large-file",
            gzip(&tar(&with(vec![Raw::file("int/large.bin", &(0..150 * 1024).map(|i| (i * 7 % 251) as u8).collect::<Vec<_>>())]), true)),
        ),
    ]
}

/// Archives both apps must refuse before writing anything: (name, why, gzip bytes).
pub fn hostile() -> Vec<(&'static str, &'static str, Vec<u8>)> {
    let link = |name: &str, flag: u8, target: &str| {
        let mut r = Raw::file(name, b"");
        r.typeflag = flag;
        r.linkname = target.as_bytes().to_vec();
        r
    };
    let mut out: Vec<(&'static str, &'static str, Vec<u8>)> = vec![
        (
            "dotdot",
            "folders named .. lead out of the extraction folder",
            gzip(&tar(&[Raw::dir("int/"), Raw::dir("int/../"), Raw::dir("int/../../"), Raw::file("int/../../escaped.txt", b"out")], true)),
        ),
        ("dotdot-file", "a file name with ..", gzip(&tar(&with(vec![Raw::file("int/../escaped.txt", b"out")]), true))),
        ("dot", "a . part", gzip(&tar(&with(vec![Raw::file("int/./dot.txt", b"x")]), true))),
        ("empty-part", "an empty part (//)", gzip(&tar(&with(vec![Raw::file("int//twice.txt", b"x")]), true))),
        ("absolute", "absolute names", gzip(&tar(&[Raw::dir("/"), Raw::dir("/int/"), Raw::file("/int/abs.txt", b"x")], true))),
        (
            "outside-int",
            "a top-level folder other than int",
            gzip(&tar(&with(vec![Raw::dir("ext/"), Raw::file("ext/evil.txt", b"x")]), true)),
        ),
        ("symlink", "a symbolic link", gzip(&tar(&with(vec![link("int/link", b'2', "/etc/passwd")]), true))),
        ("hardlink", "a hard link", gzip(&tar(&with(vec![link("int/hard", b'1', "int/favorites.txt")]), true))),
        ("backslash", "a backslash in a name", gzip(&tar(&with(vec![Raw::file("int/a\\b", b"x")]), true))),
        ("control", "a control character in a name", gzip(&tar(&with(vec![Raw::file("int/esc\x1b[2J", b"x")]), true))),
        ("non-ascii", "a name the firmware refuses (not ASCII)", gzip(&tar(&with(vec![Raw::file("int/caf\u{e9}.txt", b"x")]), true))),
        ("duplicate", "the same name twice", gzip(&tar(&with(vec![Raw::file("int/favorites.txt", b"second copy")]), true))),
        (
            "missing-parent",
            "a file whose folder is not in the archive",
            gzip(&tar(&with(vec![Raw::file("int/nowhere/x.txt", b"x")]), true)),
        ),
        ("no-int", "no int folder at all", gzip(&tar(&[Raw::dir("other/"), Raw::file("other/x", b"x")], true))),
        ("empty", "an archive with no entries", gzip(&tar(&[], true))),
        ("bad-checksum", "a header checksum that does not match", {
            let mut v = with(vec![Raw::file("int/x.txt", b"x")]);
            v.last_mut().expect("one").checksum_delta = 1;
            gzip(&tar(&v, true))
        }),
        ("long-name", "a name filling all 100 bytes (no NUL)", {
            let name = format!("int/{}", "n".repeat(96));
            gzip(&tar(&with(vec![Raw::file(&name, b"x")]), true))
        }),
        ("prefix", "a ustar prefix (the name is longer than it looks)", {
            let mut r = Raw::file("int/short.txt", b"x");
            r.prefix = b"elsewhere".to_vec();
            gzip(&tar(&with(vec![r]), true))
        }),
        ("garbage-size", "a size field that is not octal", {
            let mut r = Raw::file("int/x.txt", b"x");
            r.size_field = b"00000000x01\0".to_vec();
            gzip(&tar(&with(vec![r]), true))
        }),
        ("base256-size", "a binary (base-256) size", {
            let mut r = Raw::file("int/x.txt", b"x");
            r.size_field = [0x80u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1].to_vec();
            gzip(&tar(&with(vec![r]), true))
        }),
        ("dir-with-data", "a folder entry with a size", {
            let mut r = Raw::dir("int/withdata/");
            r.size_field = octal(5, 12);
            r.data = b"hello".to_vec();
            gzip(&tar(&with(vec![r]), true))
        }),
        ("huge-claim", "a file that claims 8 GiB and is cut short", {
            let mut r = Raw::file("int/huge.bin", b"just a little");
            r.size_field = octal(8 << 30, 12);
            gzip(&tar(&with(vec![r]), false))
        }),
        (
            "huge-file",
            "one file over the 16 MiB limit",
            gzip(&tar(&with(vec![Raw::file("int/huge.bin", &vec![0u8; (16 << 20) + 1])]), true)),
        ),
        ("truncated-tar", "the data of the last file is cut", {
            let t = tar(&with(vec![Raw::file("int/cut.bin", &[7u8; 3000])]), true);
            gzip(&t[..t.len() - 2 * BLOCK - 2048])
        }),
        ("truncated-header", "the archive ends inside a header", {
            let t = tar(&standard_int(), false);
            gzip(&t[..t.len() - 100])
        }),
    ];
    let good = gzip(&tar(&standard_int(), true));
    out.push(("truncated-gzip", "the gzip stream is cut", good[..good.len() - 30].to_vec()));
    out.push(("not-gzip", "not gzip at all", tar(&standard_int(), true)));
    out
}

/// An update package: one folder of files with a manifest, as Flipper's build makes them
/// (made-up contents). `valid` false gives a manifest the updater refuses.
pub fn update_package(valid: bool) -> Vec<u8> {
    let manifest: &[u8] = if valid {
        b"Filetype: Flipper firmware upgrade configuration\nVersion: 2\nInfo: f7-update-test\nTarget: 7\nLoader: updater.bin\nFirmware: firmware.dfu\nRadio: radio.bin\nResources: resources.tar\n"
    } else {
        b"Filetype: something else\n"
    };
    let pattern = |n: usize, seed: usize| (0..n).map(|i| ((i * 31 + seed) % 251) as u8).collect::<Vec<u8>>();
    gzip(&tar(
        &[
            Raw::dir("f7-update-test/"),
            Raw::file("f7-update-test/firmware.dfu", &pattern(4096, 1)),
            Raw::file("f7-update-test/radio.bin", &pattern(2048, 2)),
            // over 100 KiB: its write gets status pings
            Raw::file("f7-update-test/resources.tar", &pattern(120 * 1024, 3)),
            Raw::file("f7-update-test/update.fuf", manifest),
            Raw::file("f7-update-test/updater.bin", &pattern(3000, 4)),
        ],
        true,
    ))
}

/// More than 80 MiB of tar inside a small gzip: both apps stop decompressing at the limit.
pub fn bomb() -> Vec<u8> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut head = tar(&[Raw::dir("int/")], false);
    for i in 0..6 {
        let r = Raw::file(&format!("int/zeros{i}.bin"), b"");
        let mut r = r;
        r.size_field = octal(15 << 20, 12);
        head.extend_from_slice(&r.header());
        gz.write_all(&head).expect("in memory");
        head.clear();
        let zeros = vec![0u8; 1 << 20];
        for _ in 0..15 {
            gz.write_all(&zeros).expect("in memory");
        }
    }
    gz.write_all(&[0u8; 2 * BLOCK]).expect("in memory");
    gz.finish().expect("in memory")
}
