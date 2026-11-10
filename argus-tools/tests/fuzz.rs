//! Property tests and a mutation fuzzer for every parser that sees hostile bytes. The property
//! tests run in `cargo test`; the long mutation run is `cargo test --release --test fuzz -- --ignored`
//! (ARGUS_FUZZ_ITERS sets the iteration count). cargo-fuzz targets for the same entry points are
//! in ../fuzz (they need a nightly toolchain).

mod common;

use std::io::Read;

use argus_tools::api::{self, FirmwareApi};
use argus_tools::heatshrink::{self, Params};
use argus_tools::package::{self, Limits};
use argus_tools::{capture, fap, flipperfile, limbo, paths};
use common::*;
use proptest::prelude::*;

const FW: FirmwareApi = FirmwareApi { major: 88, minor: 2, target: 7 };

fn small_limits() -> Limits {
    Limits { total_bytes: 4 << 20, nested_bytes: 2 << 20, app_bytes: 1 << 20, entries: 2000, depth: 4, apps: 200 }
}

/// Scan bytes as a package in a fresh folder; it may fail, but must not panic, and nothing may be
/// written except `<out>/<sha256>.fap`.
fn scan_contained(data: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    let pkg = root.path().join("p");
    std::fs::write(&pkg, data).unwrap();
    let _ = package::scan(&pkg, &root.path().join("out"), small_limits(), Some(FW));
    for f in files_below(root.path()) {
        assert!(f == "p" || (f.starts_with("out/") && f.ends_with(".fap")), "wrote {f}");
    }
}

fn inspect_any(data: &[u8]) {
    if let Ok(info) = fap::inspect(data) {
        let _ = api::check(&info, &FW, api::builtin_table(&FW).as_ref());
    }
}

