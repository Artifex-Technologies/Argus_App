//! The Rust pcap/pcapng writer against the C++ app's `PcapExporter`, run as the oracle built by
//! tests/qt-oracle/build.sh (`ARGUS_QT_ORACLES=OUTDIR cargo test -p argus-api --test pcap`).
//! Without the variable the byte-layout checks still run; the comparison says it was skipped.

use std::io::Write;
use std::process::{Command, Stdio};

use argus_api::pcap::{self, Format};

fn packets() -> Vec<(i64, Vec<u8>)> {
    let mut out = vec![
        (0, vec![]),
        (1_789_045_402_145, vec![0xde, 0xad, 0xbe, 0xef]),
        (999, vec![1, 2, 3]),
        (1_000, vec![0; 5]),
        (-1_500, vec![7]),
        (4_102_444_800_000, vec![0x55; 6]),
    ];
    let mut x: u32 = 0x1234_5678;
    for i in 0..40 {
        let len = [0, 1, 2, 3, 4, 63, 64, 65, 1500, 262_143, 262_144, 262_145, 300_000][i % 13];
        let data = (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        out.push((i as i64 * 7_919_123, data));
    }
    out
}

fn rust_bytes(format: Format, link: u16) -> Vec<u8> {
    let mut v = pcap::header(format, link);
    for (ms, p) in packets() {
        v.extend_from_slice(&pcap::packet(format, ms, &p));
    }
    v
}

#[test]
fn layout() {
    let ng = pcap::packet(Format::PcapNg, 1_000, &[1, 2, 3, 4, 5]);
    assert_eq!(ng.len(), 32 + 8);
    assert_eq!(&ng[..4], &6u32.to_le_bytes());
    assert_eq!(&ng[ng.len() - 4..], &40u32.to_le_bytes());
    let big = pcap::packet(Format::Pcap, 0, &vec![0; 300_000]);
    assert_eq!(big.len(), 16 + pcap::SNAPLEN as usize);
    assert_eq!(pcap::header(Format::Pcap, 147).len(), 24);
    assert_eq!(pcap::header(Format::PcapNg, 147).len(), 48);
}

#[test]
fn same_bytes_as_the_cpp_exporter() {
    let Some(dir) = std::env::var_os("ARGUS_QT_ORACLES") else {
        eprintln!("ARGUS_QT_ORACLES is not set: skipped (build the oracles with tests/qt-oracle/build.sh)");
        return;
    };
    let work = std::env::temp_dir().join(format!("argus-pcap-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let mut input = String::new();
    for (ms, p) in packets() {
        let hex: String = if p.is_empty() { "-".into() } else { p.iter().map(|b| format!("{b:02x}")).collect() };
        input.push_str(&format!("{ms} {hex}\n"));
    }
    for (format, name) in [(Format::Pcap, "pcap"), (Format::PcapNg, "pcapng")] {
        for link in [0u16, 1, 147, 251, 272] {
            let out = work.join(format!("x.{name}"));
            let mut child = Command::new(std::path::Path::new(&dir).join("pcap-oracle"))
                .args([out.to_str().unwrap(), name, &link.to_string()])
                .stdin(Stdio::piped())
                .spawn()
                .expect("run the oracle");
            child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
            assert!(child.wait().unwrap().success());
            let cpp = std::fs::read(&out).unwrap();
            assert_eq!(cpp, rust_bytes(format, link), "{name} link {link}");
        }
    }
    std::fs::remove_dir_all(&work).unwrap();
}
