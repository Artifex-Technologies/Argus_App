//! The STM32WB55 layer against the C++ app's own (`dfu/device/stm32wb55/`, built into the oracle):
//!
//! - option bytes files (Flipper's `ob.data` and 2000 damaged variants per seed): both accept the
//!   same files and read the same fields;
//! - option bytes from raw bytes: the same fields, the same answer to every change asked for, and the
//!   same 128 bytes to write;
//! - the device operations and recovery flows on one fake bootloader, request for request (as
//!   tests/qt_device.rs): reading and writing the option bytes, the boot mode, correcting them from a
//!   file, the device information table, OTP, the FUS state and commands, the wireless stack's
//!   address.
//!
//! Needs `ARGUS_DFU_ORACLE` (tests/qt-oracle/build.sh); without it the tests say so and pass.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::fake::{self, flipper_option_bytes, hex, FakeDfu, Faults};
use common::oracle::{differences, run_cpp, Outcome};
use common::Rng;
use flipper_dfu::device::Dfu;
use flipper_dfu::recovery::{self, OptionBytesOutcome};
use flipper_dfu::wb55::{OptionBytes, OptionBytesFile, Wb55, FIELDS};

const OB_DATA: &str = "RDP:0xAA:r\nBOR_LEV:0x4:rw\nnBOOT0:0x1:rw\nnBOOT1:0x1:rw\nnSWBOOT0:0x1:rw\nSRAM2RST:0x0:rw\nSRAM2PE:0x1:rw\n\
nRST_STOP:0x1:rw\nnRST_STDBY:0x1:rw\nnRSTSHDW:0x1:rw\nWWDGSW:0x1:rw\nIWGDSTDBY:0x1:rw\nIWDGSTOP:0x1:rw\nIWDGSW:0x1:rw\n\
IPCCDBA:0x0:rw\nESE:0x1:r\nSFSA:0xD7:r\nFSD:0x0:r\nDDS:0x1:r\nC2OPT:0x1:r\nNBRSD:0x0:r\nSNBRSA:0xB:r\nBRSD:0x0:r\n\
SBRSA:0x12:r\nSBRV:0x35C00:r\nPCROP1A_STRT:0x1FF:r\nPCROP1A_END:0x0:r\nPCROP_RDP:0x1:rw\nPCROP1B_STRT:0x1FF:r\n\
PCROP1B_END:0x0:r\nWRP1A_STRT:0xFF:r\nWRP1A_END:0x0:r\nWRP1B_STRT:0xFF:r\nWRP1B_END:0x0:r\n";

fn oracle() -> Option<PathBuf> {
    let o = std::env::var_os("ARGUS_DFU_ORACLE").map(PathBuf::from);
    if o.is_none() {
        eprintln!("ARGUS_DFU_ORACLE not set: build tests/qt-oracle (sh build.sh OUT) to run this comparison");
    }
    o
}

fn seed(default: u64) -> u64 {
    std::env::var("ARGUS_ORACLE_SEED").ok().and_then(|s| s.parse().ok()).filter(|&s| s != 0).unwrap_or(default)
}

