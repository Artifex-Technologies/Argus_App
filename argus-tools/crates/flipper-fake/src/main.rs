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
//!
//! Writes change the fake's storage, which lasts across the sessions of one run; `--dump FILE`
//! writes it at the end (`d PATH` / `f PATH SIZE MD5`, sorted). `--changed-int` starts with
//! `/int` changed since a backup (for restores), `--int-conflict` with `/int/sub` a file;
//! `--read-only` refuses every change with "access denied" (a write-protected or failing card).
//! A reboot or factory reset request ends the session, as the device leaves USB.
//!
//! `flipper-fake --recovery DIR [--fus running|idle|stuck|never-settles] [--no-sd] [--old-resources] [--old-radio] [--start-firmware] [--fail-write-at HEX]` is one Flipper across
//! its lives, for comparing the full repair (`compare-repair.sh`): the fake bootloader (with factory
//! information in its OTP) on the Unix socket `DIR/dfu.sock`, gone after every reset and back while the
//! chip boots into it; once the chip boots its firmware, the fake Flipper on a pseudo-terminal (printed as
//! `pty /dev/pts/N` at the start). `DIR/state` says which is plugged in (`dfu N`, `serial N`, `none`; N counts the chip's lives), for the
//! C++ app's test hook `ARGUS_TEST_DEVICE_STATE`. It runs until its standard input closes, then writes
//! `DIR/dfu.log` (every bootloader request, `SESSION n` where each connection began), `DIR/flash.bin`,
//! `DIR/chip.txt` (option bytes, OTP, FUS state, resets), `DIR/requests.txt` (the RPC requests) and
//! `DIR/dump.txt` (the storage). `--fus`: the wireless stack running (default), the FUS running, or a FUS
//! stuck in a state the repair does not know, or one that never finishes deleting the wireless stack; `--fail-write-at HEX`: the data block written over that
//! address fails (errPROG); `--old-resources`: the card holds an older install of the resources with a valid
//! `/ext/Manifest` (src/firmware.rs); `--start-firmware`: the chip starts as the firmware on the
//! pseudo-terminal and turns into its bootloader when asked to reboot into DFU (the legacy update);
//! `--old-radio`: devinfo reports wireless stack 1.16.3, older than the update's 1.17.3.
//! `--old-region-data`: `/int` holds an older `/int/.region_data`.
//! `--region N`: the device's built-in region (devinfo and the OTP's factory information; default 0, the
//! development region, which needs no region data).
//! `flipper-fake --write-firmware DIR [--resources-mismatch | --core2-mismatch]` writes a made-up update folder
//! (`directory.json`, its files and the region `bundle`) for the C++ app's test hook `ARGUS_TEST_FIRMWARE_DIR`
//! (src/firmware.rs) and exits; the options damage the resources bundle or the wireless stack archive inside
//! (their SHA-256 in `directory.json` stays right).
//! `flipper-fake --update-server DIR --ca-out FILE` serves such a folder over HTTPS on 127.0.0.1 (src/update_server.rs)
//! with a test CA made at start and written to FILE, prints `listening https://127.0.0.1:PORT`, serves until its standard
//! input closes, then prints the requests it saw.
//!
//! `flipper-fake --write-archives DIR` writes the restore test archives (`good-*.tgz` that
//! must restore, `hostile-*.tgz` that must be refused, `bomb.tgz`) and `hostile.txt` (name and
//! reason), `update.tgz` and `update-bad-manifest.tgz` (update packages) into DIR, which must not
//! exist, and exits. `--print-dump` prints the dump of the
//! starting storage (after the other options) and exits.

use std::io::{self, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::ExitCode;

use flipper_fake::{describe, dfu, pty, FakeFlipper, Fault, Transcript};

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
        "usage: flipper-fake (--socket PATH | --pty) [--sessions N] [--record FILE] [--requests FILE] [--dump FILE] [--fault NAME] [--legacy] [--no-sd] [--hostile] [--captures] [--changed-int] [--int-conflict] [--read-only] [--screen-frames N]\n       flipper-fake --write-archives DIR"
    );
    ExitCode::from(2)
}

