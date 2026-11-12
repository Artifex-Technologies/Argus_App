//! Differential test: the Rust DfuSe parser and memory layout against the C++ app's own
//! (`dfu/dfusefile.cpp`, `dfu/dfumemorylayout.cpp`), built as the oracle in tests/qt-oracle (`sh
//! build.sh OUT`, then `ARGUS_DFU_ORACLE=OUT cargo test -p flipper-dfu --test qt_oracle`). Without the
//! variable the test says so and passes; the Linux CI job, which has Qt, runs it.
//!
//! Files are generated from a seed (`ARGUS_ORACLE_SEED` picks others): well-formed ones with one to
//! three targets, then most of them damaged the ways a hostile or broken file would be (cut
//! anywhere, counts and sizes off by one or huge, elements empty, overlapping, touching or running
//! past 4 GiB, junk after the targets, a wrong suffix, version or CRC), usually with the CRC
//! rewritten so the damage is reached past the checksum. Both must accept the same files and read
//! the same fields and bytes from them.

mod common;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use common::{file_with, fix_crc, fix_image_size, Rng, T};
use flipper_dfu::{dfuse, layout::MemoryLayout};
use md5::{Digest, Md5};

const FILES: usize = 3000;
const BATCH: usize = 250;
const LAYOUTS: usize = 4000;

