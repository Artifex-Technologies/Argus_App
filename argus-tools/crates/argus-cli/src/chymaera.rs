//! `argus-cli chymaera ...`: the port of `qFlipper-cli chymaera` (cli/chymaeracli.cpp) for the
//! commands that need no device: `serve`, `parse`, `import`, `selftest`. These print the C++ CLI's
//! text, not JSON, so scripts and Sarina's docs that call the C++ CLI work unchanged.

use std::path::Path;
use std::process::ExitCode;

use argus_api::bridge::Bridge;
use argus_api::device::Device;
use argus_api::eventlog::source_name;
use argus_api::server::{self, Conn, Server, DEFAULT_PORT};
use argus_api::{pcap, qjson};
use argus_capture::meta;
use argus_capture::store::{Capture, Severity, Source, Store};

const USAGE: &str = "Chymaera headless interface

Usage: argus-cli chymaera <command> [args]

Commands:
  selftest              Verify the Chymaera subsystem end-to-end (no device needed)
  serve [port] [token]  Start the Sarina control API (loopback). Default port 44700.
                        With --port DEVICE or --socket PATH before `chymaera`, that Flipper
                        is attached: device.status, capture.pull and capture.pullAll use it
  watch [seconds]       Stream live device events from the Flipper named with --port/--socket
  parse <file>          Parse a Flipper file (.nfc/.sub/...) and print JSON
  import <file>         Parse and record a capture in the datastore
";

pub fn run(cmd: &super::Cmd) -> ExitCode {
    let args = &cmd.words[1..];
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("selftest") => selftest(),
        Some("serve") => serve(cmd, rest),
        Some("watch") => watch(cmd, rest),
        Some("parse") => parse(rest),
        Some("import") => import(rest),
        other => {
            print!("{USAGE}");
            if other.is_none() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
    }
}

/// A file as `FlipperFiles::parseAuto` reads it: only a regular file, and not one over the parse
/// limit (the C++ app reads the same way since 2026-11-12).
fn read_capture(path: &str) -> Option<Vec<u8>> {
    let m = std::fs::metadata(path).ok()?;
    if !m.is_file() || m.len() > meta::MAX_PARSE as u64 {
        return None;
    }
    std::fs::read(path).ok()
}

fn parse(args: &[String]) -> ExitCode {
    let Some(path) = args.first() else {
        println!("Usage: argus-cli chymaera parse <file>");
        return ExitCode::from(1);
    };
    let name = path.rsplit('/').next().unwrap_or(path);
    match read_capture(path).and_then(|d| meta::parse_auto(name, &d)) {
        Some(v) => {
            print!("{}", String::from_utf8_lossy(&qjson::to_indented(&qjson::from_serde(&v))));
            ExitCode::SUCCESS
        }
        None => {
            println!("Could not parse: {path}");
            ExitCode::from(1)
        }
    }
}

fn import(args: &[String]) -> ExitCode {
    let Some(path) = args.first() else {
        println!("Usage: argus-cli chymaera import <file>");
        return ExitCode::from(1);
    };
    let Some(mut bridge) = Bridge::open_default() else {
        println!("Import failed: {path}");
        return ExitCode::from(1);
    };
    let summary = bridge.import_capture(path);
    if summary.is_empty() {
        println!("Import failed: {path}");
        return ExitCode::from(1);
    }
    println!("Imported: {summary}\nDatastore: {}", bridge.datastore_path());
    ExitCode::SUCCESS
}

/// Opens the Flipper named with --port/--socket, if any.
fn device(cmd: &super::Cmd, datastore: &str) -> Option<Result<Device, String>> {
    let target = cmd.target.as_ref()?;
    Some(
        super::open(target, cmd.timeout)
            .and_then(|io| Device::connect(io, cmd.timeout, std::path::PathBuf::from(datastore)))
            .map_err(|e| e.to_string()),
    )
}

