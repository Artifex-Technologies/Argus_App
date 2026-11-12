//! The harness that drives the C++ oracle (tests/qt-oracle) against the fake: every USB request the
//! C++ code makes arrives as a line and is answered from a fresh `FakeDfu`, as the Rust code's are.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use super::fake::{self, FakeDfu, Faults};

/// The most requests one C++ run may send before it is taken as looping for ever.
pub const MAX_REQUESTS: usize = 400_000;

pub struct Outcome {
    pub ok: bool,
    pub data: Vec<u8>,
    pub fake: FakeDfu,
}

pub fn unhex(s: &str) -> Vec<u8> {
    fake::unhex(s).expect("hex from the oracle")
}

/// Runs the oracle with `args` (`device OP ...`), its USB requests served by a fresh fake. Err: it
/// did not finish.
pub fn run_cpp(oracle: &Path, args: &[String], faults: &Faults) -> Result<Outcome, String> {
    let mut child = Command::new(oracle).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
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
        if let Some(rest) = line.strip_prefix("RESULT ") {
            let (ok, data) = rest.split_once(' ').unwrap_or((rest, "-"));
            result = Ok((ok == "ok", unhex(data)));
            break;
        }
        // a SLEEP line is what the device asked for, as the C++ app would wait it (an unsigned long)
        let answer = fake::answer(&mut f, &line);
        if answer.is_none() && !line.starts_with("SLEEP ") {
            result = Err(format!("unexpected line {line:?}"));
            break;
        }
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

/// What differs between two runs, if anything.
pub fn differences(rust: &Outcome, cpp: &Outcome) -> Vec<String> {
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
        diffs.push(format!("C++ {}, Rust {}", if cpp.ok { "succeeded" } else { "failed" }, if rust.ok { "succeeded" } else { "failed" }));
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
    if r.fus != c.fus || r.otp != c.otp {
        diffs.push("the device ends up different".into());
    }
    diffs
}
