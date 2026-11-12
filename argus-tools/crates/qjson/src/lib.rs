//! JSON as Qt 6 reads and writes it (`QJsonDocument::fromJson`, `toJson`), so the Rust Sarina server
//! answers byte for byte as the C++ one: the same error texts for broken requests, the same numbers
//! (integral values become integers, others Qt's shortest `'g'` form), the same key order and the
//! same escapes. Strings are kept as UTF-16, as `QString` keeps them, so a lone surrogate in a
//! request echoes back the way Qt writes it. `tests/qt_json.rs` checks all of it against Qt itself.
//!
//! Qt's rules followed here (Qt 6.12, `qjsonparser.cpp`, `qjsonwriter.cpp`, `qlocale_tools.cpp`):
//! a leading BOM is skipped only when more than three bytes follow; any value may be the document
//! but only an object or array makes one; control characters are allowed inside strings; an unknown
//! escape `\x` is the byte `x`; a number is scanned as `-?(0|[0-9]*)(\.[0-9]*)?([eE][+-]?[0-9]*)?`
//! and then converted, failing on overflow and on an underflow to zero; a number that is a whole
//! value within `i64` becomes an integer; objects are sorted by key and the last of a repeated key
//! wins; nesting deeper than 1024 fails.

use std::cmp::Ordering;

pub type QString = Vec<u16>;

pub fn qs(s: &str) -> QString {
    s.encode_utf16().collect()
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Double(f64),
    Str(QString),
    Array(Vec<Value>),
    Object(Object),
}

/// A `QJsonObject`: entries in key order.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Object {
    entries: Vec<(QString, Value)>,
}

impl Object {
    pub fn new() -> Object {
        Object::default()
    }

    fn find(&self, key: &[u16]) -> Result<usize, usize> {
        self.entries.binary_search_by(|(k, _)| k.as_slice().cmp(key))
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.find(&qs(key)).ok().map(|i| &self.entries[i].1)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// `QJsonObject::insert`; `None` is `QJsonValue::Undefined`, which removes the key.
    pub fn insert(&mut self, key: &str, value: impl Into<Option<Value>>) {
        let key = qs(key);
        match (self.find(&key), value.into()) {
            (Ok(i), Some(v)) => self.entries[i].1 = v,
            (Ok(i), None) => {
                self.entries.remove(i);
            }
            (Err(i), Some(v)) => self.entries.insert(i, (key, v)),
            (Err(_), None) => {}
        }
    }

    pub fn with(mut self, key: &str, value: impl Into<Option<Value>>) -> Object {
        self.insert(key, value);
        self
    }

    pub fn entries(&self) -> &[(QString, Value)] {
        &self.entries
    }
}

impl From<Object> for Value {
    fn from(o: Object) -> Value {
        Value::Object(o)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Value {
        Value::Str(qs(s))
    }
}

impl From<String> for Value {
    fn from(s: String) -> Value {
        Value::Str(qs(&s))
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Value {
        Value::Bool(b)
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Value {
        Value::Int(n)
    }
}

impl Value {
    /// `QJsonValue::toString()`: the string, or empty for any other type.
    pub fn to_qstring(&self) -> QString {
        match self {
            Value::Str(s) => s.clone(),
            _ => QString::new(),
        }
    }

    /// `QJsonValue::toString()` as Rust text (a lone surrogate becomes U+FFFD).
    pub fn to_text(&self) -> String {
        String::from_utf16_lossy(&self.to_qstring())
    }

    /// `QJsonValue::toInt(default)`: a whole number within `i32`, else the default.
    pub fn to_int(&self, default: i32) -> i32 {
        match *self {
            Value::Int(n) => i32::try_from(n).unwrap_or(default),
            Value::Double(d) => {
                if d.fract() == 0.0 && d >= i32::MIN as f64 && d <= i32::MAX as f64 {
                    d as i32
                } else {
                    default
                }
            }
            _ => default,
        }
    }

    pub fn as_object(&self) -> Option<&Object> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }
}

/// `QString::toUtf8()`: Qt leaves a lone surrogate out.
pub fn to_utf8(s: &[u16]) -> Vec<u8> {
    char::decode_utf16(s.iter().copied()).flatten().collect::<String>().into_bytes()
}

/// `QByteArray::trimmed()`: ASCII space, \t, \n, \v, \f and \r off both ends.
pub fn trimmed(b: &[u8]) -> &[u8] {
    let ws = |c: &u8| matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r');
    let start = b.iter().position(|c| !ws(c)).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !ws(c)).map_or(start, |i| i + 1);
    &b[start..end]
}

/// `QJsonParseError::ParseError`, the ones the parser can return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    UnterminatedObject,
    MissingNameSeparator,
    UnterminatedArray,
    MissingValueSeparator,
    IllegalValue,
    IllegalNumber,
    IllegalEscapeSequence,
    IllegalUtf8String,
    UnterminatedString,
    MissingObject,
    DeepNesting,
    GarbageAtEnd,
}

impl ParseError {
    /// `QJsonParseError::errorString()`.
    pub fn text(self) -> &'static str {
        match self {
            ParseError::UnterminatedObject => "unterminated object",
            ParseError::MissingNameSeparator => "missing name separator",
            ParseError::UnterminatedArray => "unterminated array",
            ParseError::MissingValueSeparator => "missing value separator",
            ParseError::IllegalValue => "illegal value",
            ParseError::IllegalNumber => "illegal number",
            ParseError::IllegalEscapeSequence => "invalid escape sequence",
            ParseError::IllegalUtf8String => "invalid UTF8 string",
            ParseError::UnterminatedString => "unterminated string",
            ParseError::MissingObject => "object is missing after a comma",
            ParseError::DeepNesting => "too deeply nested document",
            ParseError::GarbageAtEnd => "garbage at the end of the document",
        }
    }
}

