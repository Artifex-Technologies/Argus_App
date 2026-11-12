//! Differential test: `qjson` against Qt's own JSON parser and writer, as the C++ Sarina server uses
//! them (tests/qt-oracle/json_oracle.cpp; build it with `sh build.sh OUTDIR`, then run
//! `ARGUS_QT_ORACLES=OUTDIR cargo test -p argus-api --test qt_json`). Without the variable the test
//! says so and passes; the Linux CI job, which has Qt, runs it.
//!
//! Each case is one request line as the server would cut it from the socket, generated from a fixed
//! seed: requests with every kind of value in `cmd`, `token`, `limit` and `id` (numbers at the edges
//! of `i64`, `i32` and `f64`, exponents, strings with escapes, lone surrogates, control characters,
//! invalid UTF-8, nested and repeated keys), then often broken on purpose (cut, a byte changed or
//! dropped, a BOM, spaces, junk). Inputs are at most a few KiB, apart from a few deep nestings.

use std::io::Write;
use std::process::{Command, Stdio};

use argus_api::qjson::{self, Object, Value};

const CASES: usize = 4000;

struct Rng(u64, bool);

/// A piece that parses on its own (used for the half of the cases built only from good pieces).
fn good(piece: &[u8]) -> bool {
    let mut v = b"[".to_vec();
    v.extend_from_slice(piece);
    v.push(b']');
    qjson::from_json(&v).is_ok()
}

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
    /// A value piece; a good one when this case is a clean one.
    fn piece<'a>(&mut self, xs: &'a [&'a str]) -> &'a [u8] {
        loop {
            let p = self.pick(xs).as_bytes();
            if !self.1 || good(p) {
                return p;
            }
        }
    }
}

const NUMBERS: &[&str] = &[
    "0",
    "-0",
    "7",
    "-7",
    "7.0",
    "7.5",
    "1e2",
    "1E+2",
    "1e-2",
    "0.0001",
    "0.001",
    "1e-7",
    "123456.789",
    "1e21",
    "1e22",
    "1e300",
    "1e309",
    "-1e309",
    "1e-400",
    "0e-400",
    "0.000",
    "2147483647",
    "2147483648",
    "-2147483648",
    "-2147483649",
    "9223372036854775807",
    "9223372036854775808",
    "-9223372036854775808",
    "-9223372036854775809",
    "18446744073709551616",
    "123456789012345678901234567890",
    "9007199254740993",
    "1.",
    ".5",
    "-.5",
    "0.e5",
    ".e5",
    "-",
    "+1",
    "01",
    "1e",
    "1e+",
    "e5",
    "-e5",
    "0x10",
    "1.5e-310",
    "5e-324",
    "1.7976931348623157e308",
    "100000000000000000000",
    "0.1",
    "0.30000000000000004",
    "33.333333333333336",
    "1.2345678901234567e-5",
];

const STRINGS: &[&str] = &[
    r#""""#,
    r#""ping""#,
    r#""status""#,
    r#""log.tail""#,
    r#""a\"b""#,
    r#""a\\b""#,
    r#""\/""#,
    r#""\b\f\n\r\t""#,
    r#""\u0000""#,
    r#""\u001f\u007f\u0080""#,
    r#""é""#,
    r#""😀""#,
    r#""\ud800""#,
    r#""\udc00x""#,
    r#""\ud800\ud800""#,
    r#""􏿿""#,
    r#""\uZZZZ""#,
    r#""\u12""#,
    r#""\x""#,
    r#""\é""#,
    "\"é😀\"",
    "\"\u{2028}\u{fffe}\"",
    "\"tab\there\"",
    "\"nul\u{0}in\"",
    "\"\u{7f}\u{9b}\"",
    "\"unterminated",
    "\"bad \u{FFFD}\"",
];

