//! The Flipper's "Key: Value" text files (.sub, .ir, .nfc, .rfid, .ibtn): one reader, two views.
//!
//! [`FlipperFormat`] reads a file exactly as the C++ app's `FlipperFileFormat` does (the capture
//! `meta` the Sarina API hands out must match it byte for byte; argus-capture's Qt oracle checks
//! it): the file is read in `QIODevice::Text` mode, which drops every `\r` byte; a leading BOM is
//! dropped; each byte that is not part of a valid UTF-8 sequence becomes one U+FFFD; lines end at
//! `\n`; keys and values are trimmed of Unicode whitespace; `#` starts a comment; keys compare
//! case-insensitively; numbers parse like `QString::toInt`. A file with more than [`MAX_LINES`]
//! lines is not read at all, in both apps (a file of millions of tiny lines took gigabytes).
//!
//! [`parse`] is the capture library's view of the same pairs: bounded for display (at most
//! [`MAX_FILE`] bytes, keys up to 64 bytes, values cut at [`MAX_VALUE`] characters with control
//! characters shown as spaces) and with the numbers of long data lines (RAW_Data, data) counted.

/// The capture library reads files up to this size.
pub const MAX_FILE: usize = 4 << 20;
/// Larger files are not parsed by the capture records (`meta`); both apps keep them unparsed.
pub const MAX_PARSE: usize = 64 << 20;
/// Files with more lines are not parsed (both apps).
pub const MAX_LINES: usize = 200_000;
const MAX_KEY: usize = 64;
const MAX_VALUE: usize = 512;

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
pub fn same_key(a: &str, b: &str) -> bool {
    a.chars().flat_map(char::to_lowercase).eq(b.chars().flat_map(char::to_lowercase))
}

/// QString::toInt, as measured on Qt 6: only the part before the first NUL counts (Qt converts
/// through a C string), ASCII whitespace around it (space, \t, \n, \v, \f, \r) is ignored but no
/// other kind, base 10 with an optional sign; `None` when it fails or overflows.
pub fn to_int(s: &str) -> Option<i32> {
    let s = s.split('\0').next().unwrap_or_default();
    s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r')).parse::<i32>().ok()
}

impl FlipperFormat {
    /// `None` when the file holds no `Key: Value` line (not a Flipper file), as the C++ app.
    pub fn parse(data: &[u8]) -> Option<FlipperFormat> {
        if data.len() > MAX_PARSE || data.iter().filter(|&&b| b == b'\n').count() > MAX_LINES {
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

/// The capture library's view: fields in file order (keys repeat in .ir files, one block per
/// signal), values cut for display; long data lines are summarised by the caller from `raw_counts`.
#[derive(Debug, Default, Clone)]
pub struct FlipperFile {
    pub fields: Vec<(String, String)>,
    /// For keys whose values are long data (RAW_Data, data): number of numbers seen and the sum of
    /// their absolute values (microseconds for timings), per key.
    pub raw_counts: Vec<(String, u64, u64)>,
}

impl FlipperFile {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.iter().find(|(k, _)| same_key(k, key)).map(|(_, v)| v.as_str())
    }

    pub fn filetype(&self) -> Option<&str> {
        self.get("Filetype")
    }

    pub fn raw_totals(&self, key: &str) -> (u64, u64) {
        self.raw_counts.iter().filter(|(k, _, _)| same_key(k, key)).fold((0, 0), |(n, s), (_, a, b)| (n + a, s.saturating_add(*b)))
    }
}

fn is_long_data(key: &str) -> bool {
    same_key(key, "raw_data") || same_key(key, "data")
}

/// The capture library's view of a file (empty when it is not a Flipper-format file).
pub fn parse(bytes: &[u8]) -> FlipperFile {
    let mut f = FlipperFile::default();
    let Some(ff) = FlipperFormat::parse(&bytes[..bytes.len().min(MAX_FILE)]) else { return f };
    for (key, value) in ff.pairs {
        if key.len() > MAX_KEY {
            continue;
        }
        if is_long_data(&key) {
            let (mut n, mut sum) = (0u64, 0u64);
            for tok in value.split_whitespace() {
                if let Ok(x) = tok.parse::<i64>() {
                    n += 1;
                    sum = sum.saturating_add(x.unsigned_abs());
                }
            }
            f.raw_counts.push((key.clone(), n, sum));
        }
        let shown: String = value.chars().take(MAX_VALUE).map(|c| if c.is_control() { ' ' } else { c }).collect();
        f.fields.push((key, shown));
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fields_and_raw_counts() {
        let f = parse(
            b"Filetype: Flipper SubGhz RAW File\nVersion: 1\n# comment\nFrequency: 433920000\nRAW_Data: 100 -200 300\nRAW_Data: -50\n",
        );
        assert_eq!(f.filetype(), Some("Flipper SubGhz RAW File"));
        assert_eq!(f.get("frequency"), Some("433920000"));
        assert_eq!(f.raw_totals("RAW_Data"), (4, 650));
    }

    #[test]
    fn hostile_bytes_do_not_panic() {
        let f = parse(&[0xff, b':', 0, b'\n', b':', b':']);
        assert!(f.fields.len() <= 2);
    }

    #[test]
    fn both_views_read_a_bom_and_carriage_returns() {
        let f = parse(b"\xEF\xBB\xBFFiletype: Flipper NFC device\r\nUID: 04 A2\r\n");
        assert_eq!(f.filetype(), Some("Flipper NFC device"));
        assert_eq!(f.get("uid"), Some("04 A2"));
    }

    #[test]
    fn too_many_lines_are_not_read() {
        let mut data = b"Filetype: x\n".to_vec();
        data.extend(b"a: b\n".repeat(MAX_LINES));
        assert!(FlipperFormat::parse(&data).is_none());
        assert!(FlipperFormat::parse(&data[..data.len() - 5]).is_some());
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
