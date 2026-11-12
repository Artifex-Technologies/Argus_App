//! What a capture file says about itself: the port of the C++ app's `FlipperFileFormat` and
//! `FlipperFiles::parseAuto` (backend/chymaera/exporters/flipperfileformat.cpp). The result is
//! stored as the capture's `meta` JSON, which the Sarina API hands out (`capture.list`), so it
//! must match the C++ output exactly: the same keys, values and types, compact, keys sorted.
//!
//! Qt's rules are kept, as measured against Qt 6 (`qt_text`): the file is read in
//! `QIODevice::Text` mode, which drops every `\r` byte; a leading BOM is dropped; each byte that
//! is not part of a valid UTF-8 sequence becomes one U+FFFD; lines end at `\n`; keys and values
//! are trimmed of Unicode whitespace; `#` starts a comment; keys compare case-insensitively;
//! numbers parse like `QString::toInt` (0 when they do not).

use serde_json::{json, Map, Value};

/// Larger files are not parsed (they are kept as raw captures). The C++ app had no limit.
pub const MAX_PARSE: usize = 64 << 20;

/// One parsed Flipper-format file: its `Key: Value` pairs in file order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FlipperFormat {
    pub pairs: Vec<(String, String)>,
    pub filetype: String,
    pub version: i32,
}

/// What `QFile::open(ReadOnly | Text)` + `QString::fromUtf8` make of a file's bytes: every `\r`
/// removed, a leading BOM dropped, and one U+FFFD for each byte that does not start a valid UTF-8
/// sequence (Rust's lossy decoding uses one per maximal invalid sequence instead).
pub fn qt_text(data: &[u8]) -> String {
    let bytes: Vec<u8> = data.iter().copied().filter(|&b| b != b'\r').collect();
    let mut rest: &[u8] = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    let mut out = String::with_capacity(rest.len());
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.push_str(s);
                return out;
            }
            Err(e) => {
                let (good, bad) = rest.split_at(e.valid_up_to());
                // valid_up_to marks a valid prefix
                out.push_str(std::str::from_utf8(good).unwrap_or_default());
                out.push('\u{FFFD}');
                rest = &bad[1..];
            }
        }
    }
}

/// QString::trimmed: QChar::isSpace is the Unicode White_Space set, as Rust's.
fn trimmed(s: &str) -> &str {
    s.trim_matches(char::is_whitespace)
}

/// QString::compare(..., Qt::CaseInsensitive), close enough for keys.
fn same_key(a: &str, b: &str) -> bool {
    a.chars().flat_map(char::to_lowercase).eq(b.chars().flat_map(char::to_lowercase))
}

/// QString::toInt, as measured on Qt 6: only the part before the first NUL counts (Qt converts
/// through a C string), ASCII whitespace around it (space, \t, \n, \v, \f, \r) is ignored but no
/// other kind, base 10 with an optional sign; `None` when it fails or overflows.
fn to_int(s: &str) -> Option<i32> {
    let s = s.split('\0').next().unwrap_or_default();
    s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r')).parse::<i32>().ok()
}

impl FlipperFormat {
    /// `None` when the file holds no `Key: Value` line (not a Flipper file), as the C++ app.
    pub fn parse(data: &[u8]) -> Option<FlipperFormat> {
        if data.len() > MAX_PARSE {
            return None;
        }
        let text = qt_text(data);
        let mut out = FlipperFormat::default();
        let mut rest: &str = &text;
        loop {
            // No \r is left (qt_text), so lines end at \n only.
            let (line, next) = match rest.find('\n') {
                Some(i) => (&rest[..i], Some(&rest[i + 1..])),
                None => (rest, None),
            };
            let line = trimmed(line);
            if !line.is_empty() && !line.starts_with('#') {
                if let Some(sep) = line.find(':') {
                    let key = trimmed(&line[..sep]);
                    let value = trimmed(&line[sep + 1..]);
                    if !key.is_empty() {
                        if same_key(key, "Filetype") {
                            out.filetype = value.to_string();
                        } else if same_key(key, "Version") {
                            out.version = to_int(value).unwrap_or(0);
                        }
                        out.pairs.push((key.to_string(), value.to_string()));
                    }
                }
            }
            match next {
                Some(n) => rest = n,
                None => break,
            }
        }
        if out.pairs.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    /// The first value under `key` (case-insensitive), or "".
    pub fn value(&self, key: &str) -> &str {
        self.pairs.iter().find(|(k, _)| same_key(k, key)).map(|(_, v)| v.as_str()).unwrap_or("")
    }

    /// Every value under `key`, in file order.
    pub fn values<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.pairs.iter().filter(move |(k, _)| same_key(k, key)).map(|(_, v)| v.as_str())
    }
}

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

    #[test]
    fn decoding_matches_qt() {
        // measured with QFile(Text) + QString::fromUtf8 on Qt 6
        let cases: [(&[u8], &str); 8] = [
            (b"\xE0\x0A\x41", "\u{FFFD}\nA"),
            (b"\xE1\x80\x0A", "\u{FFFD}\u{FFFD}\n"),
            (b"\xF0\x90\x0A", "\u{FFFD}\u{FFFD}\n"),
            (b"\xED\xA0\x80\x0A", "\u{FFFD}\u{FFFD}\u{FFFD}\n"),
            (b"\xE1\x80\xE1\x80\x80", "\u{FFFD}\u{FFFD}\u{1000}"),
            (b"\xEF\xBB\xBFA", "A"),
            (b"A\xEF\xBB\xBF", "A\u{FEFF}"),
            (b"a\rb\r\nc\n\rd\r\re", "ab\nc\nde"),
        ];
        for (bytes, want) in cases {
            assert_eq!(qt_text(bytes), want, "{bytes:02x?}");
        }
    }

    #[test]
    fn version_parses_like_qt() {
        for (text, want) in
            [("Version: +7", 7), ("Version: 3x", 0), ("Version: 99999999999", 0), ("Version: -2", -2), ("Version: -2\0x", -2)]
        {
            assert_eq!(FlipperFormat::parse(text.as_bytes()).unwrap().version, want, "{text}");
        }
        // measured with QString::toInt on Qt 6
        for (text, want) in [
            ("-2\u{2028}", None),
            (" -2 ", Some(-2)),
            ("-2\u{a0}", None),
            ("-2\x0b", Some(-2)),
            ("-2 \0x", Some(-2)),
            ("-2\u{2028}\0x", None),
            ("+-2", None),
            ("\u{ff12}", None),
            ("007", Some(7)),
        ] {
            assert_eq!(to_int(text), want, "{text:?}");
        }
    }
}
