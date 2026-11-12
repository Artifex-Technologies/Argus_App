//! What a capture file says about itself: the port of the C++ app's `FlipperFileFormat` and
//! `FlipperFiles::parseAuto` (backend/chymaera/exporters/flipperfileformat.cpp). The result is
//! stored as the capture's `meta` JSON, which the Sarina API hands out (`capture.list`), so it
//! must match the C++ output exactly: the same keys, values and types, compact, keys sorted.
//!
//! The file is read by argus-tools' `flipperfile::FlipperFormat`, the one Flipper-format reader of
//! the workspace, with Qt's rules (measured against Qt 6).

use serde_json::{json, Map, Value};

pub use argus_tools::flipperfile::{same_key, to_int, FlipperFormat};

/// Larger files are not parsed (they are kept as raw captures), in both apps since 2026-11-12.
pub const MAX_PARSE: usize = argus_tools::flipperfile::MAX_PARSE;

fn lower_suffix(name: &str) -> String {
    // QFileInfo::suffix: after the last dot of the file name
    let file = name.rsplit('/').next().unwrap_or(name);
    file.rsplit_once('.').map(|(_, s)| s.to_lowercase()).unwrap_or_default()
}

fn parse_nfc(f: &FlipperFormat) -> Value {
    const KNOWN: [&str; 6] = ["Filetype", "Version", "Device type", "UID", "ATQA", "SAK"];
    let mut extra = Map::new();
    for (k, v) in &f.pairs {
        if !KNOWN.iter().any(|known| same_key(k, known)) {
            extra.insert(k.clone(), Value::String(v.clone()));
        }
    }
    json!({
        "kind": "nfc",
        "filetype": f.filetype,
        "version": f.version,
        "deviceType": f.value("Device type"),
        "uid": f.value("UID"),
        "atqa": f.value("ATQA"),
        "sak": f.value("SAK"),
        "extra": extra,
    })
}

fn parse_subghz(f: &FlipperFormat) -> Value {
    // RAW_Data lines hold space-separated signed durations (microseconds).
    let mut count: i64 = 0;
    let mut total: i64 = 0;
    for chunk in f.values("RAW_Data") {
        for tok in chunk.split(' ').filter(|t| !t.is_empty()) {
            if let Some(v) = to_int(tok) {
                count += 1;
                total = total.saturating_add(i64::from(v).abs());
            }
        }
    }
    json!({
        "kind": "subghz",
        "filetype": f.filetype,
        "version": f.version,
        "frequency": f.value("Frequency"),
        "preset": f.value("Preset"),
        "protocol": f.value("Protocol"),
        "rawSampleCount": count,
        "rawDurationUs": total,
    })
}

/// `FlipperFiles::parseAuto`: `.nfc` and `.sub` get their own fields, any other Flipper-format
/// file its fields verbatim. `None` for files that are not Flipper-format.
pub fn parse_auto(file_name: &str, data: &[u8]) -> Option<Value> {
    let f = FlipperFormat::parse(data)?;
    Some(match lower_suffix(file_name).as_str() {
        "nfc" => parse_nfc(&f),
        "sub" => parse_subghz(&f),
        _ => {
            let mut fields = Map::new();
            for (k, v) in &f.pairs {
                fields.insert(k.clone(), Value::String(v.clone()));
            }
            json!({"kind": "generic", "filetype": f.filetype, "version": f.version, "fields": fields})
        }
    })
}

/// The kind of a file the parsers do not understand, by suffix (`rawKindForSuffix`).
pub fn raw_kind(file_name: &str) -> &'static str {
    match lower_suffix(file_name).as_str() {
        "pcap" | "pcapng" => "pcap",
        "csv" => "csv",
        "log" | "txt" => "log",
        _ => "file",
    }
}

/// The capture's kind and its `meta` JSON text, as `Bridge::recordCapture` stores them.
pub fn describe(file_name: &str, data: &[u8]) -> (String, String) {
    let info = parse_auto(file_name, data).unwrap_or_else(|| json!({"kind": raw_kind(file_name)}));
    let kind = info["kind"].as_str().unwrap_or("file").to_string();
    (kind, info.to_string())
}

/// The one-line summary the C++ app logs for a recorded capture.
pub fn summary(file_name: &str, size: u64, meta: &str) -> String {
    let info: Value = serde_json::from_str(meta).unwrap_or(Value::Null);
    let s = |k: &str| match &info[k] {
        Value::String(v) => v.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    match info["kind"].as_str().unwrap_or("") {
        "nfc" => format!("NFC {} UID {}", s("deviceType"), s("uid")),
        "subghz" => format!("Sub-GHz {} @ {} ({} samples)", s("protocol"), s("frequency"), s("rawSampleCount")),
        "generic" => format!("{} ({file_name})", s("filetype")),
        kind => format!("{kind} {file_name} ({size} bytes)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_raw_counts_and_line_endings() {
        let data = b"Filetype: Flipper SubGhz RAW File\r\nVersion: 1\nFrequency: 433920000\nPreset: FuriHalSubGhzPresetOok650Async\n# comment: no\nProtocol: RAW\nRAW_Data: 100 -200  x 300\nraw_data: -5\n";
        let v: Value = serde_json::from_str(&describe("a.SUB", data).1).unwrap();
        assert_eq!(v["kind"], "subghz");
        assert_eq!(v["frequency"], "433920000");
        assert_eq!(v["rawSampleCount"], 4);
        assert_eq!(v["rawDurationUs"], 605);
        assert_eq!(v["version"], 1);
    }

    #[test]
    fn nfc_extra_and_case() {
        let data =
            b"Filetype: Flipper NFC device\nversion: 4\nDevice type: NTAG215\nuid: 04 A2 26 BA\nATQA: 00 44\nSAK: 00\nPages total: 135\n";
        let (kind, meta) = describe("x.nfc", data);
        assert_eq!(kind, "nfc");
        assert_eq!(
            meta,
            r#"{"atqa":"00 44","deviceType":"NTAG215","extra":{"Pages total":"135"},"filetype":"Flipper NFC device","kind":"nfc","sak":"00","uid":"04 A2 26 BA","version":4}"#
        );
    }

    #[test]
    fn raw_kinds_and_non_flipper_files() {
        assert_eq!(describe("s.pcap", &[0xd4, 0xc3, 0xb2, 0xa1]).0, "pcap");
        assert_eq!(describe("log.TXT", b"no pairs here\n").0, "log");
        assert_eq!(describe("scan.csv", b"a,b\n1,2\n").0, "csv");
        assert_eq!(describe("noext", b"").0, "file");
        // a CSV with a colon in it is a Flipper-format file to the C++ parser, too
        assert_eq!(describe("t.csv", b"time: 12\n").0, "generic");
        assert_eq!(describe("bad.sub", b"\n\n#x: y\n").1, r#"{"kind":"file"}"#);
    }
}
