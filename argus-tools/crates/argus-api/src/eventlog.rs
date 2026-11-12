//! The live event log: the port of `ChymaeraEventLog` (backend/chymaera/eventlog). Entries are kept
//! in a ring of 5000 for `log.tail` and written to the datastore's `events` table as they come.
//! Messages stay UTF-16 (`QString`), so `log.tail` hands back exactly what `log.add` was given.

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

pub use argus_capture::store::{Severity, Source};

use crate::qjson::{Object, QString, Value};

pub const MAX_ENTRIES: usize = 5000;

pub fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::Debug => "debug",
        Severity::Info => "info",
        Severity::Notice => "notice",
        Severity::Warning => "warning",
        Severity::Error => "error",
        Severity::Critical => "critical",
    }
}

pub fn source_name(s: Source) -> &'static str {
    match s {
        Source::System => "system",
        Source::Device => "device",
        Source::Sarina => "sarina",
        Source::Capture => "capture",
        Source::Export => "export",
        Source::User => "user",
    }
}

/// `severityFromName`: trimmed, lower case, else the fallback.
pub fn severity_from_name(name: &str, fallback: Severity) -> Severity {
    match name.trim_matches(char::is_whitespace).to_lowercase().as_str() {
        "debug" => Severity::Debug,
        "info" => Severity::Info,
        "notice" => Severity::Notice,
        "warning" => Severity::Warning,
        "error" => Severity::Error,
        "critical" => Severity::Critical,
        _ => fallback,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub epoch_ms: i64,
    pub severity: Severity,
    pub source: Source,
    pub message: QString,
    pub detail: QString,
}

/// `QDateTime::toString(Qt::ISODateWithMs)` of a local time: `yyyy-MM-ddTHH:mm:ss.zzz`, no offset.
pub fn local_iso_ms(epoch_ms: i64) -> String {
    let secs = epoch_ms.div_euclid(1000);
    let ms = epoch_ms.rem_euclid(1000);
    // SAFETY: localtime_r only writes the tm we hand it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t: libc::time_t = secs as libc::time_t;
    let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
    if !ok {
        return String::new();
    }
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec, ms)
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl Entry {
    /// `entryToMap`, as `log.tail` hands it out.
    pub fn to_json(&self) -> Value {
        Object::new()
            .with("timestamp", Value::from(local_iso_ms(self.epoch_ms)))
            .with("epochMs", Value::Int(self.epoch_ms))
            .with("severity", Value::from(severity_name(self.severity)))
            .with("source", Value::from(source_name(self.source)))
            .with("message", Value::Str(self.message.clone()))
            .with("detail", Value::Str(self.detail.clone()))
            .into()
    }
}

#[derive(Default)]
pub struct EventLog {
    entries: VecDeque<Entry>,
}

impl EventLog {
    pub fn push(&mut self, e: Entry) {
        self.entries.push_back(e);
        while self.entries.len() > MAX_ENTRIES {
            self.entries.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// `tail(limit)`: the newest `limit` entries, oldest first; none for a negative limit.
    pub fn tail(&self, limit: i32) -> Vec<&Entry> {
        let n = self.entries.len();
        let start = n.saturating_sub(limit.max(0) as usize);
        self.entries.range(start..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_and_tail() {
        let mut log = EventLog::default();
        for i in 0..(MAX_ENTRIES + 3) {
            log.push(Entry { epoch_ms: i as i64, severity: Severity::Info, source: Source::User, message: vec![], detail: vec![] });
        }
        assert_eq!(log.len(), MAX_ENTRIES);
        assert_eq!(log.tail(2).iter().map(|e| e.epoch_ms).collect::<Vec<_>>(), [5001, 5002]);
        assert!(log.tail(-1).is_empty());
        assert_eq!(log.tail(i32::MAX).len(), MAX_ENTRIES);
    }

    #[test]
    fn names() {
        assert_eq!(severity_from_name(" WARNING\u{a0}", Severity::Info), Severity::Warning);
        assert_eq!(severity_from_name("loud", Severity::Info), Severity::Info);
        assert!(local_iso_ms(0).starts_with("19"));
        assert_eq!(local_iso_ms(1_789_045_402_145).len(), 23);
    }
}