const NESTING_LIMIT: u32 = 1024;

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    depth: u32,
}

type R<T> = Result<T, ParseError>;

/// `QJsonDocument::fromJson`: an object or an array, or the parse error (a valid scalar is
/// `IllegalValue`, as Qt reports it).
pub fn from_json(data: &[u8]) -> R<Value> {
    let mut p = Parser { s: data, i: 0, depth: 0 };
    match p.parse()? {
        v @ (Value::Object(_) | Value::Array(_)) => Ok(v),
        _ => Err(ParseError::IllegalValue),
    }
}

fn is_digit(c: u8) -> bool {
    c.is_ascii_digit()
}

impl Parser<'_> {
    fn at_end(&self) -> bool {
        self.i >= self.s.len()
    }

    fn eat_space(&mut self) -> bool {
        while let Some(&c) = self.s.get(self.i) {
            if !matches!(c, b' ' | b'\t' | b'\n' | b'\r') {
                break;
            }
            self.i += 1;
        }
        !self.at_end()
    }

    fn next_token(&mut self) -> u8 {
        if !self.eat_space() {
            return 0;
        }
        let t = self.s[self.i];
        self.i += 1;
        match t {
            b'[' | b'{' | b':' | b',' | b']' | b'}' | b'"' => t,
            _ => 0,
        }
    }

    fn parse(&mut self) -> R<Value> {
        if self.s.len() > 3 && self.s.starts_with(b"\xEF\xBB\xBF") {
            self.i = 3;
        }
        if !self.eat_space() {
            return Err(ParseError::IllegalValue);
        }
        let v = if self.s[self.i] == b'"' {
            self.i += 1;
            Value::Str(self.parse_string()?.0)
        } else {
            self.parse_value()?
        };
        self.eat_space();
        if !self.at_end() {
            return Err(ParseError::GarbageAtEnd);
        }
        Ok(v)
    }

    fn literal(&mut self, rest: &[u8], v: Value) -> R<Value> {
        if self.s[self.i..].starts_with(rest) {
            self.i += rest.len();
            Ok(v)
        } else {
            Err(ParseError::IllegalValue)
        }
    }

    /// Called with at least one byte left.
    fn parse_value(&mut self) -> R<Value> {
        let c = self.s[self.i];
        self.i += 1;
        match c {
            b'n' => self.literal(b"ull", Value::Null),
            b't' => self.literal(b"rue", Value::Bool(true)),
            b'f' => self.literal(b"alse", Value::Bool(false)),
            b'"' => Ok(Value::Str(self.parse_string()?.0)),
            b'[' => self.parse_array(),
            b'{' => self.parse_object(),
            b',' => Err(ParseError::IllegalValue),
            b'}' | b']' => Err(ParseError::MissingObject),
            _ => {
                self.i -= 1;
                self.parse_number()
            }
        }
    }

    fn parse_object(&mut self) -> R<Value> {
        self.depth += 1;
        if self.depth > NESTING_LIMIT {
            return Err(ParseError::DeepNesting);
        }
        // (key, stored as UTF-16 by Qt (it had an escape), value)
        let mut members: Vec<(QString, bool, Value)> = Vec::new();
        let mut token = self.next_token();
        while token == b'"' {
            let (key, escaped) = self.parse_string()?;
            if self.next_token() != b':' {
                return Err(ParseError::MissingNameSeparator);
            }
            if !self.eat_space() {
                return Err(ParseError::UnterminatedObject);
            }
            let v = self.parse_value()?;
            members.push((key, escaped, v));
            token = self.next_token();
            if token != b',' {
                break;
            }
            token = self.next_token();
            if token == b'}' {
                return Err(ParseError::MissingObject);
            }
        }
        if token != b'}' {
            return Err(ParseError::UnterminatedObject);
        }
        self.depth -= 1;
        // Qt: a stable sort by key, then the last of equal keys wins.
        members.sort_by(|a, b| key_order(&a.0, a.1, &b.0, b.1));
        let mut entries: Vec<(QString, Value)> = Vec::with_capacity(members.len());
        let mut flags: Vec<bool> = Vec::with_capacity(members.len());
        for (k, esc, v) in members {
            if let (Some(last), Some(&last_esc)) = (entries.last_mut(), flags.last()) {
                if key_order(&last.0, last_esc, &k, esc) == Ordering::Equal {
                    *last = (k, v);
                    *flags.last_mut().expect("same length") = esc;
                    continue;
                }
            }
            entries.push((k, v));
            flags.push(esc);
        }
        Ok(Value::Object(Object { entries }))
    }

    fn parse_array(&mut self) -> R<Value> {
        self.depth += 1;
        if self.depth > NESTING_LIMIT {
            return Err(ParseError::DeepNesting);
        }
        if !self.eat_space() {
            return Err(ParseError::UnterminatedArray);
        }
        let mut items = Vec::new();
        if self.s[self.i] == b']' {
            self.next_token();
        } else {
            loop {
                if !self.eat_space() {
                    return Err(ParseError::UnterminatedArray);
                }
                items.push(self.parse_value()?);
                let token = self.next_token();
                if token == b']' {
                    break;
                } else if token != b',' {
                    return Err(if !self.eat_space() { ParseError::UnterminatedArray } else { ParseError::MissingValueSeparator });
                }
            }
        }
        self.depth -= 1;
        Ok(Value::Array(items))
    }

    fn parse_number(&mut self) -> R<Value> {
        let s = self.s;
        let start = self.i;
        let mut is_int = true;
        if s.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        if s.get(self.i) == Some(&b'0') {
            self.i += 1;
        } else {
            while s.get(self.i).copied().is_some_and(is_digit) {
                self.i += 1;
            }
        }
        if s.get(self.i) == Some(&b'.') {
            self.i += 1;
            while let Some(&c) = s.get(self.i).filter(|c| is_digit(**c)) {
                is_int = is_int && c == b'0';
                self.i += 1;
            }
        }
        if matches!(s.get(self.i), Some(b'e' | b'E')) {
            is_int = false;
            self.i += 1;
            if matches!(s.get(self.i), Some(b'-' | b'+')) {
                self.i += 1;
            }
            while s.get(self.i).copied().is_some_and(is_digit) {
                self.i += 1;
            }
        }
        // only ASCII was scanned
        let text = std::str::from_utf8(&s[start..self.i]).map_err(|_| ParseError::IllegalNumber)?;
        if is_int {
            if let Ok(n) = text.parse::<i64>() {
                return Ok(Value::Int(n));
            }
        }
        let d = to_double(text).ok_or(ParseError::IllegalNumber)?;
        Ok(match double_to_i64(d) {
            Some(n) => Value::Int(n),
            None => Value::Double(d),
        })
    }

    /// After the opening quote. Returns the string and whether Qt stores it as UTF-16 (it had a
    /// backslash), which decides how keys sort.
    fn parse_string(&mut self) -> R<(QString, bool)> {
        let start = self.i;
        // the fast pass: plain UTF-8 up to the closing quote
        let mut escaped = false;
        while !self.at_end() {
            match self.s[self.i] {
                b'"' => break,
                b'\\' => {
                    escaped = true;
                    break;
                }
                _ => {
                    self.scan_utf8()?;
                }
            }
        }
        if !escaped {
            if self.at_end() {
                return Err(ParseError::UnterminatedString);
            }
            let text = std::str::from_utf8(&self.s[start..self.i]).map_err(|_| ParseError::IllegalUtf8String)?;
            self.i += 1;
            return Ok((qs(text), false));
        }
        self.i = start;
        let mut out = QString::new();
        while !self.at_end() {
            match self.s[self.i] {
                b'"' => break,
                b'\\' => {
                    let u = self.scan_escape().ok_or(ParseError::IllegalEscapeSequence)?;
                    out.push(u);
                }
                _ => {
                    let c = self.scan_utf8()?;
                    let mut buf = [0u16; 2];
                    out.extend_from_slice(c.encode_utf16(&mut buf));
                }
            }
        }
        if self.at_end() {
            return Err(ParseError::UnterminatedString);
        }
        self.i += 1;
        Ok((out, true))
    }

    /// One UTF-8 character (Qt's `nextUcs4FromUtf8`: no overlong forms, surrogates or values past
    /// U+10FFFF, as Rust's own check).
    fn scan_utf8(&mut self) -> R<char> {
        let rest = &self.s[self.i..];
        let len = match rest[0] {
            0x00..=0x7F => 1,
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => return Err(ParseError::IllegalUtf8String),
        };
        let bytes = rest.get(..len).ok_or(ParseError::IllegalUtf8String)?;
        let c = std::str::from_utf8(bytes).ok().and_then(|s| s.chars().next()).ok_or(ParseError::IllegalUtf8String)?;
        self.i += len;
        Ok(c)
    }

    /// At a backslash: one UTF-16 unit (a `\u` escape may be half a surrogate pair).
    fn scan_escape(&mut self) -> Option<u16> {
        self.i += 1;
        let e = *self.s.get(self.i)?;
        self.i += 1;
        Some(match e {
            b'b' => 0x8,
            b'f' => 0xc,
            b'n' => 0xa,
            b'r' => 0xd,
            b't' => 0x9,
            b'u' => {
                let hex = self.s.get(self.i..self.i + 4)?;
                let mut v: u16 = 0;
                for &h in hex {
                    v = (v << 4) | (h as char).to_digit(16)? as u16;
                    self.i += 1;
                }
                v
            }
            // '"', '\\', '/' and anything else: the byte itself
            other => u16::from(other),
        })
    }
}

