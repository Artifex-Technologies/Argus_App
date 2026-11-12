//! Differential test of the DfuSe protocol: the C++ app's own `DfuseDevice` (dfu/dfusedevice.cpp,
//! built into the oracle with a USB stand-in that opens nothing, tests/qt-oracle/fakeusbdevice.cpp)
//! and the Rust `Dfu` are each run against a fresh copy of the same fake bootloader
//! (tests/common/fake.rs). The C++ app's requests reach the fake as lines over a pipe. Both must send
//! the same requests in the same order (and ask for the same waits), succeed or fail alike, read the
//! same bytes, and leave the fake's memory the same. Scenarios: a firmware file, erase, download,
//! upload, FUS commands and leaving, each against a well-behaved device and against devices that
//! stall, stay busy, ask for huge waits, send short statuses, report a transfer size of 0, refuse a
//! step, or describe a hostile memory layout.
//!
//! Needs the oracle (`sh tests/qt-oracle/build.sh OUT`, then `ARGUS_DFU_ORACLE=OUT`); without it the
//! test says so and passes. Nothing here opens a USB device: the C++ USBDevice is replaced.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use common::fake::{self, hex, FakeDfu, Faults, FLASH, OPTION_BYTES, PAGE};
use common::{file, Rng, T};
use flipper_dfu::device::{Dfu, Transport};
use flipper_dfu::dfuse;

/// The most requests one C++ run may send before it is taken as looping for ever.
const MAX_REQUESTS: usize = 400_000;

#[derive(Clone, Debug)]
enum Op {
    Flash(Vec<u8>),
    Erase(u32, u64),
    Download(u32, u8, Vec<u8>),
    Upload(u32, usize, u8),
    Command(Vec<u8>),
    Leave,
}

struct Outcome {
    ok: bool,
    data: Vec<u8>,
    fake: FakeDfu,
}

fn run_rust(op: &Op, faults: &Faults) -> Outcome {
    let mut d = Dfu::new(FakeDfu::new(faults.clone()));
    let (ok, data) = match op {
        Op::Flash(bytes) => (dfuse::parse(bytes).is_ok_and(|f| d.download_file(&f).is_ok()), Vec::new()),
        Op::Erase(a, n) => (d.erase(*a, *n).is_ok(), Vec::new()),
        Op::Download(a, alt, bytes) => (d.download(bytes, *a, *alt).is_ok(), Vec::new()),
        Op::Upload(a, n, alt) => match d.upload(*a, *n, *alt) {
            Ok(v) => (true, v),
            // the C++ app hands back what it had read so far
            Err(_) => (false, Vec::new()),
        },
        Op::Command(bytes) => (d.command(bytes).is_ok(), Vec::new()),
        Op::Leave => (d.leave().is_ok(), Vec::new()),
    };
    Outcome { ok, data, fake: d.into_inner() }
}