/// `--update-server DIR --ca-out FILE`: the HTTPS stand-in for Flipper's update server (src/update_server.rs)
/// on 127.0.0.1, its test CA written to FILE (which must not exist); prints `listening https://127.0.0.1:PORT`
/// and serves until its standard input closes, then prints the requests it saw (`GET PATH`).
fn serve_updates(dir: &std::path::Path, ca_out: &std::path::Path) -> ExitCode {
    use flipper_fake::update_server::{TestCa, UpdateServer};
    let ca = TestCa::new();
    let written =
        std::fs::OpenOptions::new().write(true).create_new(true).open(ca_out).and_then(|mut f| f.write_all(ca.ca_pem().as_bytes()));
    if let Err(e) = written {
        eprintln!("flipper-fake: {}: {e}", ca_out.display());
        return ExitCode::from(1);
    }
    let server = match UpdateServer::start(dir, &ca) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("flipper-fake: {e}");
            return ExitCode::from(1);
        }
    };
    println!("listening {}", server.base());
    let _ = io::stdout().flush();
    let _ = io::copy(&mut io::stdin(), &mut io::sink());
    for r in server.requests.lock().expect("log").iter() {
        println!("{r}");
    }
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let mut socket: Option<PathBuf> = None;
    let mut pty = false;
    let mut sessions = 1usize;
    let mut record: Option<PathBuf> = None;
    let mut requests: Option<PathBuf> = None;
    let mut dump: Option<PathBuf> = None;
    let mut print_dump = false;
    let mut fake = FakeFlipper::standard();
    let mut recovery: Option<PathBuf> = None;
    let mut fus = [0xFF, 0xFE];
    let mut fail_write_at: Option<u32> = None;
    let mut fus_never_settles = false;
    let mut start_firmware = false;
    let mut region = 0u8;
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
            "--dump" => dump = args.next().map(PathBuf::from),
            "--print-dump" => print_dump = true,
            "--changed-int" => fake = fake.with_changed_int(),
            "--int-conflict" => fake = fake.with_int_conflict(),
            "--old-region-data" => fake = fake.with_old_region_data(),
            "--read-only" => fake.read_only = true,
            "--write-firmware" => {
                let dir = args.next();
                let (resources, core2) = match args.next().as_deref() {
                    None => (false, false),
                    Some("--resources-mismatch") => (true, false),
                    Some("--core2-mismatch") => (false, true),
                    Some(_) => return usage(),
                };
                return match dir {
                    Some(dir) => match flipper_fake::firmware::write_with(&PathBuf::from(&dir), resources, core2) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(e) => {
                            eprintln!("flipper-fake: {dir}: {e}");
                            ExitCode::from(1)
                        }
                    },
                    None => usage(),
                };
            }
            "--recovery" => recovery = args.next().map(PathBuf::from),
            "--region" => match args.next().and_then(|n| n.parse::<u8>().ok()) {
                Some(n) => {
                    region = n;
                    for (k, v) in fake.devinfo.iter_mut() {
                        if k == "hardware.region.builtin" {
                            *v = n.to_string();
                        }
                    }
                }
                None => return usage(),
            },
            "--update-server" => {
                let (Some(dir), Some("--ca-out"), Some(ca)) = (args.next(), args.next().as_deref(), args.next()) else {
                    return usage();
                };
                return serve_updates(&PathBuf::from(dir), &PathBuf::from(ca));
            }
            "--fus" => match args.next().as_deref() {
                Some("running") => fus = [0xFF, 0xFE],
                Some("idle") => fus = [0, 0],
                Some("stuck") => fus = [0x30, 0x02],
                Some("never-settles") => {
                    fus = [0xFF, 0xFE];
                    fus_never_settles = true;
                }
                _ => return usage(),
            },
            "--fail-write-at" => match args.next().and_then(|a| u32::from_str_radix(a.trim_start_matches("0x"), 16).ok()) {
                Some(a) => fail_write_at = Some(a),
                None => return usage(),
            },
            "--write-archives" => {
                return match args.next() {
                    Some(dir) => write_archives(&PathBuf::from(dir)),
                    None => usage(),
                }
            }
            "--fault" => match args.next().as_deref().and_then(parse_fault) {
                Some(f) => fake.fault = f,
                None => return usage(),
            },
            "--legacy" => fake.protobuf_version = (0, 13),
            "--no-sd" => fake.sd_card = false,
            "--old-resources" => fake = flipper_fake::firmware::with_old_resources(fake),
            "--start-firmware" => start_firmware = true,
            "--old-radio" => {
                for (k, v) in fake.devinfo.iter_mut() {
                    if k == "radio.stack.minor" {
                        *v = "16".into();
                    }
                }
            }
            "--hostile" => fake = fake.with_hostile_names(),
            "--captures" => fake = fake.with_captures(),
            "--screen-frames" => match args.next().and_then(|n| n.parse().ok()).filter(|n| (0..=1000).contains(n)) {
                Some(n) => fake.screen_frames = n,
                None => return usage(),
            },
            _ => return usage(),
        }
    }
    if print_dump {
        print!("{}", fake.dump());
        return ExitCode::SUCCESS;
    }
    if let Some(dir) = recovery {
        if pty || socket.is_some() {
            return usage();
        }
        let faults = dfu::Faults {
            fus_model: true,
            fus: Some(fus),
            otp: Some(dfu::factory_otp(region)),
            fail_write_at,
            fus_never_settles,
            ..dfu::Faults::default()
        };
        return match serve_recovery(&dir, faults, fake, start_firmware) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("flipper-fake: {}: {e}", dir.display());
                ExitCode::from(1)
            }
        };
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
    if let Some(path) = dump {
        if let Err(e) = std::fs::write(&path, fake.dump()) {
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

fn write_archives(dir: &std::path::Path) -> ExitCode {
    use flipper_fake::archives;
    let r = (|| -> io::Result<()> {
        std::fs::create_dir(dir)?;
        for (name, bytes) in archives::good() {
            std::fs::write(dir.join(format!("good-{name}.tgz")), bytes)?;
        }
        let mut list = String::new();
        for (name, why, bytes) in archives::hostile() {
            std::fs::write(dir.join(format!("hostile-{name}.tgz")), bytes)?;
            list.push_str(&format!("{name}\t{why}\n"));
        }
        std::fs::write(dir.join("hostile.txt"), list)?;
        std::fs::write(dir.join("update.tgz"), archives::update_package(true))?;
        std::fs::write(dir.join("update-bad-manifest.tgz"), archives::update_package(false))?;
        std::fs::write(dir.join("bomb.tgz"), archives::bomb())
    })();
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("flipper-fake: {}: {e}", dir.display());
            ExitCode::from(1)
        }
    }
}

