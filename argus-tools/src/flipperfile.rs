//! The Flipper's "Key: Value" text files (.sub, .ir, .nfc, .rfid, .ibtn), read with bounds.

pub const MAX_FILE: usize = 4 << 20;
const MAX_LINES: usize = 200_000;
const MAX_VALUE: usize = 512;

/// Fields in file order (keys repeat in .ir files, one block per signal). Values are cut at
/// MAX_VALUE characters; long data lines are summarised by the caller from `long_values`.
#[derive(Debug, Default, Clone)]
pub struct FlipperFile {
    pub fields: Vec<(String, String)>,
    /// For keys whose values are long data (RAW_Data, data): number of numbers seen and the sum of
    /// their absolute values (microseconds for timings), per key.
    pub raw_counts: Vec<(String, u64, u64)>,
}

impl FlipperFile {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str())
    }

    pub fn filetype(&self) -> Option<&str> {
        self.get("Filetype")
    }

    pub fn raw_totals(&self, key: &str) -> (u64, u64) {
        self.raw_counts
            .iter()
            .filter(|(k, _, _)| k.eq_ignore_ascii_case(key))
            .fold((0, 0), |(n, s), (_, a, b)| (n + a, s.saturating_add(*b)))
    }
}

fn is_long_data(key: &str) -> bool {
    matches!(key.to_ascii_lowercase().as_str(), "raw_data" | "data")
}

pub fn parse(bytes: &[u8]) -> FlipperFile {
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_FILE)]);
    let mut f = FlipperFile::default();
    for line in text.lines().take(MAX_LINES) {
        let line = line.trim_end_matches('\r');
        if line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let key = k.trim();
        if key.is_empty() || key.len() > 64 {
            continue;
        }
        let value = v.trim();
        if is_long_data(key) {
            let (mut n, mut sum) = (0u64, 0u64);
            for tok in value.split_whitespace() {
                if let Ok(x) = tok.parse::<i64>() {
                    n += 1;
                    sum = sum.saturating_add(x.unsigned_abs());
                }
            }
            f.raw_counts.push((key.to_string(), n, sum));
        }
        let shown: String = value.chars().take(MAX_VALUE).map(|c| if c.is_control() { ' ' } else { c }).collect();
        f.fields.push((key.to_string(), shown));
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fields_and_raw_counts() {
        let f = parse(b"Filetype: Flipper SubGhz RAW File\nVersion: 1\n# comment\nFrequency: 433920000\nRAW_Data: 100 -200 300\nRAW_Data: -50\n");
        assert_eq!(f.filetype(), Some("Flipper SubGhz RAW File"));
        assert_eq!(f.get("frequency"), Some("433920000"));
        assert_eq!(f.raw_totals("RAW_Data"), (4, 650));
    }

    #[test]
    fn hostile_bytes_do_not_panic() {
        let f = parse(&[0xff, b':', 0, b'\n', b':', b':']);
        assert!(f.fields.len() <= 2);
    }
}