/// A deterministic xorshift PRNG for the mutation fuzzer.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mutate(rng: &mut Rng, base: &[u8]) -> Vec<u8> {
    let mut d = base.to_vec();
    for _ in 0..1 + rng.below(8) {
        if d.is_empty() {
            d.push(rng.next() as u8);
        }
        let i = rng.below(d.len());
        match rng.below(6) {
            0 => d[i] ^= 1 << rng.below(8),
            1 => d[i] = rng.next() as u8,
            2 => {
                let v = [0u8, 0xff, 0x7f, 0x80][rng.below(4)];
                let n = rng.below(5).min(d.len() - i);
                d[i..i + n].fill(v);
            }
            3 => d.truncate(i),
            4 => {
                let n = rng.below(64);
                let chunk: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                d.splice(i..i, chunk);
            }
            _ => {
                if i + 4 <= d.len() {
                    d[i..i + 4].copy_from_slice(&u32::MAX.to_le_bytes()[..]);
                }
            }
        }
    }
    d
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn fap_inspect_never_panics(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        inspect_any(&data);
        let mut elfish = b"\x7fELF\x01\x01".to_vec();
        elfish.extend_from_slice(&data);
        inspect_any(&elfish);
    }

    #[test]
    fn mutated_real_fap_never_panics(seed in any::<u64>()) {
        let mut rng = Rng(seed | 1);
        inspect_any(&mutate(&mut rng, &read_fixture("snake_game.fap")));
    }

    #[test]
    fn heatshrink_decoder_is_bounded(data in proptest::collection::vec(any::<u8>(), 0..2048), w in 4u8..=15, l in 3u8..15) {
        prop_assume!(l < w);
        let mut out = Vec::new();
        // each input bit yields at most one output byte per 1+w+l bits of backref with count up to 2^l
        let _ = heatshrink::Decoder::new(data.as_slice(), Params { window_bits: w, lookahead_bits: l }).take(1 << 22).read_to_end(&mut out);
        prop_assert!(out.len() <= data.len() * 8 * (1usize << l));
    }

    #[test]
    fn heatshrink_round_trip(data in proptest::collection::vec(any::<u8>(), 0..3000), w in 4u8..=10, l in 3u8..8) {
        prop_assume!(l < w);
        let p = Params { window_bits: w, lookahead_bits: l };
        let mut out = Vec::new();
        heatshrink::Decoder::new(heatshrink::encode(&data, p).as_slice(), p).read_to_end(&mut out).unwrap();
        prop_assert_eq!(out, data);
    }

    #[test]
    fn package_scan_random_bytes(data in proptest::collection::vec(any::<u8>(), 0..2048), kind in 0u8..5) {
        let mut d = match kind {
            0 => vec![0x1f, 0x8b],
            1 => b"PK\x03\x04".to_vec(),
            2 => heatshrink::encode_with_header(b"", Params { window_bits: 8, lookahead_bits: 4 }),
            3 => b"\x7fELF".to_vec(),
            _ => Vec::new(),
        };
        d.extend_from_slice(&data);
        scan_contained(&d);
    }

    #[test]
    fn package_scan_mutated_packages(seed in any::<u64>(), which in 0usize..3) {
        let mut rng = Rng(seed | 1);
        let base = [read_fixture("update-f7-test.tgz"), read_fixture("resources-plain.tar"), read_fixture("resources-c.ths")];
        scan_contained(&mutate(&mut rng, &base[which]));
    }

    #[test]
    fn tar_names_never_escape(name in "[a-z./\\\\]{1,60}") {
        let snake = read_fixture("snake_game.fap");
        let fname = format!("{name}.fap");
        scan_contained(&tar_of(&[(fname.as_str(), &snake)]));
        let e = paths::entry_name(fname.as_bytes());
        let c = paths::category_for(&e);
        prop_assert!(paths::valid_category(&c));
        let f = paths::device_file_name(&e, "0123456789abcdef");
        prop_assert!(!f.contains('/') && !f.starts_with('.') && f.ends_with(".fap"));
    }

    #[test]
    fn flipper_files_and_captures(data in proptest::collection::vec(any::<u8>(), 0..2048), ft in 0usize..7) {
        let heads = ["", "Filetype: Flipper SubGhz Key File\n", "Filetype: Flipper SubGhz RAW File\n", "Filetype: IR signals file\n",
                     "Filetype: Flipper NFC device\n", "Filetype: Flipper RFID key\n", "Filetype: Flipper iButton key\n"];
        let mut d = heads[ft].as_bytes().to_vec();
        d.extend_from_slice(&data);
        let _ = flipperfile::parse(&d);
        let mut b = capture::Builder::default();
        b.add(&d, capture::Location { origin: "t".into(), source: "t".into(), path: "/ext/subghz/x.sub".into() });
        for c in b.finish(0).captures {
            let _ = capture::explain(&c);
        }
    }

    #[test]
    fn limbo_output_parsers(s in "\\PC{0,400}", pre in 0usize..4) {
        let heads = ["", "ARGUS-FAP-BEGIN a.fap 00\n", "ARGUS-SRC-BEGIN 00\n", "ARGUS-ERROR x\n"];
        let text = format!("{}{s}\nARGUS-FAP-END\nARGUS-SRC-END\n", heads[pre]);
        let out = tempfile::tempdir().unwrap();
        let _ = limbo::parse_build_output(&text, &FW, out.path());
        let _ = limbo::parse_fetch_output(&text);
        let _ = limbo::parse_response(text.as_bytes());
    }

    #[test]
    fn device_paths(p in "[/a-z.\\\\\\x00]{0,30}") {
        if paths::valid_device_path(&p) {
            prop_assert!(!p.split('/').any(|x| x == ".."));
            let m = paths::mirror_path(std::path::Path::new("/m"), &p).unwrap();
            prop_assert!(m.starts_with("/m"));
        }
    }
}

#[test]
#[ignore]
fn mutation_fuzz_long() {
    let iters: usize = std::env::var("ARGUS_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(20_000);
    let bases = [
        read_fixture("snake_game.fap"),
        read_fixture("update-f7-test.tgz"),
        read_fixture("resources-plain.tar"),
        read_fixture("resources-c.ths"),
        read_fixture("captures/ext/nfc/badge.nfc"),
    ];
    let mut rng = Rng(0x5eed_1234_abcd_ef01);
    for i in 0..iters {
        let which = i % bases.len();
        let d = mutate(&mut rng, &bases[which]);
        match which {
            0 => inspect_any(&d),
            4 => {
                let _ = flipperfile::parse(&d);
                let _ = capture::describe(&d);
            }
            _ => scan_contained(&d),
        }
    }
}