/// Waits until a client opens the pseudo-terminal, or `stop` is set (false then).
fn wait_for_client_or(port: &serialport::TTYPort, stop: &std::sync::atomic::AtomicBool) -> bool {
    use nix::poll::{poll, PollFd, PollFlags};
    use std::os::fd::AsRawFd;
    use std::sync::atomic::Ordering;
    while !stop.load(Ordering::SeqCst) {
        let mut fds = [PollFd::new(port.as_raw_fd(), PollFlags::POLLIN)];
        match poll(&mut fds, 50) {
            Ok(_) if fds[0].revents().is_some_and(|r| r.contains(PollFlags::POLLHUP)) => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            Ok(_) => return true,
            Err(_) => return false,
        }
    }
    false
}

/// `--recovery DIR` (see the top of this file).
fn serve_recovery(dir: &std::path::Path, faults: dfu::Faults, fake: FakeFlipper, start_firmware: bool) -> io::Result<()> {
    use serialport::SerialPort;
    use std::io::Read;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    let state = dir.join("state");
    // the chip's lives are counted, so a reader that misses a short absence still sees a new device
    let lives = Arc::new(AtomicUsize::new(0));
    let state_lock = Arc::new(Mutex::new(()));
    let write_state = {
        let (dir, state, lives, lock) = (dir.to_path_buf(), state.clone(), lives.clone(), state_lock.clone());
        move |kind: &str| {
            let _guard = lock.lock();
            let text = if kind == "none" { "none".to_string() } else { format!("{kind} {}", lives.fetch_add(1, Ordering::SeqCst) + 1) };
            let tmp = dir.join("state.tmp");
            let _ = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &state));
        }
    };
    write_state(if start_firmware { "serial" } else { "none" });

    let (mut master, slave) = serialport::TTYPort::pair().map_err(io::Error::from)?;
    let name = slave.name().unwrap_or_default();
    drop(slave);

    let stop = Arc::new(AtomicBool::new(false));
    let booted = Arc::new(AtomicBool::new(start_firmware));
    // the bootloader appears: at the start, or when the firmware reboots into it
    let enter = Arc::new(AtomicBool::new(!start_firmware));

    // the firmware side: sessions on the pseudo-terminal once the chip has booted its firmware
    let (stop2, booted2, enter2, write_state2) = (stop.clone(), booted.clone(), enter.clone(), write_state.clone());
    let rpc = std::thread::spawn(move || {
        let mut all: Vec<Transcript> = Vec::new();
        for _ in 0..16 {
            while !booted2.load(Ordering::SeqCst) && !stop2.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
            if !wait_for_client_or(&master, &stop2) {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
            match fake.serve(&mut pty::PtyMaster { port: &mut master }) {
                Ok(t) => {
                    let left = t.left;
                    // a reboot into DFU: the firmware leaves and the chip comes back as its bootloader
                    let to_dfu = left
                        && matches!(t.requests.last().and_then(|m| m.content.as_ref()),
                            Some(flipper_rpc::proto::pb::main::Content::SystemRebootRequest(r)) if r.mode == 1);
                    all.push(t);
                    if to_dfu {
                        booted2.store(false, Ordering::SeqCst);
                        write_state2("none");
                        enter2.store(true, Ordering::SeqCst);
                    }
                    if left {
                        pty::wait_for_close(&master);
                    }
                }
                Err(_) => break,
            }
        }
        (fake, all)
    });

    let sock = dir.join("dfu.sock");
    let (stop3, booted3, sock2, enter3) = (stop.clone(), booted.clone(), sock.clone(), enter.clone());
    let chip = std::thread::spawn(move || {
        dfu::serve_lives(dfu::FakeDfu::new(faults), &sock2, Duration::from_millis(100), &stop3, &enter3, &mut |life| match life {
            dfu::Life::Bootloader => write_state("dfu"),
            dfu::Life::Gone => write_state("none"),
            dfu::Life::Firmware => {
                booted3.store(true, Ordering::SeqCst);
                write_state("serial");
            }
        })
    });
    println!("pty {name}");
    println!("listening {}", sock.display());
    let _ = io::stdout().flush();

    // until the harness closes our standard input
    let mut sink = Vec::new();
    let _ = io::stdin().take(1 << 20).read_to_end(&mut sink);
    stop.store(true, Ordering::SeqCst);

    let f = chip.join().map_err(|_| io::Error::other("the bootloader thread panicked"))?;
    let (fake, transcripts) = rpc.join().map_err(|_| io::Error::other("the firmware thread panicked"))?;

    std::fs::write(dir.join("dfu.log"), dfu::log_with_sessions(&f).join("\n") + "\n")?;
    std::fs::write(dir.join("flash.bin"), &f.flash)?;
    let ws = &f.sram[0x100 + 20..0x100 + 24];
    let chip_text = format!(
        "option_bytes {}\notp {}\nfus {}\nfus_commands {}\nwireless_stack {}.{}.{}\nerased_pages {}\nresets {}\nbooted_firmware {}\n",
        dfu::hex(&f.option_bytes),
        dfu::hex(&f.otp),
        dfu::hex(&f.fus),
        dfu::hex(&f.fus_commands),
        ws[3],
        ws[2],
        ws[1],
        f.erased_pages.len(),
        f.resets,
        f.booted_firmware
    );
    std::fs::write(dir.join("chip.txt"), chip_text)?;
    let mut text = String::new();
    for (i, t) in transcripts.iter().enumerate() {
        for m in &t.requests {
            text.push_str(&format!("{} {}\n", i + 1, describe(m)));
        }
    }
    std::fs::write(dir.join("requests.txt"), text)?;
    std::fs::write(dir.join("dump.txt"), fake.dump())?;
    let _ = std::fs::remove_file(&state);
    Ok(())
}