fn spawn(oracle: &Path, op: &Op, dir: &Path) -> Child {
    let mut cmd = Command::new(oracle);
    cmd.arg("device");
    match op {
        Op::Flash(bytes) => {
            let p = dir.join("flash.dfu");
            std::fs::write(&p, bytes).unwrap();
            cmd.arg("flash").arg(p);
        }
        Op::Erase(a, n) => {
            cmd.args(["erase".into(), format!("{a:x}"), n.to_string()]);
        }
        Op::Download(a, alt, bytes) => {
            let p = dir.join("data.bin");
            std::fs::write(&p, bytes).unwrap();
            cmd.args(["download".into(), format!("{a:x}"), alt.to_string()]).arg(p);
        }
        Op::Upload(a, n, alt) => {
            cmd.args(["upload".into(), format!("{a:x}"), n.to_string(), alt.to_string()]);
        }
        Op::Command(bytes) => {
            cmd.args(["command".into(), hex(bytes)]);
        }
        Op::Leave => {
            cmd.arg("leave");
        }
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap()
}

fn unhex(s: &str) -> Vec<u8> {
    if s == "-" {
        return Vec::new();
    }
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

/// Runs the C++ DfuseDevice with its requests served by a fresh fake. Err: it did not finish.
fn run_cpp(oracle: &Path, op: &Op, faults: &Faults, dir: &Path) -> Result<Outcome, String> {
    let mut child = spawn(oracle, op, dir);
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut f = FakeDfu::new(faults.clone());
    let mut result = Err("no RESULT line".to_string());
    for n in 0.. {
        if n > MAX_REQUESTS {
            result = Err(format!("still sending requests after {MAX_REQUESTS} (loops for ever); last: {:?}", f.log.last()));
            break;
        }
        let line = match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(Ok(l)) => l,
            Ok(Err(e)) => {
                result = Err(e.to_string());
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                result = Err(format!("silent for 20 s; last: {:?}", f.log.last()));
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let parts: Vec<&str> = line.split(' ').collect();
        let answer = match parts[..] {
            ["SLEEP", ms] => {
                // what the device asked for, as the C++ app would wait it (an unsigned long)
                f.log.push(format!("SLEEP {ms}"));
                None
            }
            ["ALT", alt] => Some(if f.set_alt(alt.parse().unwrap()).is_ok() { "OK".to_string() } else { "ERR".into() }),
            ["OUT", req, value, data] => Some(match f.control_out(req.parse().unwrap(), value.parse().unwrap(), &unhex(data)) {
                Ok(n) => format!("OK {n}"),
                Err(_) => "ERR".into(),
            }),
            ["IN", req, value, len] => Some(match f.control_in(req.parse().unwrap(), value.parse().unwrap(), len.parse().unwrap()) {
                Ok(d) => format!("DATA {}", hex(&d)),
                Err(_) => "ERR".into(),
            }),
            ["DESC"] => Some(format!("DATA {}", hex(&f.functional_descriptor().unwrap()))),
            ["STRING", alt] => Some(match f.interface_string(alt.parse().unwrap()) {
                Ok(d) => format!("DATA {}", hex(&d)),
                Err(_) => "ERR".into(),
            }),
            ["RESULT", ok, data] => {
                result = Ok((ok == "ok", unhex(data)));
                break;
            }
            _ => {
                result = Err(format!("unexpected line {line:?}"));
                break;
            }
        };
        if let Some(a) = answer {
            if writeln!(stdin, "{a}").is_err() {
                break;
            }
        }
    }
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    let (ok, data) = result?;
    Ok(Outcome { ok, data, fake: f })
}

fn scenarios() -> Vec<(String, Op, Faults)> {
    let mut rng = Rng(0xdf0_0001);
    let fw = rng.bytes(3 * PAGE + 100);
    let firmware = file(&[T::new(0, vec![(FLASH + 0x1000, fw.clone())])]);
    let two = file(&[T::new(0, vec![(FLASH, rng.bytes(10)), (FLASH + 0x8000, rng.bytes(PAGE))])]);
    let with_ob = file(&[T::new(0, vec![(FLASH, rng.bytes(10))]), T::new(1, vec![(OPTION_BYTES, rng.bytes(16))])]);
    let mut bad = firmware.clone();
    bad[400] ^= 1;
    let empty_target = {
        // the C++ parser took this file (no target) as valid; nothing reaches the device either way now
        let mut f = file(&[T::new(0, vec![(FLASH, vec![1])])]);
        f.drain(11..11 + 274 + 9);
        f[10] = 0;
        common::fix_image_size(&mut f);
        common::fix_crc(&mut f);
        f
    };
    let ok = Faults::default;
    let mut zero = fake::descriptor();
    zero[5] = 0;
    zero[6] = 0;
    let mut one = fake::descriptor();
    one[5] = 1;
    one[6] = 0;
    let hostile_layouts: Vec<Vec<u8>> = vec![
        b"@Internal Flash   /0x08000000/1*K".to_vec(),
        b"@Internal Flash   /0x08000000/4294967295*0Kg".to_vec(),
        b"@Internal Flash   /0xZZ/256*04Kg".to_vec(),
        b"@Internal Flash   /0x08000000/1*04Kg".to_vec(),
        b"@Internal Flash   /0x08000000/256*04Mg".to_vec(),
    ];
    let mut out: Vec<(String, Op, Faults)> = vec![
        ("flash firmware".into(), Op::Flash(firmware.clone()), ok()),
        ("flash firmware, busy polls of 25 ms".into(), Op::Flash(firmware.clone()), Faults { busy_poll_ms: 25, extra_busy: 2, ..ok() }),
        ("flash two elements".into(), Op::Flash(two), ok()),
        ("flash with an option-bytes target".into(), Op::Flash(with_ob), ok()),
        ("flash a damaged file".into(), Op::Flash(bad), ok()),
        ("flash a file with no target".into(), Op::Flash(empty_target), ok()),
        ("flash from an error state".into(), Op::Flash(firmware.clone()), Faults { start_in_error: true, ..ok() }),
        ("flash from dnload-idle".into(), Op::Flash(firmware.clone()), Faults { start_dnload_idle: true, ..ok() }),
        (
            "flash, the device refuses the 3rd step".into(),
            Op::Flash(firmware.clone()),
            Faults { fail_op: Some((3, fake::ERR_WRITE)), ..ok() },
        ),
        (
            "flash, the device refuses a data block".into(),
            Op::Flash(firmware.clone()),
            Faults { fail_op: Some((7, fake::ERR_PROG)), ..ok() },
        ),
        ("erase a page".into(), Op::Erase(FLASH, 1), ok()),
        ("erase across pages".into(), Op::Erase(FLASH + 0x1800, 0x1801), ok()),
        ("erase nothing".into(), Op::Erase(FLASH, 0), ok()),
        ("erase past the flash".into(), Op::Erase(FLASH + 0xF_F000, 0x1001), ok()),
        ("erase before the flash".into(), Op::Erase(FLASH - 0x1000, 0x10), ok()),
        ("download without erase".into(), Op::Download(FLASH, 0, rng.bytes(2000)), ok()),
        ("download onto written flash".into(), Op::Download(FLASH, 0, vec![1; 8]), Faults { fail_op: Some((2, fake::ERR_PROG)), ..ok() }),
        ("download the option bytes".into(), Op::Download(OPTION_BYTES, 1, rng.bytes(128)), ok()),
        ("upload the option bytes".into(), Op::Upload(OPTION_BYTES, 128, 1), ok()),
        ("upload past the end".into(), Op::Upload(OPTION_BYTES, 3000, 1), ok()),
        ("upload the OTP".into(), Op::Upload(fake::OTP, 1024, 2), ok()),
        ("upload the info table pointer".into(), Op::Upload(fake::SRAM2A, 4, 0), ok()),
        ("upload nothing".into(), Op::Upload(FLASH, 0, 0), ok()),
        ("upload answered with nothing".into(), Op::Upload(OPTION_BYTES, 128, 1), Faults { empty_upload: true, ..ok() }),
        ("FUS delete".into(), Op::Command(vec![0x52]), ok()),
        ("FUS start wireless stack".into(), Op::Command(vec![0x5A]), ok()),
        ("unknown command".into(), Op::Command(vec![0x99]), ok()),
        ("leave".into(), Op::Leave, ok()),
        ("leave from an error state".into(), Op::Leave, Faults { start_in_error: true, ..ok() }),
        // devices that misbehave
        ("poll timeout with high bytes".into(), Op::Erase(FLASH, 1), Faults { busy_poll_ms: 0x80_8080, ..ok() }),
        ("poll timeout 0xffffff".into(), Op::Command(vec![0x53]), Faults { busy_poll_ms: 0xFF_FFFF, ..ok() }),
        ("busy for ever (erase)".into(), Op::Erase(FLASH, 1), Faults { stuck_busy: true, ..ok() }),
        ("busy for ever (download)".into(), Op::Download(FLASH, 0, vec![1; 10]), Faults { stuck_busy: true, ..ok() }),
        ("never dnload-idle".into(), Op::Download(FLASH, 0, vec![1; 10]), Faults { idle_instead_of_dnload_idle: true, ..ok() }),
        ("short status".into(), Op::Download(FLASH, 0, vec![1; 10]), Faults { status_len: Some(5), ..ok() }),
        ("no status".into(), Op::Erase(FLASH, 1), Faults { status_len: Some(0), ..ok() }),
        ("transfer size 0 (download)".into(), Op::Download(FLASH, 0, vec![1; 10]), Faults { descriptor: Some(zero.clone()), ..ok() }),
        ("transfer size 0 (upload)".into(), Op::Upload(OPTION_BYTES, 16, 1), Faults { descriptor: Some(zero), ..ok() }),
        ("transfer size 1, 70000 bytes".into(), Op::Download(FLASH, 0, vec![7; 70_000]), Faults { descriptor: Some(one.clone()), ..ok() }),
        ("transfer size 1, 65534 bytes".into(), Op::Download(FLASH, 0, vec![7; 65_534]), Faults { descriptor: Some(one), ..ok() }),
        ("no functional descriptor".into(), Op::Download(FLASH, 0, vec![1; 10]), Faults { descriptor: Some(vec![9, 0x21, 0]), ..ok() }),
    ];
    for (i, s) in hostile_layouts.into_iter().enumerate() {
        out.push((format!("hostile layout {i}"), Op::Erase(FLASH, 0x2000), Faults { strings: Some(vec![s.clone()]), ..ok() }));
        out.push((format!("hostile layout {i}, firmware"), Op::Flash(firmware.clone()), Faults { strings: Some(vec![s]), ..ok() }));
    }
    out
}

#[test]
fn cpp_and_rust_send_the_same_requests() {
    let Some(oracle) = std::env::var_os("ARGUS_DFU_ORACLE") else {
        eprintln!("ARGUS_DFU_ORACLE not set: build tests/qt-oracle (sh build.sh OUT) to run this comparison");
        return;
    };
    let oracle = PathBuf::from(oracle);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("dfu-device{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut failures = Vec::new();
    let all = scenarios();
    for (name, op, faults) in &all {
        let rust = run_rust(op, faults);
        let cpp = match run_cpp(&oracle, op, faults, &dir) {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("{name}: the C++ app {e}"));
                continue;
            }
        };
        let mut diffs = Vec::new();
        if let Some(i) = (0..rust.fake.log.len().max(cpp.fake.log.len())).find(|&i| rust.fake.log.get(i) != cpp.fake.log.get(i)) {
            let at = |log: &[String]| log.get(i).map_or("(end)".to_string(), |l| l.chars().take(80).collect());
            diffs.push(format!(
                "request {i} of {}/{}: C++ {:?}, Rust {:?}",
                cpp.fake.log.len(),
                rust.fake.log.len(),
                at(&cpp.fake.log),
                at(&rust.fake.log)
            ));
        }
        if rust.ok != cpp.ok {
            diffs.push(format!(
                "C++ {}, Rust {}",
                if cpp.ok { "succeeded" } else { "failed" },
                if rust.ok { "succeeded" } else { "failed" }
            ));
        }
        if rust.ok && rust.data != cpp.data {
            diffs.push(format!("read {} bytes (C++) and {} (Rust)", cpp.data.len(), rust.data.len()));
        }
        let (r, c) = (&rust.fake, &cpp.fake);
        if r.flash != c.flash
            || r.option_bytes != c.option_bytes
            || r.erased_pages != c.erased_pages
            || r.fus_commands != c.fus_commands
            || r.left != c.left
        {
            diffs.push("the device ends up different".into());
        }
        if !diffs.is_empty() {
            failures.push(format!("{name}: {}", diffs.join("; ")));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{} of {} scenarios differ:\n{}", failures.len(), all.len(), failures.join("\n"));
}