fn hex(b: &[u8]) -> String {
    if b.is_empty() {
        return "-".into();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn describe_file(bytes: &[u8]) -> String {
    let Ok(f) = dfuse::parse(bytes) else { return "bad".into() };
    let s = f.suffix;
    let mut out =
        format!("ok v={} dev={} pid={} vid={} dfu={} crc={}", f.version, s.bcd_device, s.id_product, s.id_vendor, s.bcd_dfu, s.crc);
    for t in &f.targets {
        out += &format!(" | alt={} named={} name={} size={} n={}", t.alt, t.named, hex(&t.name), t.size, t.elements.len());
        for e in &t.elements {
            out += &format!(" [{} {} {} {}]", e.address, e.data.len(), e.data.len(), hex(&Md5::digest(&e.data)));
        }
    }
    out
}

const BASES: &[u32] = &[0x0800_0000, 0x0801_0000, 0x1FFF_8000, 0x2003_0000, 0x0804_0000];
const EDGES: &[u32] = &[0, 0xFFFF_FFC0, 0xFFFF_FFFF, 0xFFFF_FF00];

fn target(rng: &mut Rng, base: u32) -> T {
    let mut elements: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut at = if rng.below(12) == 0 { *rng.pick(EDGES) } else { base };
    for _ in 0..1 + rng.below(4) {
        let len = *rng.pick(&[1usize, 2, 7, 8, 16, 63, 64, 100, 512]);
        elements.push((at, rng.bytes(len)));
        at = at.wrapping_add(len as u32);
        match rng.below(12) {
            // touching the next one: allowed
            0..=2 => {}
            // one byte into the next one: overlap
            3 => at = at.wrapping_sub(1),
            _ => at = at.wrapping_add(rng.below(0x2000) as u32),
        }
    }
    if rng.below(10) == 0 {
        elements.reverse();
    }
    let mut t = T::new(*rng.pick(&[0u8, 0, 1, 2, 255]), elements);
    t.named = *rng.pick(&[0u32, 1, 0x100, 0xFFFF_FFFF]);
    t.name = match rng.below(4) {
        0 => Vec::new(),
        1 => rng.bytes(255),
        _ => b"ST...".to_vec(),
    };
    t
}

fn generate(rng: &mut Rng) -> Vec<u8> {
    // targets far apart, unless the same base comes up twice for one alternate setting (overlap)
    let mut targets: Vec<T> = (0..1 + rng.below(3))
        .map(|i| {
            let base = if rng.below(10) == 0 { BASES[0] } else { BASES[i] };
            target(rng, base)
        })
        .collect();
    let mut version = 1u8;
    let mut count = None;
    let mut junk = Vec::new();
    match rng.below(18) {
        0 => {
            let t = rng.below(targets.len());
            let n = targets[t].elements.len() as u32;
            targets[t].count = Some(*rng.pick(&[0, n.wrapping_sub(1), n + 1, 0x1000_0000, u32::MAX]));
        }
        1 => {
            let t = rng.below(targets.len());
            let size = targets[t].bytes().len() as u32 - 274;
            targets[t].size = Some(*rng.pick(&[0, size.wrapping_sub(1), size + 1, u32::MAX]));
        }
        2 => {
            let t = rng.below(targets.len());
            let e = rng.below(targets[t].elements.len());
            targets[t].elements[e].1.clear();
        }
        3 => count = Some(*rng.pick(&[0u8, targets.len() as u8 - 1, targets.len() as u8 + 1, 255])),
        4 => version = *rng.pick(&[0u8, 2, 0x1a, 255]),
        5 => {
            let n = 1 + rng.below(20);
            junk = rng.bytes(n);
        }
        _ => {}
    }
    let mut f = file_with(&targets, version, count, &junk);
    match rng.below(16) {
        // cut anywhere, the prefix's size and the CRC kept consistent (or not)
        0 => {
            let at = rng.below(f.len());
            f.truncate(at);
            if rng.below(2) == 0 {
                fix_image_size(&mut f);
                fix_crc(&mut f);
            }
        }
        // a few bytes changed anywhere, then the CRC rewritten (or not)
        1 | 2 => {
            for _ in 0..1 + rng.below(3) {
                let at = rng.below(f.len());
                f[at] = rng.next() as u8;
            }
            if rng.below(4) != 0 {
                fix_crc(&mut f);
            }
        }
        // an element's size field made huge
        3 => {
            if f.len() > 11 + 274 + 8 {
                let at = 11 + 274 + 4;
                f[at..at + 4].copy_from_slice(&(*rng.pick(&[0xFFFF_FFF0u32, 0x8000_0000, 0x0100_0000])).to_le_bytes());
                fix_crc(&mut f);
            }
        }
        // the suffix's signature or length
        4 => {
            let n = f.len();
            f[n - 8 + rng.below(4)] ^= 1 << rng.below(8);
            if rng.below(2) == 0 {
                fix_crc(&mut f);
            }
        }
        _ => {}
    }
    f
}

#[test]
fn rust_dfuse_parser_equals_the_cpp_apps() {
    let Some(oracle) = std::env::var_os("ARGUS_DFU_ORACLE") else {
        eprintln!("ARGUS_DFU_ORACLE not set: build tests/qt-oracle (sh build.sh OUT) to run this comparison");
        return;
    };
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("dfu-oracle{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let seed = std::env::var("ARGUS_ORACLE_SEED").ok().and_then(|s| s.parse().ok()).filter(|&s| s != 0).unwrap_or(0xdf05_e000_0000_0001);
    let mut rng = Rng(seed);
    let (mut mismatches, mut valid) = (Vec::new(), 0);
    let mut reasons = std::collections::BTreeMap::<String, usize>::new();
    for batch in 0..FILES / BATCH {
        let mut cases = Vec::new();
        for i in 0..BATCH {
            let name = format!("f{batch}_{i}.dfu");
            let data = generate(&mut rng);
            std::fs::write(dir.join(&name), &data).unwrap();
            cases.push((name, data));
        }
        let out = Command::new(&oracle).arg("file").args(cases.iter().map(|(n, _)| dir.join(n))).output().unwrap();
        assert!(out.status.success(), "oracle failed: {}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8(out.stdout).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), cases.len());
        for ((name, data), line) in cases.iter().zip(lines) {
            let want = describe_file(data);
            valid += usize::from(want != "bad");
            if let Err(e) = dfuse::parse(data) {
                let kind = format!("{e:?}");
                *reasons.entry(kind.split(['(', ' ']).next().unwrap_or_default().to_string()).or_default() += 1;
            }
            if line != want {
                mismatches.push(format!("{name} ({} bytes, {:?})\n  C++:  {line}\n  Rust: {want}", data.len(), dfuse::parse(data).err()));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("{valid} of {FILES} valid; refused: {reasons:?}");
    assert!(mismatches.is_empty(), "{} of {FILES} differ; first:\n{}", mismatches.len(), mismatches[..mismatches.len().min(3)].join("\n"));
    // the generator must keep producing both kinds
    assert!(valid > FILES / 10 && valid < FILES * 9 / 10, "{valid} of {FILES} valid");
}

/// Real firmware files, when `ARGUS_DFU_REAL` lists some (colon-separated paths; they are not in the
/// repository): both parsers must accept them and read the same.
#[test]
fn real_firmware_files_read_the_same() {
    let (Some(oracle), Some(paths)) = (std::env::var_os("ARGUS_DFU_ORACLE"), std::env::var_os("ARGUS_DFU_REAL")) else {
        eprintln!("ARGUS_DFU_ORACLE and ARGUS_DFU_REAL not both set: no real files compared");
        return;
    };
    for path in std::env::split_paths(&paths) {
        let data = std::fs::read(&path).unwrap();
        let out = Command::new(&oracle).arg("file").arg(&path).output().unwrap();
        let want = describe_file(&data);
        assert!(want.starts_with("ok "), "{}: {:?}", path.display(), dfuse::parse(&data).err());
        assert_eq!(String::from_utf8(out.stdout).unwrap().trim_end(), want, "{}", path.display());
    }
}

fn describe_layout(desc: &[u8], start: u32, end: u64) -> String {
    let Ok(l) = MemoryLayout::parse(desc) else { return "bad".into() };
    let mut out = format!("ok name={} addr={} banks=", hex(&l.name), l.address);
    for b in &l.banks {
        out += &format!("{}*{}{},", b.page_count, b.page_size, char::from(b.kind));
    }
    out += " pages=";
    match l.page_addresses(start, end) {
        Ok(pages) => out.extend(pages.iter().map(|p| format!("{p},"))),
        Err(_) => out += "none",
    }
    out
}

const NAMES: &[&[u8]] = &[b"@Internal Flash   ", b"@Option Bytes  ", b"", b" \x0b\t@OTP Memory\x0c ", b"@x\x00y", b"\xc3\xa9"];
const ADDRS: &[&[u8]] = &[
    b"0x08000000",
    b"0X1FFF8000",
    b"0x1FFF7000",
    b"0x0",
    b"0xFFFFFFFF",
    b"0x123456789",
    b"08000000",
    b"0x",
    b"0xZZ",
    b" 0x08000000 ",
    b"0x0800 0000",
    b"-0x1",
    b"0x+1",
];
const COUNTS: &[&[u8]] =
    &[b"256", b"0256", b"01", b"1", b"0", b"4294967295", b"4294967296", b"99999999999", b"", b"x", b"+1", b" 1", b"1 "];
const SIZES: &[&[u8]] = &[b"04", b"004", b"4", b"128", b"1", b"0", b"", b"x", b"4294967295", b"1024", b"9999999999"];
const MULTS: &[u8] = b"K BMKkx*";
const KINDS: &[u8] = b"gabcdefhA ";

/// Mostly the first (valid) entry of a list, sometimes any.
fn mostly<'a, X>(rng: &mut Rng, xs: &'a [X]) -> &'a X {
    if rng.below(3) != 0 {
        &xs[0]
    } else {
        rng.pick(xs)
    }
}

fn gen_layout(rng: &mut Rng) -> (Vec<u8>, u32, u64) {
    let mut d = rng.pick(NAMES).to_vec();
    if rng.below(40) == 0 {
        d.push(0xff);
    }
    if rng.below(30) != 0 {
        d.push(b'/');
    }
    d.extend(*mostly(rng, ADDRS));
    if rng.below(30) != 0 {
        d.push(b'/');
    }
    let banks = if rng.below(20) == 0 { 0 } else { 1 + rng.below(3) };
    for i in 0..banks {
        if i > 0 {
            d.extend(if rng.below(20) == 0 { &b",,"[..] } else { &b","[..] });
        }
        if rng.below(8) == 0 {
            d.push(b' ');
        }
        d.extend(*mostly(rng, COUNTS));
        if rng.below(25) != 0 {
            d.push(b'*');
        }
        d.extend(*mostly(rng, SIZES));
        if rng.below(15) != 0 {
            d.push(*mostly(rng, MULTS));
        }
        if rng.below(15) != 0 {
            d.push(*mostly(rng, KINDS));
        }
        if rng.below(8) == 0 {
            d.push(*rng.pick(b" \x0b\r"));
        }
    }
    match rng.below(20) {
        0 => d.extend(b"/0x1FFF0000/01*1Ka"),
        1 => d.push(b','),
        _ => {}
    }
    // ranges around the start of the layout and its page boundaries
    let parsed = MemoryLayout::parse(&d).ok();
    let base = parsed.as_ref().map_or(0x0800_0000, |l| l.address);
    let lend = parsed.as_ref().map_or(0x0810_0000, MemoryLayout::end);
    let page = parsed.as_ref().map_or(0x1000, |l| u64::from(l.banks[0].page_size)).max(1);
    let start = match rng.below(6) {
        0 => base,
        1 => base.wrapping_sub(1),
        2 => base.wrapping_add((page * rng.below(4) as u64) as u32),
        3 => base.wrapping_add(rng.below(0x3000) as u32),
        4 => (lend.saturating_sub(1)) as u32,
        _ => rng.next() as u32,
    };
    let end = match rng.below(7) {
        0 => u64::from(start),
        1 => u64::from(start) + 1,
        2 => u64::from(start) + page * rng.below(5) as u64,
        3 => u64::from(start) + rng.below(0x5000) as u64,
        4 => lend,
        5 => lend + 1,
        _ => u64::from(start).saturating_sub(1),
    };
    (d, start, end)
}

#[test]
fn rust_memory_layout_equals_the_cpp_apps() {
    let Some(oracle) = std::env::var_os("ARGUS_DFU_ORACLE") else {
        eprintln!("ARGUS_DFU_ORACLE not set: build tests/qt-oracle (sh build.sh OUT) to run this comparison");
        return;
    };
    let seed = std::env::var("ARGUS_ORACLE_SEED").ok().and_then(|s| s.parse().ok()).filter(|&s| s != 0).unwrap_or(0x1a70_0000_0000_0001);
    let mut rng = Rng(seed);
    let mut cases: Vec<(Vec<u8>, u32, u64)> = (0..LAYOUTS).map(|_| gen_layout(&mut rng)).collect();
    // the descriptors the STM32WB55's bootloader sends, and some it does not
    for (d, s, e) in [
        (&b"@Internal Flash   /0x08000000/256*04Kg"[..], 0x0800_0000u32, 0x0810_0000u64),
        (b"@Internal Flash   /0x08000000/256*04Kg", 0x0800_1800, 0x0800_3001),
        (b"@Internal Flash   /0x08000000/256*04Kg", 0x080F_F000, 0x0810_0001),
        (b"@Option Bytes   /0x1FFF8000/01*128 e", 0x1FFF_8000, 0x1FFF_8080),
        (b"@OTP Memory   /0x1FFF7000/01*1Ke", 0x1FFF_7000, 0x1FFF_7400),
        (b"x/0x0/1*K", 0, 1),
        (b"x/0x0/1*1Mg", 0, 0x10_0000),
        (b"x/0xFFFF0000/16*4Kg", 0xFFFF_0000, 1 << 32),
        (b"x/0xFFFF0000/17*4Kg", 0xFFFF_0000, 1 << 32),
        (b"x/0x0/4294967295*1Ka", 0, 0x1000),
        (b"x/0x08000000/2*1Kg,4*2Kg", 0x0800_0400, 0x0800_2800),
    ] {
        cases.push((d.to_vec(), s, e));
    }
    let mut child = Command::new(&oracle).arg("layout").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut input = String::new();
    for (d, s, e) in &cases {
        input += &format!("{}\t{s:x}\t{e:x}\n", hex(d));
    }
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), cases.len());
    let mut mismatches = Vec::new();
    let mut ok = 0;
    for ((d, s, e), line) in cases.iter().zip(lines) {
        let want = describe_layout(d, *s, *e);
        ok += usize::from(want != "bad");
        if line != want {
            mismatches.push(format!("{:?} {s:#x}..{e:#x}\n  C++:  {line}\n  Rust: {want}", String::from_utf8_lossy(d)));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} of {} differ; first:\n{}",
        mismatches.len(),
        cases.len(),
        mismatches[..mismatches.len().min(3)].join("\n")
    );
    assert!(ok > cases.len() / 20, "only {ok} layouts parsed");
}
