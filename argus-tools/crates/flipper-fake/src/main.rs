//! `flipper-fake --socket PATH [--record FILE] [--fault NAME] [--legacy] [--no-sd]`
//!
//! Listens on a Unix socket, serves one client as a fake Flipper, writes every byte the client
//! sent to FILE (if given) and exits. Prints `listening PATH` on stdout once ready.
//! Faults: none, no-prompt, silent, garbage, oversize, busy, flood, wrong-type, close-mid-frame.

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::ExitCode;

use flipper_fake::{FakeFlipper, Fault};

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
        _ => return None,
    })
}

fn main() -> ExitCode {
    let mut socket: Option<PathBuf> = None;
    let mut record: Option<PathBuf> = None;
    let mut fake = FakeFlipper::standard();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--socket" => socket = args.next().map(PathBuf::from),
            "--record" => record = args.next().map(PathBuf::from),
            "--fault" => match args.next().as_deref().and_then(parse_fault) {
                Some(f) => fake.fault = f,
                None => {
                    eprintln!("flipper-fake: unknown fault");
                    return ExitCode::from(2);
                }
            },
            "--legacy" => fake.protobuf_version = (0, 13),
            "--no-sd" => fake.sd_card = false,
            _ => {
                eprintln!("usage: flipper-fake --socket PATH [--record FILE] [--fault NAME] [--legacy] [--no-sd]");
                return ExitCode::from(2);
            }
        }
    }
    let Some(socket) = socket else {
        eprintln!("flipper-fake: --socket PATH is required");
        return ExitCode::from(2);
    };
    let listener = match UnixListener::bind(&socket) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("flipper-fake: cannot listen on {}: {e}", socket.display());
            return ExitCode::from(1);
        }
    };
    println!("listening {}", socket.display());
    let result = listener.accept().and_then(|(mut stream, _)| fake.serve(&mut stream));
    let _ = std::fs::remove_file(&socket);
    match result {
        Ok(t) => {
            if let Some(path) = record {
                if let Err(e) = std::fs::write(&path, &t.received) {
                    eprintln!("flipper-fake: cannot write {}: {e}", path.display());
                    return ExitCode::from(1);
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("flipper-fake: {e}");
            ExitCode::from(1)
        }
    }
}