fn watch(cmd: &super::Cmd, args: &[String]) -> ExitCode {
    let seconds = match args.first() {
        None => 0,
        Some(s) => match s.trim().parse::<u64>() {
            Ok(n) => n,
            Err(_) => {
                println!("Not a number of seconds: {s}");
                return ExitCode::from(1);
            }
        },
    };
    if cmd.target.is_none() {
        println!("Name the Flipper with --port DEVICE or --socket PATH (argus-cli never searches for one)");
        return ExitCode::from(1);
    }
    let Some(mut bridge) = Bridge::open_default() else {
        println!("No data folder (HOME or XDG_DATA_HOME) for the datastore");
        return ExitCode::from(1);
    };
    bridge.on_entry(|e| {
        println!("[{}] {}", source_name(e.source), String::from_utf16_lossy(&e.message));
    });
    let stop = server::stop_on_signals();
    let during = if seconds > 0 { format!(" for {seconds}s") } else { String::new() };
    println!("Watching for device events{during} (Ctrl+C to stop)…");
    match device(cmd, bridge.datastore_path()) {
        Some(Ok(d)) => bridge.attach_device(d),
        Some(Err(e)) => {
            println!("[device] Cannot open the device: {e}");
            return ExitCode::from(1);
        }
        None => unreachable!("checked above"),
    }
    let end = (seconds > 0).then(|| std::time::Instant::now() + std::time::Duration::from_secs(seconds));
    while !stop.load(std::sync::atomic::Ordering::Relaxed) && end.is_none_or(|t| std::time::Instant::now() < t) && bridge.has_device() {
        bridge.poll_device();
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    ExitCode::SUCCESS
}

fn serve(cmd: &super::Cmd, args: &[String]) -> ExitCode {
    let port = match args.first() {
        None => DEFAULT_PORT,
        Some(p) => match p.trim().parse::<u16>() {
            Ok(p) => p,
            Err(_) => {
                println!("Not a port: {p} (0 to 65535)");
                return ExitCode::from(1);
            }
        },
    };
    let token = args.get(1).cloned().unwrap_or_default();
    let Some(mut bridge) = Bridge::open_default() else {
        println!("No data folder (HOME or XDG_DATA_HOME) for the datastore");
        return ExitCode::from(1);
    };
    // the live event stream on stdout, as the C++ CLI
    bridge.on_entry(|e| {
        println!("[{}] {}", source_name(e.source), String::from_utf16_lossy(&e.message));
    });
    let stop = server::stop_on_signals();
    let server = match Server::start(&mut bridge, port, &token) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            println!("Failed to start the Sarina control API on port {port}");
            return ExitCode::from(1);
        }
    };
    let how = match server.token_file() {
        Some(f) => format!("token written to {}", f.display()),
        None => "token required".into(),
    };
    println!(
        "Sarina control API listening on 127.0.0.1:{} ({how})\nDatastore: {}\nPress Ctrl+C to stop.",
        server.port(),
        bridge.datastore_path()
    );
    match device(cmd, bridge.datastore_path()) {
        None => {}
        Some(Ok(d)) => bridge.attach_device(d),
        Some(Err(e)) => {
            println!("Cannot open the device: {e}");
            server.stop(&mut bridge);
            return ExitCode::from(1);
        }
    }
    server.run(&mut bridge, stop);
    server.stop(&mut bridge);
    ExitCode::SUCCESS
}

fn check(name: &str, ok: bool, detail: &str, failures: &mut u32) {
    print!("{}{name}", if ok { "  PASS  " } else { "  FAIL  " });
    if !ok && !detail.is_empty() {
        print!("  — {detail}");
    }
    println!();
    if !ok {
        *failures += 1;
    }
}