const RAW_BYTES: &[&[u8]] = &[b"\xff", b"\xc0\xaf", b"\xed\xa0\x80", b"\xf4\x90\x80\x80", b"\xe2\x82", b"\x80", b"\xf0\x9f\x98"];

const KEYS: &[&str] = &[
    r#""cmd""#,
    r#""token""#,
    r#""limit""#,
    r#""id""#,
    r#""id""#,
    r#""name""#,
    r#""a""#,
    r#""b""#,
    r#""é""#,
    r#""é""#,
    r#""😀""#,
    "\"\u{ffff}\"",
    "\"😀\"",
    r#""￿""#,
    r#""""#,
    r#""ID""#,
];

fn value(r: &mut Rng, depth: usize, out: &mut Vec<u8>) {
    let choice = if depth > 3 { r.below(5) } else { r.below(8) };
    match choice {
        0 => out.extend_from_slice(r.piece(NUMBERS)),
        1 => out.extend_from_slice(r.piece(STRINGS)),
        2 => out.extend_from_slice(r.piece(&["true", "false", "null", "nul", "tru", "nulx", "fals"])),
        3 if r.1 => out.extend_from_slice(r.piece(STRINGS)),
        3 => {
            out.push(b'"');
            out.extend_from_slice(b"x");
            out.extend_from_slice(r.pick(RAW_BYTES));
            out.push(b'"');
        }
        4 => out.extend_from_slice(r.piece(NUMBERS)),
        5 | 6 => object(r, depth + 1, out),
        _ => {
            out.push(b'[');
            let n = r.below(4);
            for i in 0..n {
                if i > 0 {
                    out.extend_from_slice(if r.1 { b"," } else { r.pick(&[&b","[..], b",", b" , ", b""]) });
                }
                value(r, depth + 1, out);
            }
            out.push(b']');
        }
    }
}

fn object(r: &mut Rng, depth: usize, out: &mut Vec<u8>) {
    out.push(b'{');
    let n = r.below(6);
    for i in 0..n {
        if i > 0 {
            out.extend_from_slice(if r.1 { b", " } else { r.pick(&[&b","[..], b",", b", ", b"\t,\n", b",", b",", b""]) });
        }
        out.extend_from_slice(r.pick(KEYS).as_bytes());
        out.extend_from_slice(if r.1 { b":" } else { r.pick(&[&b":"[..], b":", b" : ", b":", b":", b":", b""]) });
        value(r, depth, out);
    }
    if !r.1 && r.below(20) == 0 {
        out.push(b',');
    }
    out.push(b'}');
}

fn case(r: &mut Rng, i: usize) -> Vec<u8> {
    // a few deep nestings around Qt's limit of 1024
    if i % 500 == 7 {
        let n = *r.pick(&[1023usize, 1024, 1025, 2000]);
        let mut v = b"{\"id\":".to_vec();
        v.extend(std::iter::repeat_n(b'[', n - 1));
        v.extend(std::iter::repeat_n(b']', n - 1));
        v.push(b'}');
        return v;
    }
    r.1 = r.below(2) == 0;
    let mut out = Vec::new();
    if r.below(10) == 0 {
        value(r, 0, &mut out);
    } else {
        object(r, 0, &mut out);
    }
    // break it now and then
    match if r.1 { 99 } else { r.below(12) } {
        0 if !out.is_empty() => {
            let at = r.below(out.len());
            out.truncate(at);
        }
        1 if !out.is_empty() => {
            let at = r.below(out.len());
            out[at] = *r.pick(b"{}[]:,\"\\ \tx0-.e\x00\xff\x0b\x0c\r");
        }
        2 if !out.is_empty() => {
            let at = r.below(out.len());
            out.remove(at);
        }
        3 => out.splice(0..0, b"\xef\xbb\xbf".iter().copied()).for_each(drop),
        4 => out.extend_from_slice(r.pick(&[&b" x"[..], b" {}", b"\r", b" \x0b", b"\x00", b"  "])),
        5 => out.splice(0..0, r.pick(&[&b" "[..], b"\t", b"\x0b", b"\r\n"]).iter().copied()).for_each(drop),
        _ => {}
    }
    out.retain(|&b| b != b'\n');
    out
}