/// How Qt orders two parsed keys: by UTF-16 units when both had an escape, else by code points.
fn key_order(a: &[u16], a_utf16: bool, b: &[u16], b_utf16: bool) -> Ordering {
    if a_utf16 && b_utf16 {
        a.cmp(b)
    } else {
        let points = |s: &[u16]| -> Vec<u32> {
            char::decode_utf16(s.iter().copied()).map(|r| r.map(u32::from).unwrap_or_else(|e| u32::from(e.unpaired_surrogate()))).collect()
        };
        points(a).cmp(&points(b))
    }
}

/// `QByteArray::toDouble` on a scanned number: fails on anything Rust cannot read, on overflow and
/// on an underflow to zero of a number with a non-zero digit before its exponent.
fn to_double(text: &str) -> Option<f64> {
    let d: f64 = text.parse().ok()?;
    if !d.is_finite() {
        return None;
    }
    if d == 0.0 {
        let mantissa = text.split(['e', 'E']).next().unwrap_or_default();
        if mantissa.bytes().any(|b| (b'1'..=b'9').contains(&b)) {
            return None;
        }
    }
    Some(d)
}

/// `convertDoubleTo<qint64>`: a whole number within range.
fn double_to_i64(d: f64) -> Option<i64> {
    if d.fract() == 0.0 && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&d) {
        Some(d as i64)
    } else {
        None
    }
}

