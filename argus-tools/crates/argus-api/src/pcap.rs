//! Packet captures Wireshark opens: the port of `PcapExporter` (backend/chymaera/exporters).
//! Classic pcap (microsecond magic, version 2.4) or pcapng (a section header, one interface, an
//! enhanced packet block per packet), little-endian, packets cut at a snap length of 262144 bytes.
//! `tests/pcap.rs` compares the bytes with the C++ exporter's.

use std::io::Write;
use std::path::Path;

pub const SNAPLEN: u32 = 262_144;
/// `LINKTYPE_USER0`, for payloads without a standard link type.
pub const LINK_USER0: u16 = 147;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Pcap,
    PcapNg,
}

fn u16le(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn u32le(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// The file header (`writePcapGlobalHeader` / `writePcapNgHeaders`).
pub fn header(format: Format, link: u16) -> Vec<u8> {
    let mut out = Vec::new();
    match format {
        Format::Pcap => {
            u32le(&mut out, 0xA1B2_C3D4);
            u16le(&mut out, 2);
            u16le(&mut out, 4);
            u32le(&mut out, 0);
            u32le(&mut out, 0);
            u32le(&mut out, SNAPLEN);
            u32le(&mut out, u32::from(link));
        }
        Format::PcapNg => {
            u32le(&mut out, 0x0A0D_0D0A);
            u32le(&mut out, 28);
            u32le(&mut out, 0x1A2B_3C4D);
            u16le(&mut out, 1);
            u16le(&mut out, 0);
            out.extend_from_slice(&u64::MAX.to_le_bytes());
            u32le(&mut out, 28);
            u32le(&mut out, 1);
            u32le(&mut out, 20);
            u16le(&mut out, link);
            u16le(&mut out, 0);
            u32le(&mut out, SNAPLEN);
            u32le(&mut out, 20);
        }
    }
    out
}

/// One packet record (`writePacket`); `epoch_ms` is its time in milliseconds since 1970.
pub fn packet(format: Format, epoch_ms: i64, data: &[u8]) -> Vec<u8> {
    let captured = &data[..data.len().min(SNAPLEN as usize)];
    let orig_len = data.len() as u32;
    let cap_len = captured.len() as u32;
    // the C++ arithmetic: truncating division, then unsigned
    let sec = (epoch_ms / 1000) as u64;
    let usec = ((epoch_ms % 1000) * 1000) as u64;
    let mut out = Vec::new();
    match format {
        Format::Pcap => {
            u32le(&mut out, sec as u32);
            u32le(&mut out, usec as u32);
            u32le(&mut out, cap_len);
            u32le(&mut out, orig_len);
            out.extend_from_slice(captured);
        }
        Format::PcapNg => {
            let pad = (4 - cap_len % 4) % 4;
            let block_len = 32 + cap_len + pad;
            let ts = sec.wrapping_mul(1_000_000).wrapping_add(usec);
            u32le(&mut out, 6);
            u32le(&mut out, block_len);
            u32le(&mut out, 0);
            u32le(&mut out, (ts >> 32) as u32);
            u32le(&mut out, ts as u32);
            u32le(&mut out, cap_len);
            u32le(&mut out, orig_len);
            out.extend_from_slice(captured);
            out.resize(out.len() + pad as usize, 0);
            u32le(&mut out, block_len);
        }
    }
    out
}

/// `PcapExporter::writeFile`: a new file (an existing one is replaced, as the C++ app does).
pub fn write_file(path: &Path, format: Format, link: u16, packets: &[(i64, &[u8])]) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(&header(format, link))?;
    for (ms, p) in packets {
        f.write_all(&packet(format, *ms, p))?;
    }
    f.flush()
}