fn hex(b: &[u8]) -> String {
    if b.is_empty() {
        return "-".into();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// What the oracle prints for one line, computed with the Rust port.
fn rust_line(raw: &[u8]) -> String {
    let trimmed = qjson::trimmed(raw);
    if trimmed.is_empty() {
        return "skip".into();
    }
    let parsed = qjson::from_json(trimmed);
    let request = match &parsed {
        Ok(Value::Object(o)) => o,
        _ => {
            let why = match parsed {
                Err(e) => e.text(),
                Ok(_) => "no error occurred",
            };
            let e = Object::new().with("ok", Value::Bool(false)).with("error", Value::from(format!("invalid JSON: {why}")));
            return format!("err {}", hex(&qjson::to_compact(&e.into())));
        }
    };
    let field = |k: &str| request.get(k).cloned().unwrap_or(Value::Null);
    let mut response = Object::new().with("ok", Value::Bool(true));
    if let Some(id) = request.get("id") {
        response.insert("id", id.clone());
    }
    let doc = Value::Object(request.clone());
    format!(
        "obj {} {} {} {} {} {}",
        hex(&qjson::to_compact(&doc)),
        hex(&qjson::to_indented(&doc)),
        hex(&qjson::to_utf8(&field("cmd").to_qstring())),
        hex(&qjson::to_utf8(&field("token").to_qstring())),
        field("limit").to_int(50),
        hex(&qjson::to_compact(&response.into())),
    )
}

#[test]
fn qjson_matches_qt() {
    let Some(dir) = std::env::var_os("ARGUS_QT_ORACLES") else {
        eprintln!("ARGUS_QT_ORACLES is not set: skipped (build the oracles with tests/qt-oracle/build.sh)");
        return;
    };
    let seed: u64 = std::env::var("ARGUS_QT_JSON_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x5A21_7A11);
    let mut r = Rng(seed | 1, false);
    let cases: Vec<Vec<u8>> = (0..CASES).map(|i| case(&mut r, i)).collect();
    let mut input = String::new();
    for c in &cases {
        input.push_str(&if c.is_empty() { String::new() } else { hex(c) });
        input.push('\n');
    }
    let oracle = std::path::Path::new(&dir).join("json-oracle");
    let mut child = Command::new(oracle).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().expect("run the oracle");
    let mut stdin = child.stdin.take().expect("stdin");
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child.wait_with_output().expect("oracle output");
    writer.join().expect("writer").expect("write");
    assert!(out.status.success(), "the oracle failed");
    let lines: Vec<&str> = std::str::from_utf8(&out.stdout).expect("ascii").lines().collect();
    assert_eq!(lines.len(), cases.len());
    let mut kinds = std::collections::BTreeMap::new();
    let mut bad = 0;
    for (c, want) in cases.iter().zip(&lines) {
        let got = rust_line(c);
        let kind = match want.strip_prefix("err ") {
            Some(h) => {
                let bytes: Vec<u8> = (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(b'?')).collect();
                let text = String::from_utf8_lossy(&bytes).into_owned();
                text.split("invalid JSON: ").nth(1).unwrap_or(&text).split('"').next().unwrap_or_default().to_string()
            }
            None => want[..4].trim().to_string(),
        };
        *kinds.entry(kind).or_insert(0) += 1;
        if got != *want {
            bad += 1;
            if bad <= 5 {
                eprintln!("input {}\n  qt   {want}\n  rust {got}", hex(c));
            }
        }
    }
    eprintln!("{} cases (seed {seed}): {kinds:?}", cases.len());
    assert_eq!(bad, 0, "{bad} of {} lines differ from Qt", cases.len());
}