/// `QJsonValue(double)` (`doubleValueHelper`): an integer when that adds no precision.
pub fn double_value(d: f64) -> Value {
    if d.fract() == 0.0 && d.abs() <= 9_007_199_254_740_992.0 {
        Value::Int(d as i64)
    } else {
        Value::Double(d)
    }
}

/// `QByteArray::number(d, 'g', QLocale::FloatingPointShortest)` for a finite `d`.
pub fn format_double(d: f64) -> String {
    if d == 0.0 {
        return "0".into();
    }
    // Rust's shortest round-trip digits, as double-conversion's
    let e = format!("{:e}", d.abs());
    let (mantissa, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let decpt = exp.parse::<i32>().unwrap_or(0) + 1;
    let length = digits.len() as i32;
    // resolveFormat
    let mut bias = 4;
    if length <= decpt && length > 1 {
        bias += 1;
    } else if length == 1 && decpt <= 0 {
        bias -= 1;
    }
    let use_decimal = if decpt <= 0 {
        1 - decpt <= bias
    } else if decpt <= length {
        true
    } else {
        decpt <= length + bias
    };
    let mut out = String::new();
    if d < 0.0 {
        out.push('-');
    }
    if use_decimal {
        if decpt <= 0 {
            out.push_str("0.");
            for _ in decpt..0 {
                out.push('0');
            }
            out.push_str(&digits);
        } else if decpt >= length {
            out.push_str(&digits);
            for _ in length..decpt {
                out.push('0');
            }
        } else {
            out.push_str(&digits[..decpt as usize]);
            out.push('.');
            out.push_str(&digits[decpt as usize..]);
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let x = decpt - 1;
        out.push('e');
        out.push(if x < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", x.abs()));
    }
    out
}

fn hexdig(u: u16) -> u8 {
    b"0123456789abcdef"[usize::from(u & 0xf)]
}

/// Qt's `escapedString`.
fn write_string(s: &[u16], out: &mut Vec<u8>) {
    out.push(b'"');
    let mut i = 0;
    while i < s.len() {
        let u = s[i];
        i += 1;
        if u < 0x80 {
            if u < 0x20 || u == 0x22 || u == 0x5c {
                out.push(b'\\');
                match u {
                    0x22 => out.push(b'"'),
                    0x5c => out.push(b'\\'),
                    0x8 => out.push(b'b'),
                    0xc => out.push(b'f'),
                    0xa => out.push(b'n'),
                    0xd => out.push(b'r'),
                    0x9 => out.push(b't'),
                    _ => out.extend_from_slice(&[b'u', b'0', b'0', hexdig(u >> 4), hexdig(u)]),
                }
            } else {
                out.push(u as u8);
            }
            continue;
        }
        let c = if (0xD800..0xDC00).contains(&u) && s.get(i).is_some_and(|l| (0xDC00..0xE000).contains(l)) {
            let c = char::from_u32(0x10000 + ((u32::from(u) - 0xD800) << 10) + (u32::from(s[i]) - 0xDC00));
            i += 1;
            c
        } else {
            char::from_u32(u32::from(u))
        };
        match c {
            Some(c) => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            None => out.extend_from_slice(&[b'\\', b'u', hexdig(u >> 12), hexdig(u >> 8), hexdig(u >> 4), hexdig(u)]),
        }
    }
    out.push(b'"');
}

fn write_value(v: &Value, out: &mut Vec<u8>, indent: usize, compact: bool) {
    match v {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Int(n) => out.extend_from_slice(n.to_string().as_bytes()),
        Value::Double(d) if d.is_finite() => out.extend_from_slice(format_double(*d).as_bytes()),
        Value::Double(_) => out.extend_from_slice(b"null"),
        Value::Str(s) => write_string(s, out),
        Value::Array(items) => {
            out.extend_from_slice(if compact { b"[" } else { b"[\n" });
            let inner = indent + usize::from(!compact);
            for (n, item) in items.iter().enumerate() {
                out.resize(out.len() + 4 * inner, b' ');
                write_value(item, out, inner, compact);
                if n + 1 < items.len() {
                    out.extend_from_slice(if compact { b"," } else { b",\n" });
                } else if !compact {
                    out.push(b'\n');
                }
            }
            out.resize(out.len() + 4 * indent, b' ');
            out.push(b']');
        }
        Value::Object(o) => {
            out.extend_from_slice(if compact { b"{" } else { b"{\n" });
            let inner = indent + usize::from(!compact);
            for (n, (k, item)) in o.entries.iter().enumerate() {
                out.resize(out.len() + 4 * inner, b' ');
                write_string(k, out);
                out.extend_from_slice(if compact { b":" } else { b": " });
                write_value(item, out, inner, compact);
                if n + 1 < o.entries.len() {
                    out.extend_from_slice(if compact { b"," } else { b",\n" });
                } else if !compact {
                    out.push(b'\n');
                }
            }
            out.resize(out.len() + 4 * indent, b' ');
            out.push(b'}');
        }
    }
}

/// `QJsonDocument(v).toJson(QJsonDocument::Compact)`.
pub fn to_compact(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_value(v, &mut out, 0, true);
    out
}

/// `QJsonDocument(v).toJson(QJsonDocument::Indented)` (four spaces, a newline at the end).
pub fn to_indented(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_value(v, &mut out, 0, false);
    out.push(b'\n');
    out
}

/// A serde_json value as Qt would see it after reading its text (how the C++ app reads
/// argus-tools' output).
pub fn from_serde(v: &serde_json::Value) -> Value {
    match from_json(v.to_string().as_bytes()) {
        Ok(v) => v,
        // a scalar: wrap it to read it the same way
        Err(_) => match from_json(format!("[{v}]").as_bytes()) {
            Ok(Value::Array(mut a)) if a.len() == 1 => a.remove(0),
            _ => Value::Null,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(s: &str) -> String {
        match from_json(s.as_bytes()) {
            Ok(v) => String::from_utf8(to_compact(&v)).unwrap(),
            Err(e) => format!("error: {}", e.text()),
        }
    }

    #[test]
    fn as_qt_writes_them() {
        assert_eq!(round(r#"{"id":7.0,"cmd":"ping"}"#), r#"{"cmd":"ping","id":7}"#);
        assert_eq!(round(r#"{"a":1,"a":2}"#), r#"{"a":2}"#);
        assert_eq!(round(r#"{"id":1e300,"x":"\ud800"}"#), r#"{"id":1e+300,"x":"\ud800"}"#);
        assert_eq!(round(r#"[1e-400]"#), "error: illegal number");
        assert_eq!(round(r#"[0e-400, 0.0001, 0.001, 1.5, 1e21]"#), "[0,1e-04,0.001,1.5,1e+21]");
        assert_eq!(round(r#""x""#), "error: illegal value");
        assert_eq!(round(r#"{"a":1,}"#), "error: object is missing after a comma");
        assert_eq!(round("[1 2]"), "error: missing value separator");
        assert_eq!(round("[1 2"), "error: unterminated array");
    }

    #[test]
    fn nesting_limit() {
        let ok = format!("{}{}", "[".repeat(1024), "]".repeat(1024));
        assert!(from_json(ok.as_bytes()).is_ok());
        let deep = format!("{}{}", "[".repeat(1025), "]".repeat(1025));
        assert_eq!(from_json(deep.as_bytes()), Err(ParseError::DeepNesting));
    }

    #[test]
    fn to_int_as_qt() {
        assert_eq!(Value::Int(5).to_int(50), 5);
        assert_eq!(Value::Int(1 << 40).to_int(50), 50);
        assert_eq!(Value::Double(2.5).to_int(50), 50);
        assert_eq!(Value::Str(qs("5")).to_int(50), 50);
    }

    #[test]
    fn lone_surrogates_leave_utf8() {
        assert_eq!(to_utf8(&[0xd800, u16::from(b'x')]), b"x");
    }
}
