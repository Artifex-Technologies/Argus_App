//! `flipper-fake (--socket PATH | --pty) [--sessions N] [--record FILE] [--requests FILE]
//!               [--fault NAME] [--legacy] [--no-sd]`
//!
//! Serves N connections (default 1) as a fake Flipper, one after the other, then writes every
//! byte the clients sent to `--record` and one line per RPC request (`session id Type arg`) to
//! `--requests`, and exits.
//!
//! - `--socket PATH`: listens on a Unix socket. Prints `listening PATH` once ready.
//! - `--pty`: makes a pseudo-terminal and prints `pty /dev/pts/N`; clients open that path as a
//!   serial port (the C++ app through its test hook `ARGUS_TEST_SERIAL_PORT`, argus-cli with
//!   `--port`). Like a real Flipper, the banner is printed when a client opens the port. A
//!   pseudo-terminal has no DTR line, so neither client raises it there.
//!
//! Faults: none, no-prompt, silent, garbage, oversize, busy, flood, wrong-type,
//! close-mid-frame, short-read, long-read, omit-empty-data, frame-without-data, short-frame, long-frame. `--hostile` adds
//! names to `/int` that would lead outside a local folder (both apps must refuse them);
//! `--captures` adds capture files to the capture folders (and a secret outside them).
//! `--screen-frames N` sets how many screen frames follow a stream start (default 1).

use std::io::{self, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::ExitCode;

use flipper_fake::{describe, pty, FakeFlipper, Fault, Transcript};

fn parse_fault(name: &str) -> Option<Fault> {
    Some(match name {
        "none" => Fault::None,
        "no-prompt" => Fault::NoPrompt,
        "silent" => Fault::Silent,
        "garbage" => Fault::GarbageFrame,
        "oversize" => Fault::OversizeFrame,
        "busy" => Fault::Status(4),
        "flood" => Fault::FloodOtherIds(2000),
        "wrong-type" => Fault::WrongType,
        "close-mid-frame" => Fault::CloseMidFrame,
        "short-read" => Fault::ShortRead,
        "long-read" => Fault::LongRead,
        "omit-empty-data" => Fault::OmitEmptyData,
        "frame-without-data" => Fault::FrameWithoutData,
        "short-frame" => Fault::ShortFrame,
        "long-frame" => Fault::LongFrame,
        _ => return None,
    })
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: flipper-fake (--socket PATH | --pty) [--sessions N] [--record FILE] [--requests FILE] [--fault NAME] [--legacy] [--no-sd] [--hostile] [--captures] [--screen-frames N]"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut socket: Option<PathBuf> = None;
    let mut pty = false;
    let mut sessions = 1usize;
    let mut record: Option<PathBuf> = None;
    let mut requests: Option<PathBuf> = None;
    let mut fake = FakeFlipper::standard();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--socket" => socket = args.next().map(PathBuf::from),
            "--pty" => pty = true,
            "--sessions" => match args.next().and_then(|n| n.parse().ok()).filter(|n| (1..=16).contains(n)) {
                Some(n) => sessions = n,
                None => return usage(),
            },
            "--record" => record = args.next().map(PathBuf::from),
            "--requests" => requests = args.next().map(PathBuf::from),
            "--fault" => match args.next().as_deref().and_then(parse_fault) {
                Some(f) => fake.fault = f,
                None => return usage(),
            },
            "--legacy" => fake.protobuf_version = (0, 13),
            "--no-sd" => fake.sd_card = false,
            "--hostile" => fake = fake.with_hostile_names(),
            "--captures" => fake = fake.with_captures(),
            "--screen-frames" => match args.next().and_then(|n| n.parse().ok()).filter(|n| (0..=1000).contains(n)) {
                Some(n) => fake.screen_frames = n,
                None => return usage(),
            },
            _ => return usage(),
        }
    }
    if pty == socket.is_some() {
        return usage();
    }

    let mut all: Vec<Transcript> = Vec::new();
    let result: io::Result<()> = if pty {
        let (transcripts, r) = pty::serve_pty(&fake, sessions, |name| {
            println!("pty {name}");
            let _ = io::stdout().flush();
        });
        all = transcripts;
        r
    } else {
        let socket = socket.expect("checked");
        match UnixListener::bind(&socket) {
            Err(e) => Err(e),
            Ok(listener) => {
                println!("listening {}", socket.display());
                let _ = io::stdout().flush();
                let r = (|| {
                    for _ in 0..sessions {
                        let (mut stream, _) = listener.accept()?;
                        all.push(fake.serve(&mut stream)?);
                    }
                    Ok(())
                })();
                let _ = std::fs::remove_file(&socket);
                r
            }
        }
    };

    if let Some(path) = record {
        let bytes: Vec<u8> = all.iter().flat_map(|t| t.received.iter().copied()).collect();
        if let Err(e) = std::fs::write(&path, bytes) {
            eprintln!("flipper-fake: cannot write {}: {e}", path.display());
            return ExitCode::from(1);
        }
    }
    if let Some(path) = requests {
        let mut text = String::new();
        for (i, t) in all.iter().enumerate() {
            for m in &t.requests {
                text.push_str(&format!("{} {}\n", i + 1, describe(m)));
            }
        }
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("flipper-fake: cannot write {}: {e}", path.display());
            return ExitCode::from(1);
        }
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("flipper-fake: {e}");
            ExitCode::from(1)
        }
    }
}