fn selftest() -> ExitCode {
    let mut failures = 0;
    println!("Chymaera self-test");
    let work = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            println!("  FAIL  temporary folder  — {e}");
            return ExitCode::from(1);
        }
    };

    // datastore
    match Store::open(&work.path().join("selftest.db")) {
        Ok(ds) => {
            check("datastore open", true, "", &mut failures);
            let sid = ds.start_session("selftest", "");
            check("datastore session", sid.is_ok(), &sid.as_ref().err().map(|e| e.to_string()).unwrap_or_default(), &mut failures);
            let _ = ds.log_event(Severity::Info, Source::System, "hello", "");
            check("datastore event", ds.event_count().unwrap_or(0) >= 1, "", &mut failures);
            let c = Capture {
                kind: "nfc".into(),
                name: "x.nfc".into(),
                source_path: "/tmp/x.nfc".into(),
                size: 10,
                meta: "{}".into(),
                sha256: String::new(),
                local_path: String::new(),
            };
            let _ = ds.add_capture(&c);
            check("datastore capture", !ds.capture_rows(10).unwrap_or_default().is_empty(), "", &mut failures);
        }
        Err(e) => check("datastore open", false, &e.to_string(), &mut failures),
    }

    // pcap / pcapng
    let ng = work.path().join("selftest.pcapng");
    let wrote = pcap::write_file(&ng, pcap::Format::PcapNg, pcap::LINK_USER0, &[(0, &[0xde, 0xad, 0xbe, 0xef][..]), (0, &[0; 5][..])]);
    let size = std::fs::metadata(&ng).map(|m| m.len()).unwrap_or(0);
    check("pcapng write", wrote.is_ok() && size > 60, "", &mut failures);
    let p = work.path().join("selftest.pcap");
    let wrote = pcap::write_file(&p, pcap::Format::Pcap, pcap::LINK_USER0, &[(0, &[1, 2, 3, 4, 5][..])]);
    let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
    check("pcap write", wrote.is_ok() && size >= 24 + 16 + 5, "", &mut failures);

    // Flipper file parsers
    let sub = meta::parse_auto(
        "x.sub",
        b"Filetype: Flipper SubGhz RAW File\nVersion: 1\nFrequency: 433920000\nPreset: FuriHalSubGhzPresetOok650Async\nProtocol: RAW\nRAW_Data: 100 -200 300 -400 500\n",
    )
    .unwrap_or_default();
    check("parse .sub frequency", sub["frequency"] == "433920000", "", &mut failures);
    check("parse .sub raw count", sub["rawSampleCount"] == 5, "", &mut failures);
    let nfc =
        meta::parse_auto("x.nfc", b"Filetype: Flipper NFC device\nVersion: 2\nDevice type: UID\nUID: 04 A2 26 BA\nATQA: 00 44\nSAK: 00\n")
            .unwrap_or_default();
    check("parse .nfc uid", nfc["uid"] == "04 A2 26 BA", "", &mut failures);

    // the control API, in process (no port is opened)
    let mut bridge = Bridge::new(&work.path().join("api.db"), work.path().join("captures"));
    let token = server::new_token().unwrap_or_default();
    let mut conn = Conn::default();
    let reply = conn.feed(&mut bridge, token.as_bytes(), format!("{{\"cmd\":\"ping\",\"token\":\"{token}\"}}\n").as_bytes());
    let text = String::from_utf8_lossy(&reply).into_owned();
    check("control API ping", text.starts_with("{\"ok\":true,\"pong\":true"), text.trim(), &mut failures);
    let reply = conn.feed(&mut bridge, token.as_bytes(), b"{\"cmd\":\"ping\",\"token\":\"wrong\"}\n");
    check("control API refuses a wrong token", reply == b"{\"error\":\"unauthorized\",\"ok\":false}\n", "", &mut failures);
    let runtime = work.path().join("runtime");
    let written = std::fs::create_dir(&runtime).and_then(|_| server::write_token(&runtime, &token));
    let private = written.as_ref().is_ok_and(|f| {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).map(|m| m.permissions().mode() & 0o777).unwrap_or(0);
        mode(f) == 0o600 && f.parent().is_some_and(|d| mode(d) == 0o700) && std::fs::read_to_string(f).is_ok_and(|t| t == token)
    });
    check("control token file is private (0700 folder, 0600 file)", private, "", &mut failures);

    // device names that could leave a folder
    let good = ["a", ".bt.settings", "snake game.fap", "café"].iter().all(|n| flipper_rpc::transfer::safe_name(n));
    let long = "a".repeat(256);
    let bad = ["", ".", "..", "a/b", "a\\b", "a\0b", "a\nb", "x\x1b[2J", "x\x7f", "x\u{9b}", &long]
        .iter()
        .all(|n| !flipper_rpc::transfer::safe_name(n));
    check("device names that could leave a folder are refused", good && bad, "", &mut failures);

    if failures == 0 {
        println!("\nSELF-TEST PASSED");
        ExitCode::SUCCESS
    } else {
        println!("\nSELF-TEST FAILED ({failures})");
        ExitCode::from(1)
    }
}
