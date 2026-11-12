//! Differential test: the Rust capture metadata (`meta::describe`) against the C++ app's own
//! parser, run as the Qt oracle in tests/qt-oracle (build it with `sh build.sh OUT`, then run
//! `ARGUS_QT_ORACLE=OUT cargo test -p argus-capture --test qt_oracle`). Without the variable the
//! test says so and passes; the Linux CI job, which has Qt, runs it.
//!
//! Inputs are generated from a fixed seed, at most ~1 KiB each: Flipper-style key/value lines
//! with the awkward parts mixed in (carriage returns, invalid UTF-8, BOMs, Unicode spaces,
//! comments, repeated and mixed-case keys, numbers that overflow), under every file suffix.

use std::path::PathBuf;
use std::process::Command;

use argus_capture::meta;

const CASES: usize = 3000;
const BATCH: usize = 250;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

const KEYS: &[&str] = &[
    "Filetype",
    "filetype",
    "Version",
    "VERSION",
    "Device type",
    "UID",
    "uid",
    "ATQA",
    "SAK",
    "Frequency",
    "Preset",
    "Protocol",
    "RAW_Data",
    "raw_data",
    "Bit",
    "Key",
    "name",
    "type",
    "Data",
    "Pages total",
    "",
    " ",
    "#Key",
    "Ver sion",
];
const VALUES: &[&str] = &[
    "Flipper NFC device",
    "Flipper SubGhz RAW File",
    "4",
    "+7",
    "-2",
    "99999999999",
    "3x",
    "",
    "433920000",
    "04 A2 26 BA",
    "100 -200  300",
    "1\t2",
    "-2147483648",
    "2147483648",
    "  spaced  ",
    "a:b:c",
    "x # not a comment",
    "\u{00A0}nbsp\u{00A0}",
    "\u{2028}line\u{3000}",
    "caf\u{e9}",
    "\u{FEFF}bom",
    "\u{85}nel",
];
const NOISE: &[&[u8]] = &[
    b"\r",
    b"\r\n",
    b"\n",
    b"\n\r",
    b"\xE0\x0A",
    b"\xE1\x80",
    b"\xF0\x90",
    b"\xFF",
    b"\x80",
    b"\xED\xA0\x80",
    b"\xEF\xBB\xBF",
    b"\x0b",
    b"\x0c",
    b"\x00",
    b"\t",
    b"#",
    b":",
    b"  ",
    b"\xC2\x85",
    b"\xC2\xA0",
    b"\xE2\x80\xA8",
];
const SUFFIXES: &[&str] = &["nfc", "NFC", "sub", "Sub", "ir", "rfid", "ibtn", "csv", "pcap", "pcapng", "txt", "log", "bin", ""];

fn generate(rng: &mut Rng) -> Vec<u8> {
    let mut out = Vec::new();
    if rng.below(8) == 0 {
        out.extend_from_slice(b"\xEF\xBB\xBF");
    }
    let lines = rng.below(14);
    for _ in 0..lines {
        match rng.below(10) {
            0 => {
                // raw bytes
                for _ in 0..rng.below(24) {
                    out.push(rng.next() as u8);
                }
            }
            1 => out.extend_from_slice(rng.pick(NOISE)),
            _ => {
                out.extend_from_slice(rng.pick(KEYS).as_bytes());
                if rng.below(6) != 0 {
                    out.extend_from_slice(if rng.below(4) == 0 { b" : " } else { b": " });
                }
                out.extend_from_slice(rng.pick(VALUES).as_bytes());
                if rng.below(5) == 0 {
                    out.extend_from_slice(rng.pick(NOISE));
                }
            }
        }
        out.extend_from_slice(if rng.below(6) == 0 { rng.pick(NOISE) } else { b"\n" });
        if out.len() > 1024 {
            break;
        }
    }
    out
}

#[test]
fn rust_metadata_equals_the_cpp_apps() {
    let Some(oracle) = std::env::var_os("ARGUS_QT_ORACLE") else {
        eprintln!("ARGUS_QT_ORACLE not set: build tests/qt-oracle (sh build.sh OUT) to run this comparison");
        return;
    };
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("oracle{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // ARGUS_ORACLE_SEED picks other inputs (a nonzero number)
    let seed = std::env::var("ARGUS_ORACLE_SEED").ok().and_then(|s| s.parse().ok()).filter(|&s| s != 0).unwrap_or(0x5eed_a5c1_1f1a_0001);
    let mut rng = Rng(seed);
    let mut mismatches = Vec::new();
    for batch in 0..CASES / BATCH {
        let mut cases = Vec::new();
        for i in 0..BATCH {
            let suffix = rng.pick(SUFFIXES);
            let name = if suffix.is_empty() { format!("c{batch}_{i}") } else { format!("c{batch}_{i}.{suffix}") };
            let data = generate(&mut rng);
            std::fs::write(dir.join(&name), &data).unwrap();
            cases.push((name, data));
        }
        let out = Command::new(&oracle).args(cases.iter().map(|(n, _)| dir.join(n))).output().unwrap();
        assert!(out.status.success(), "oracle failed: {}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8(out.stdout).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), cases.len());
        for ((name, data), line) in cases.iter().zip(lines) {
            let (kind, meta) = meta::describe(name, data);
            let want = format!("{kind}\t{meta}");
            if line != want {
                mismatches.push(format!("{name} ({data:02x?})\n  C++:  {line}\n  Rust: {want}"));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(mismatches.is_empty(), "{} of {CASES} differ; first:\n{}", mismatches.len(), mismatches[..mismatches.len().min(3)].join("\n"));
}