fn tmp(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn describe_file(data: &[u8]) -> String {
    let Ok(f) = OptionBytesFile::parse(data) else { return "bad".into() };
    let mut fields: Vec<(&str, u32)> = f.entries.iter().map(|(f, v, _)| (f.name, *v)).collect();
    fields.sort();
    let mut out = "ok ".to_string();
    for (n, v) in fields {
        out += &format!("{n}={v:x} ");
    }
    out
}

fn damage_file(rng: &mut Rng) -> Vec<u8> {
    let mut lines: Vec<String> = OB_DATA.lines().map(String::from).collect();
    for _ in 0..rng.below(4) {
        let i = rng.below(lines.len());
        match rng.below(16) {
            0 => {
                lines.remove(i);
            }
            1 => {
                let l = lines[i].clone();
                lines.push(l);
            }
            2 => lines[i] = lines[i].replace(":rw", ":r"),
            3 => lines[i] = lines[i].replace(":r", ":rw"),
            4 => lines[i] = lines[i].replace("0x", ""),
            5 => lines[i] = lines[i].replace("0x", "0X"),
            6 => lines[i] = lines[i].replacen(':', ":0x1", 1),
            7 => lines[i] = format!("#{}", lines[i]),
            8 => lines[i] = lines[i].replace(':', " : "),
            9 => {
                lines[i] =
                    rng.pick(&["RDP:0x1CC:r", "RDP:0xCC:r", "RDP:0xAA:rw", "SFSA:0x100:r", "SBRV:0x40000:r", "nBOOT0:0x2:rw"]).to_string()
            }
            10 => {
                lines[i] = rng
                    .pick(&["FOO:0x1:rw", "nboot0:0x1:rw", "nBOOT0:0x1:RW", "nBOOT0:0x1:", "nBOOT0:0x1", "nBOOT0:0x1:rw:x", ""])
                    .to_string()
            }
            11 => {
                lines[i] = lines[i].replace("0x1", rng.pick(&["0x00000001", "0x000000000001", "1x", "0xx1", "+1", "0x1 ", "0xFFFFFFFF1"]))
            }
            12 => lines[i] = lines[i].replace("IWGDSTDBY", "IWDGSTDBY"),
            13 => lines.push("IWDGSTDBY:0x1:rw".into()),
            14 => lines[i] = lines[i].to_lowercase(),
            _ => {
                let j = rng.below(lines.len());
                lines.swap(i, j);
            }
        }
    }
    let ending = *rng.pick(&["\n", "\n", "\r\n", "\r", "\n\n"]);
    let mut out = lines.join(ending).into_bytes();
    match rng.below(6) {
        0 => {}
        1 => out.push(0xFF),
        _ => out.extend(ending.as_bytes()),
    }
    out
}

#[test]
fn option_bytes_files_read_the_same() {
    let Some(oracle) = oracle() else { return };
    let dir = tmp("ob-files");
    let mut rng = Rng(seed(0x0b0b_0000_0000_0001));
    let mut cases = vec![OB_DATA.as_bytes().to_vec(), Vec::new()];
    cases.extend((0..2000).map(|_| damage_file(&mut rng)));
    let mut mismatches = Vec::new();
    let mut ok = 0;
    for (b, batch) in cases.chunks(250).enumerate() {
        let paths: Vec<PathBuf> = batch
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let p = dir.join(format!("{b}_{i}.data"));
                std::fs::write(&p, d).unwrap();
                p
            })
            .collect();
        let out = Command::new(&oracle).arg("obfile").args(&paths).output().unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), batch.len());
        for (d, line) in batch.iter().zip(lines) {
            let want = describe_file(d);
            ok += usize::from(want != "bad");
            if line.trim_end() != want.trim_end() {
                mismatches.push(format!("{:?}\n  C++:  {line}\n  Rust: {want}", String::from_utf8_lossy(d)));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        mismatches.is_empty(),
        "{} of {} differ; first:\n{}",
        mismatches.len(),
        cases.len(),
        mismatches[..mismatches.len().min(3)].join("\n")
    );
    assert!(ok > 200 && ok < cases.len() - 200, "{ok} of {} accepted", cases.len());
}

fn describe_data(raw: &[u8], sets: &[(String, u32)]) -> String {
    let Ok(mut ob) = OptionBytes::from_bytes(raw) else { return "bad".into() };
    let mut out = String::new();
    for (n, v) in ob.fields() {
        out += &format!("{n}={v:x} ");
    }
    out += "\n";
    for (n, v) in sets {
        out.push(if ob.set(n, *v).is_ok() { '1' } else { '0' });
    }
    out += "\n";
    out += &hex(&ob.encoded());
    out
}

#[test]
fn option_bytes_encode_the_same() {
    let Some(oracle) = oracle() else { return };
    let mut rng = Rng(seed(0x0b0b_da7a_0000_0001));
    let names: Vec<&str> = FIELDS.iter().map(|f| f.name).chain(["IWGDSTDBY", "FOO", ""]).collect();
    let mut mismatches = Vec::new();
    for case in 0..600 {
        let raw = match case % 3 {
            0 => rng.bytes(128),
            1 => flipper_option_bytes(),
            _ => {
                let mut r = flipper_option_bytes();
                let i = rng.below(128);
                r[i] ^= 1 << rng.below(8);
                r
            }
        };
        let raw = if case == 5 { rng.bytes(127) } else { raw };
        let sets: Vec<(String, u32)> = (0..rng.below(5))
            .map(|_| (rng.pick(&names).to_string(), *rng.pick(&[0u32, 1, 2, 0x7, 0xFF, 0x1FF, 0x3FFFF, 0x40000, u32::MAX])))
            .collect();
        let mut args = vec!["obdata".to_string(), if raw.is_empty() { "-".into() } else { hex(&raw) }];
        args.extend(sets.iter().map(|(n, v)| format!("{n}={v:x}")));
        let out = Command::new(&oracle).args(&args).output().unwrap();
        let cpp = String::from_utf8(out.stdout).unwrap();
        let want = describe_data(&raw, &sets);
        if cpp.trim_end() != want.trim_end() {
            mismatches.push(format!("case {case} sets {sets:?}\n  C++:  {cpp}\n  Rust: {want}"));
        }
    }
    assert!(mismatches.is_empty(), "{} differ; first:\n{}", mismatches.len(), mismatches[..mismatches.len().min(2)].join("\n"));
}

#[derive(Clone, Debug)]
enum Op {
    ObRead,
    Boot(bool),
    ObFile(Vec<u8>),
    Version,
    Otp(usize),
    FusState,
    StartFus,
    FusCommand(&'static str, u8),
    Radio(Vec<u8>),
}

fn run_rust(op: &Op, faults: &Faults) -> Outcome {
    let mut d = Dfu::new(FakeDfu::new(faults.clone()));
    let (ok, data): (bool, Vec<u8>) = match op {
        Op::ObRead => match d.option_bytes() {
            Ok(ob) => (true, ob.encoded().to_vec()),
            Err(_) => (false, Vec::new()),
        },
        Op::Boot(normal) => (recovery::set_boot_mode(&mut d, *normal).is_ok(), Vec::new()),
        Op::ObFile(f) => match recovery::download_option_bytes(&mut d, f) {
            Ok(OptionBytesOutcome::Left { .. }) => (true, b"left".to_vec()),
            Ok(OptionBytesOutcome::Written { .. }) => (true, b"written".to_vec()),
            Err(_) => (false, Vec::new()),
        },
        Op::Version => match d.version_info() {
            Ok(v) => (true, format!("{} {}", v.fus, v.wireless).into_bytes()),
            Err(_) => (false, Vec::new()),
        },
        Op::Otp(n) => match d.otp(*n) {
            Ok(v) => (!v.is_empty(), v),
            Err(_) => (false, Vec::new()),
        },
        Op::FusState => match d.fus_state() {
            Ok(s) => (true, format!("{:x}/{:x}", s.status, s.error).into_bytes()),
            Err(_) => (false, Vec::new()),
        },
        Op::StartFus => (recovery::start_fus(&mut d).is_ok(), Vec::new()),
        Op::FusCommand(_, c) => (d.command(&[*c]).is_ok(), Vec::new()),
        Op::Radio(image) => match recovery::download_wireless_stack(&mut d, image, None) {
            Ok(a) => (true, format!("{a:x}").into_bytes()),
            Err(_) => (false, Vec::new()),
        },
    };
    Outcome { ok, data, fake: d.into_inner() }
}

fn args(op: &Op, dir: &Path) -> Vec<String> {
    let mut a: Vec<String> = vec!["device".into()];
    let file = |name: &str, data: &[u8]| {
        let p = dir.join(name);
        std::fs::write(&p, data).unwrap();
        p.display().to_string()
    };
    match op {
        Op::ObRead => a.push("ob-read".into()),
        Op::Boot(n) => a.extend(["boot".into(), if *n { "normal" } else { "dfu" }.into()]),
        Op::ObFile(f) => a.extend(["ob-file".into(), file("ob.data", f)]),
        Op::Version => a.push("version".into()),
        Op::Otp(n) => a.extend(["otp".into(), n.to_string()]),
        Op::FusState => a.push("fus-state".into()),
        Op::StartFus => a.push("start-fus".into()),
        Op::FusCommand(name, _) => a.push((*name).into()),
        Op::Radio(image) => a.extend(["radio".into(), file("radio.bin", image)]),
    }
    a
}

fn scenarios() -> Vec<(String, Op, Faults)> {
    let ok = Faults::default;
    let mut rng = Rng(0x0b_0001);
    let mut rw_differs = OB_DATA.replace("SRAM2RST:0x0:rw", "SRAM2RST:0x1:rw");
    rw_differs = rw_differs.replace("BOR_LEV:0x4:rw", "BOR_LEV:0x3:rw");
    let r_differs = OB_DATA.replace("SFSA:0xD7:r", "SFSA:0xD0:r");
    let dangerous = OB_DATA.replace("SFSA:0xD7:r", "SFSA:0xD0:rw").replace("RDP:0xAA:r", "RDP:0xBB:r");
    let mut zero_table = fake::fus_info_table();
    zero_table[..4].copy_from_slice(&[0; 4]);
    let mut ws_table = vec![0u8; 56];
    ws_table[4..8].copy_from_slice(&[0, 1, 1, 1]);
    ws_table[16..20].copy_from_slice(&[0, 2, 15, 1]);
    vec![
        ("read option bytes".into(), Op::ObRead, ok()),
        ("read option bytes, empty answers".into(), Op::ObRead, Faults { empty_upload: true, ..ok() }),
        ("read random option bytes".into(), Op::ObRead, Faults { option_bytes: Some(rng.bytes(128)), ..ok() }),
        ("boot mode normal".into(), Op::Boot(true), ok()),
        ("boot mode dfu".into(), Op::Boot(false), ok()),
        ("boot mode dfu, random reserved bits".into(), Op::Boot(false), Faults { option_bytes: Some(rng.bytes(128)), ..ok() }),
        ("boot mode, the write refused".into(), Op::Boot(false), Faults { fail_op: Some((3, fake::ERR_WRITE)), ..ok() }),
        ("boot mode, the set address refused".into(), Op::Boot(false), Faults { fail_op: Some((2, fake::ERR_ADDRESS)), ..ok() }),
        ("boot mode, option bytes unreadable".into(), Op::Boot(false), Faults { empty_upload: true, ..ok() }),
        ("ob.data, nothing to correct".into(), Op::ObFile(OB_DATA.into()), ok()),
        ("ob.data, two rw fields differ".into(), Op::ObFile(rw_differs.clone().into_bytes()), ok()),
        ("ob.data, a read-only field differs".into(), Op::ObFile(r_differs.into_bytes()), ok()),
        ("ob.data, SFSA rw and RDP 0xBB".into(), Op::ObFile(dangerous.into_bytes()), ok()),
        ("ob.data, option bytes unreadable".into(), Op::ObFile(rw_differs.into_bytes()), Faults { empty_upload: true, ..ok() }),
        ("ob.data, damaged file".into(), Op::ObFile(b"RDP:0x1CC:r\n".to_vec()), ok()),
        ("version, FUS table".into(), Op::Version, ok()),
        ("version, wireless stack table".into(), Op::Version, Faults { info_table: Some(ws_table), ..ok() }),
        ("version, no magic".into(), Op::Version, Faults { info_table: Some(zero_table), ..ok() }),
        ("version, table below SRAM2A".into(), Op::Version, Faults { table_address: Some(0x2002_FFFF), ..ok() }),
        ("version, table past SRAM".into(), Op::Version, Faults { table_address: Some(0x2003_03F0), ..ok() }),
        ("OTP 16".into(), Op::Otp(16), ok()),
        ("OTP 1024".into(), Op::Otp(1024), ok()),
        ("OTP 5000".into(), Op::Otp(5000), ok()),
        ("FUS state idle".into(), Op::FusState, ok()),
        ("FUS state not running".into(), Op::FusState, Faults { fus: Some([0xFF, 0xFE]), ..ok() }),
        ("FUS state upgrading".into(), Op::FusState, Faults { fus: Some([0x13, 0x00]), ..ok() }),
        ("start FUS, running".into(), Op::StartFus, ok()),
        ("start FUS, not running".into(), Op::StartFus, Faults { fus: Some([0xFF, 0xFE]), ..ok() }),
        ("start FUS, odd state".into(), Op::StartFus, Faults { fus: Some([0x30, 0x02]), ..ok() }),
        ("FUS delete".into(), Op::FusCommand("fus-delete", 0x52), ok()),
        ("FUS upgrade".into(), Op::FusCommand("fus-upgrade", 0x53), ok()),
        ("FUS start wireless stack".into(), Op::FusCommand("fus-start", 0x5A), ok()),
        ("radio image below SFSA".into(), Op::Radio(rng.bytes(3 * 4096 + 7)), ok()),
        ("radio image larger than the room below SFSA".into(), Op::Radio(vec![1; 0xD8 * 4096]), ok()),
        ("radio image, SFSA 0".into(), Op::Radio(vec![1; 100]), {
            let mut ob = OptionBytes::from_bytes(&flipper_option_bytes()).unwrap();
            ob.set("SFSA", 0).unwrap();
            Faults { option_bytes: Some(ob.bytes().to_vec()), ..ok() }
        }),
    ]
}

#[test]
fn device_operations_match_the_cpp_apps() {
    let Some(oracle) = oracle() else { return };
    let dir = tmp("wb55-device");
    let all = scenarios();
    let mut failures = Vec::new();
    for (name, op, faults) in &all {
        let rust = run_rust(op, faults);
        let cpp = match run_cpp(&oracle, &args(op, &dir), faults) {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("{name}: the C++ app {e}"));
                continue;
            }
        };
        let diffs = differences(&rust, &cpp);
        if !diffs.is_empty() {
            failures.push(format!("{name}: {}", diffs.join("; ")));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{} of {} scenarios differ:\n{}", failures.len(), all.len(), failures.join("\n"));
}
