//! The capture library: Sub-GHz, infrared, NFC (and 125 kHz RFID / iButton) files from the
//! card's pulls and from snapshots, indexed by content, tagged, searchable, with duplicates found.
//!
//! Captures are private and stay on this PC. The index keeps header facts (frequency, protocol,
//! card type, UID...) and never the payload: no key codes, RAW timings, IR data, or card blocks.
//! Semantic duplicates are found with a fingerprint that is a hash of those values, so the values
//! themselves are not stored in it either.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::flipperfile::{self, FlipperFile};
use crate::snapshot::Snapshots;
use crate::store;
use crate::{Error, Result};

pub const FORMAT: u32 = 1;

/// Capture folders on the card and the file types they hold. `assets` sub-folders (key
/// dictionaries, manufacturer keys) are never indexed.
pub const FOLDERS: &[(&str, &str)] =
    &[("/ext/subghz", "sub"), ("/ext/infrared", "ir"), ("/ext/nfc", "nfc"), ("/ext/lfrfid", "rfid"), ("/ext/ibutton", "ibtn")];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Location {
    /// "snapshot", "pull" or "file"
    pub origin: String,
    /// Snapshot id, or the folder it was found in.
    pub source: String,
    /// The path on the card.
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capture {
    pub id: String,
    pub sha256: String,
    pub kind: String,
    pub filetype: String,
    pub name: String,
    pub size: u64,
    pub summary: String,
    pub fields: BTreeMap<String, Value>,
    #[serde(default)]
    pub fingerprint: Option<String>,
    pub locations: Vec<Location>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    pub format: u32,
    pub built_unix: u64,
    pub captures: Vec<Capture>,
    #[serde(default)]
    pub skipped: Vec<String>,
}

fn kind_of(ft: &str) -> Option<&'static str> {
    let f = ft.to_ascii_lowercase();
    Some(if f.contains("subghz raw") {
        "subghz_raw"
    } else if f.contains("subghz") {
        "subghz"
    } else if f.starts_with("ir ") {
        "infrared"
    } else if f.contains("nfc") {
        "nfc"
    } else if f.contains("rfid") {
        "rfid"
    } else if f.contains("ibutton") {
        "ibutton"
    } else {
        return None;
    })
}

fn preset_name(p: &str) -> String {
    let map = [
        ("Ook270", "AM270"),
        ("Ook650", "AM650"),
        ("2FSKDev238", "FM238"),
        ("2FSKDev12K", "FM12K"),
        ("2FSKDev476", "FM476"),
        ("Custom", "Custom"),
        ("MSK99_97Kb", "MSK"),
        ("GFSK9_99Kb", "GFSK"),
    ];
    map.iter().find(|(k, _)| p.contains(k)).map(|(_, v)| v.to_string()).unwrap_or_else(|| p.to_string())
}

/// Rolling-code protocols in the Sub-GHz app: a replayed capture is normally refused by the receiver.
const ROLLING: &[&str] = &[
    "KeeLoq",
    "Star Line",
    "Faac SLH",
    "Somfy Telis",
    "Somfy Keytis",
    "Alutech AT-4N",
    "KingGates Stylo4k",
    "BFT Mitto",
    "Security+ 1.0",
    "Security+ 2.0",
    "Nice FloR-S",
    "Came Atomo",
    "Phoenix_V2",
    "Kinggates",
];

fn fp(parts: &[&str]) -> String {
    crate::sha256_hex(parts.join("\u{1f}").as_bytes())[..32].to_string()
}

fn mhz(hz: u64) -> String {
    let m = format!("{:.3}", hz as f64 / 1e6);
    m.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Read one capture's header facts. None if it is not a capture type Argus knows.
pub struct Described {
    pub kind: String,
    pub filetype: String,
    pub fields: BTreeMap<String, Value>,
    pub summary: String,
    pub fingerprint: Option<String>,
}

pub fn describe(bytes: &[u8]) -> Option<Described> {
    let f: FlipperFile = flipperfile::parse(bytes);
    let ft = f.filetype()?.to_string();
    let kind = kind_of(&ft)?;
    let mut m = BTreeMap::new();
    let get = |k: &str| f.get(k).map(str::to_string);
    if let Some(v) = get("Version") {
        m.insert("version".into(), json!(v));
    }
    let (summary, fingerprint) = match kind {
        "subghz" | "subghz_raw" => {
            let hz = get("Frequency").and_then(|v| v.parse::<u64>().ok());
            if let Some(hz) = hz {
                m.insert("frequency_hz".into(), json!(hz));
                m.insert("frequency_mhz".into(), json!(mhz(hz)));
            }
            if let Some(p) = get("Preset") {
                m.insert("preset".into(), json!(preset_name(&p)));
            }
            let proto = get("Protocol").unwrap_or_default();
            if !proto.is_empty() {
                m.insert("protocol".into(), json!(proto));
            }
            let freq = hz.map(|h| format!("{} MHz", mhz(h))).unwrap_or_else(|| "unknown frequency".into());
            if kind == "subghz_raw" {
                let (n, us) = f.raw_totals("RAW_Data");
                m.insert("raw_samples".into(), json!(n));
                m.insert("raw_duration_ms".into(), json!(us / 1000));
                (format!("Sub-GHz RAW recording at {freq}: {n} timings, {} ms", us / 1000), None)
            } else {
                let bits = get("Bit").and_then(|v| v.parse::<u32>().ok());
                if let Some(b) = bits {
                    m.insert("bits".into(), json!(b));
                }
                if let Some(te) = get("TE").and_then(|v| v.parse::<u32>().ok()) {
                    m.insert("te_us".into(), json!(te));
                }
                let rolling = ROLLING.iter().any(|r| proto.eq_ignore_ascii_case(r));
                m.insert("code_type".into(), json!(if rolling { "rolling" } else { "fixed" }));
                let key = get("Key").unwrap_or_default();
                let fpv =
                    (!key.is_empty()).then(|| fp(&["subghz", &hz.unwrap_or(0).to_string(), &proto, &bits.unwrap_or(0).to_string(), &key]));
                let b = bits.map(|b| format!("{b}-bit ")).unwrap_or_default();
                (format!("Sub-GHz {proto} {b}key at {freq}"), fpv)
            }
        }
        "infrared" => {
            let mut signals = Vec::new();
            let mut cur: Option<serde_json::Map<String, Value>> = None;
            let mut parsed_keys = BTreeSet::new();
            let (mut pa, mut pc, mut pp) = (String::new(), String::new(), String::new());
            for (k, v) in &f.fields {
                match k.to_ascii_lowercase().as_str() {
                    "name" => {
                        if let Some(s) = cur.take() {
                            signals.push(Value::Object(s));
                        }
                        if !pp.is_empty() {
                            parsed_keys.insert(format!("{pp}/{pa}/{pc}"));
                        }
                        pa.clear();
                        pc.clear();
                        pp.clear();
                        let mut s = serde_json::Map::new();
                        s.insert("name".into(), json!(v));
                        cur = Some(s);
                    }
                    "type" | "frequency" => {
                        if let Some(s) = cur.as_mut() {
                            s.insert(k.to_ascii_lowercase(), json!(v));
                        }
                    }
                    "protocol" => {
                        pp = v.clone();
                        if let Some(s) = cur.as_mut() {
                            s.insert("protocol".into(), json!(v));
                        }
                    }
                    "address" => pa = v.clone(),
                    "command" => pc = v.clone(),
                    _ => {}
                }
            }
            if let Some(s) = cur.take() {
                signals.push(Value::Object(s));
            }
            if !pp.is_empty() {
                parsed_keys.insert(format!("{pp}/{pa}/{pc}"));
            }
            let mut protos: BTreeMap<String, usize> = BTreeMap::new();
            for s in &signals {
                let p = s.get("protocol").and_then(Value::as_str).map(String::from).unwrap_or_else(|| "raw".into());
                *protos.entry(p).or_default() += 1;
            }
            let n = signals.len();
            signals.truncate(200);
            m.insert("signal_count".into(), json!(n));
            m.insert("signals".into(), Value::Array(signals));
            m.insert("protocols".into(), json!(protos));
            let plist: Vec<String> = protos.iter().map(|(p, c)| format!("{p} x{c}")).collect();
            // only a remote whose every signal is decoded has a stable identity
            let fpv = (!parsed_keys.is_empty() && parsed_keys.len() == n).then(|| {
                let mut parts = vec!["infrared"];
                parts.extend(parsed_keys.iter().map(String::as_str));
                fp(&parts)
            });
            (format!("Infrared remote, {n} signal(s): {}", plist.join(", ")), fpv)
        }
        "nfc" => {
            let dt = get("Device type").unwrap_or_default();
            let uid = get("UID").unwrap_or_default();
            m.insert("device_type".into(), json!(dt));
            if !uid.is_empty() {
                m.insert("uid".into(), json!(uid));
                m.insert("uid_bytes".into(), json!(uid.split_whitespace().count()));
            }
            for (k, name) in [
                ("ATQA", "atqa"),
                ("SAK", "sak"),
                ("Mifare Classic type", "mifare_classic_type"),
                ("NTAG/Ultralight type", "ultralight_type"),
            ] {
                if let Some(v) = get(k) {
                    m.insert(name.into(), json!(v));
                }
            }
            let blocks: Vec<&String> = f.fields.iter().filter(|(k, _)| k.starts_with("Block ")).map(|(_, v)| v).collect();
            let pages = f.fields.iter().filter(|(k, _)| k.starts_with("Page ")).count();
            if !blocks.is_empty() {
                let unread = blocks.iter().filter(|v| v.contains("??")).count();
                m.insert("blocks".into(), json!(blocks.len()));
                m.insert("blocks_unread".into(), json!(unread));
            }
            if pages > 0 {
                m.insert("pages".into(), json!(pages));
            }
            let fpv = (!uid.is_empty()).then(|| fp(&["nfc", &dt, &uid.to_ascii_uppercase()]));
            let u = if uid.is_empty() { String::new() } else { format!(" UID {uid}") };
            (format!("NFC {dt}{u}"), fpv)
        }
        "rfid" => {
            let kt = get("Key type").unwrap_or_default();
            let data = get("Data").unwrap_or_default();
            m.insert("key_type".into(), json!(kt));
            m.insert("data_bytes".into(), json!(data.split_whitespace().count()));
            let fpv = (!data.is_empty()).then(|| fp(&["rfid", &kt, &data]));
            (format!("125 kHz RFID {kt} key"), fpv)
        }
        _ => {
            let proto = get("Protocol").or_else(|| get("Key type")).unwrap_or_default();
            m.insert("protocol".into(), json!(proto));
            let data = get("Data").or_else(|| get("Rom Data")).unwrap_or_default();
            let fpv = (!data.is_empty()).then(|| fp(&["ibutton", &proto, &data]));
            (format!("iButton {proto} key"), fpv)
        }
    };
    Some(Described { kind: kind.to_string(), filetype: ft, fields: m, summary, fingerprint })
}

/// Is this card path one the library indexes (a capture folder, not under assets/.cache)?
pub fn is_capture_path(p: &str) -> bool {
    if p.split('/').any(|s| s == "assets" || s == ".cache" || s.starts_with('.')) {
        return false;
    }
    FOLDERS.iter().any(|(dir, ext)| p.starts_with(&format!("{dir}/")) && p.to_ascii_lowercase().ends_with(&format!(".{ext}")))
}

#[derive(Default)]
pub struct Builder {
    by_sha: BTreeMap<String, Capture>,
    skipped: Vec<String>,
}

impl Builder {
    pub fn add(&mut self, bytes: &[u8], loc: Location) {
        let sha = crate::sha256_hex(bytes);
        if let Some(c) = self.by_sha.get_mut(&sha) {
            if !c.locations.contains(&loc) {
                c.locations.push(loc);
            }
            return;
        }
        let file_name = loc.path.rsplit('/').next().unwrap_or("").to_string();
        let Some(Described { kind, filetype, fields, summary, fingerprint }) = describe(bytes) else {
            if self.skipped.len() < 1000 {
                self.skipped.push(format!("{} ({}): not a capture file Argus reads", loc.path, loc.source));
            }
            return;
        };
        let name = file_name.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or(file_name.clone());
        self.by_sha.insert(
            sha.clone(),
            Capture {
                id: sha[..16].to_string(),
                sha256: sha,
                kind,
                filetype,
                name: crate::display_text(name.as_bytes(), 120),
                size: bytes.len() as u64,
                summary,
                fields,
                fingerprint,
                locations: vec![loc],
                tags: vec![],
            },
        );
    }

    /// Every capture file in every snapshot of the store.
    pub fn add_snapshots(&mut self, snaps: &Snapshots) -> Result<()> {
        for s in snaps.list()? {
            for f in &s.files {
                if !is_capture_path(&f.path) || f.size as usize > flipperfile::MAX_FILE {
                    continue;
                }
                let Some(p) = snaps.store.object_path(&f.sha256) else { continue };
                if let Ok(bytes) = fs::read(p) {
                    self.add(&bytes, Location { origin: "snapshot".into(), source: s.id.clone(), path: f.path.clone() });
                }
            }
        }
        Ok(())
    }

    /// Every capture file in a local folder that mirrors the card (a pull: `<dir>/.../ext/nfc/x.nfc`).
    /// Symlinks are not followed. Each folder is read in name order (its files, then its folders one by one), so of
    /// identical files the first by name names the capture on every file system (read_dir's own order is the file system's).
    pub fn add_tree(&mut self, dir: &Path, origin: &str) -> Result<()> {
        let mut stack = vec![(dir.to_path_buf(), 0usize)];
        let mut seen = 0usize;
        while let Some((d, depth)) = stack.pop() {
            if depth > 24 {
                continue;
            }
            let Ok(rd) = fs::read_dir(&d) else { continue };
            let mut entries = Vec::new();
            let mut capped = false;
            for e in rd.flatten() {
                seen += 1;
                if seen > 200_000 {
                    capped = true;
                    break;
                }
                entries.push(e);
            }
            entries.sort_by_key(|e| e.file_name());
            let mut dirs = Vec::new();
            for e in entries {
                let Ok(ft) = e.file_type() else { continue };
                let p = e.path();
                if ft.is_dir() {
                    dirs.push((p, depth + 1));
                } else if ft.is_file() {
                    let rel = p.strip_prefix(dir).unwrap_or(&p).to_string_lossy().replace('\\', "/");
                    let card = match rel.find("ext/") {
                        Some(i) if i == 0 || rel[..i].ends_with('/') => format!("/{}", &rel[i..]),
                        _ => continue,
                    };
                    if !is_capture_path(&card) {
                        continue;
                    }
                    let mut bytes = Vec::new();
                    if let Ok(f) = fs::File::open(&p) {
                        let _ = f.take(flipperfile::MAX_FILE as u64 + 1).read_to_end(&mut bytes);
                    }
                    if bytes.len() > flipperfile::MAX_FILE {
                        continue;
                    }
                    let source = crate::display_text(dir.to_string_lossy().as_bytes(), 300);
                    self.add(&bytes, Location { origin: origin.into(), source, path: card });
                }
            }
            if capped {
                return Ok(());
            }
            // last pushed is read first: the folders in name order
            stack.extend(dirs.into_iter().rev());
        }
        Ok(())
    }

    pub fn finish(self, now: u64) -> Index {
        let mut captures: Vec<Capture> = self.by_sha.into_values().collect();
        for c in &mut captures {
            c.locations.sort();
        }
        captures.sort_by(|a, b| (&a.kind, &a.name, &a.sha256).cmp(&(&b.kind, &b.name, &b.sha256)));
        Index { format: FORMAT, built_unix: now, captures, skipped: self.skipped }
    }
}

/// The library folder: index.json (rebuilt from sources) and tags.json (the user's tags, by SHA-256).
pub struct Library {
    dir: PathBuf,
}

pub fn valid_tag(t: &str) -> bool {
    !t.is_empty() && t.len() <= 32 && t.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || " _-.".contains(c))
}

impl Library {
    pub fn open(dir: &Path) -> Result<Library> {
        store::mkdir_private(dir)?;
        Ok(Library { dir: dir.to_path_buf() })
    }

    pub fn save_index(&self, idx: &Index) -> Result<()> {
        store::write_atomic(&self.dir.join("index.json"), &serde_json::to_vec(idx)?)
    }

    pub fn tags(&self) -> Result<BTreeMap<String, Vec<String>>> {
        match fs::read(self.dir.join("tags.json")) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(_) => Ok(BTreeMap::new()),
        }
    }

    /// The index with the user's tags filled in.
    pub fn load(&self) -> Result<Index> {
        let raw = fs::read(self.dir.join("index.json"))
            .map_err(|_| Error::new("no_index", "the capture library has not been built yet (capture index)"))?;
        let mut idx: Index = serde_json::from_slice(&raw)?;
        let tags = self.tags()?;
        for c in &mut idx.captures {
            c.tags = tags.get(&c.sha256).cloned().unwrap_or_default();
        }
        Ok(idx)
    }

    pub fn find<'a>(&self, idx: &'a Index, id: &str) -> Result<&'a Capture> {
        let id = id.to_ascii_lowercase();
        if id.len() < 8 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::new("bad_id", "a capture id is at least 8 hex digits of its SHA-256"));
        }
        let hits: Vec<&Capture> = idx.captures.iter().filter(|c| c.sha256.starts_with(&id)).collect();
        match hits.len() {
            1 => Ok(hits[0]),
            0 => Err(Error::new("no_capture", "no capture has that id")),
            _ => Err(Error::new("ambiguous_id", "several captures start with that id; give more digits")),
        }
    }

    pub fn tag(&self, sha: &str, add: &[String], remove: &[String]) -> Result<Vec<String>> {
        if !store::valid_sha256(sha) {
            return Err(Error::new("bad_id", "tags are set by full SHA-256"));
        }
        let mut all = self.tags()?;
        let mut set: BTreeSet<String> = all.remove(sha).unwrap_or_default().into_iter().collect();
        for t in add {
            let t = t.trim().to_ascii_lowercase();
            if !valid_tag(&t) {
                return Err(Error::new(
                    "bad_tag",
                    format!("'{}' is not a tag: up to 32 of a-z 0-9 space _ - .", crate::display_text(t.as_bytes(), 40)),
                ));
            }
            set.insert(t);
        }
        for t in remove {
            set.remove(&t.trim().to_ascii_lowercase());
        }
        let out: Vec<String> = set.into_iter().collect();
        if !out.is_empty() {
            all.insert(sha.to_string(), out.clone());
        }
        store::write_atomic(&self.dir.join("tags.json"), &serde_json::to_vec_pretty(&all)?)?;
        Ok(out)
    }
}

fn haystack(c: &Capture) -> String {
    let mut h = vec![c.kind.clone(), c.filetype.clone(), c.name.clone(), c.summary.clone(), c.id.clone()];
    h.extend(c.locations.iter().map(|l| l.path.clone()));
    h.extend(c.tags.iter().cloned());
    for (k, v) in &c.fields {
        if k == "signals" {
            if let Some(a) = v.as_array() {
                for s in a {
                    if let Some(n) = s.get("name").and_then(Value::as_str) {
                        h.push(n.to_string());
                    }
                }
            }
            continue;
        }
        h.push(match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        });
    }
    h.join("\n").to_ascii_lowercase()
}

/// Search: every whitespace-separated term must match. A term is a substring, or `kind:`, `tag:`,
/// `proto:`, `freq:` (MHz, prefix), `uid:` (spaces ignored).
pub fn search<'a>(idx: &'a Index, query: &str, limit: usize) -> Vec<&'a Capture> {
    let terms: Vec<String> = query.split_whitespace().map(|t| t.to_ascii_lowercase()).collect();
    idx.captures
        .iter()
        .filter(|c| {
            let hay = haystack(c);
            terms.iter().all(|t| match t.split_once(':') {
                Some(("kind", v)) => c.kind.starts_with(v),
                Some(("tag", v)) => c.tags.iter().any(|x| x == v),
                Some(("proto", v)) => {
                    c.fields.get("protocol").and_then(Value::as_str).is_some_and(|p| p.to_ascii_lowercase().contains(v))
                        || c.fields
                            .get("protocols")
                            .and_then(Value::as_object)
                            .is_some_and(|o| o.keys().any(|k| k.to_ascii_lowercase().contains(v)))
                }
                Some(("freq", v)) => c.fields.get("frequency_mhz").and_then(Value::as_str).is_some_and(|f| f.starts_with(v)),
                Some(("uid", v)) => c
                    .fields
                    .get("uid")
                    .and_then(Value::as_str)
                    .is_some_and(|u| u.replace(' ', "").to_ascii_lowercase().contains(&v.replace(' ', ""))),
                _ => hay.contains(t.as_str()),
            })
        })
        .take(limit)
        .collect()
}

#[derive(Debug, Serialize)]
pub struct DupeGroup {
    /// "same_file": identical bytes found under different names or folders.
    /// "same_signal": different files with the same decoded content (same key, UID, IR codes).
    pub kind: &'static str,
    pub captures: Vec<String>,
    pub paths: Vec<String>,
}

pub fn duplicates(idx: &Index) -> Vec<DupeGroup> {
    let mut out = Vec::new();
    for c in &idx.captures {
        let paths: BTreeSet<&str> = c.locations.iter().map(|l| l.path.as_str()).collect();
        if paths.len() > 1 {
            out.push(DupeGroup { kind: "same_file", captures: vec![c.id.clone()], paths: paths.into_iter().map(String::from).collect() });
        }
    }
    let mut by_fp: BTreeMap<&str, Vec<&Capture>> = BTreeMap::new();
    for c in &idx.captures {
        if let Some(f) = &c.fingerprint {
            by_fp.entry(f.as_str()).or_default().push(c);
        }
    }
    for (_, group) in by_fp {
        if group.len() > 1 {
            let paths: BTreeSet<String> = group.iter().flat_map(|c| c.locations.iter().map(|l| l.path.clone())).collect();
            out.push(DupeGroup {
                kind: "same_signal",
                captures: group.iter().map(|c| c.id.clone()).collect(),
                paths: paths.into_iter().collect(),
            });
        }
    }
    out
}

/// A few plain sentences about a capture, for people and for Sarina. Built from header facts only.
pub fn explain(c: &Capture) -> String {
    let f = |k: &str| c.fields.get(k);
    let s = |k: &str| f(k).and_then(Value::as_str).unwrap_or("").to_string();
    let n = |k: &str| f(k).and_then(Value::as_u64);
    let mut out = Vec::new();
    match c.kind.as_str() {
        "subghz" => {
            out.push(format!(
                "A Sub-GHz key file saved by the Flipper's Sub-GHz app: the {} protocol{} at {} MHz{}.",
                s("protocol"),
                n("bits").map(|b| format!(", {b} bits")).unwrap_or_default(),
                s("frequency_mhz"),
                if s("preset").is_empty() { String::new() } else { format!(", {} modulation preset", s("preset")) }
            ));
            if s("code_type") == "rolling" {
                out.push(
                    "It is a rolling-code protocol: the code changes on every press, so a receiver normally rejects a replayed copy."
                        .into(),
                );
            } else {
                out.push("It is a fixed-code protocol: every press sends the same code, so the file is the whole signal.".into());
            }
        }
        "subghz_raw" => {
            out.push(format!(
                "A raw Sub-GHz recording at {} MHz{}: {} on/off timings over about {} ms, not decoded into a protocol.",
                s("frequency_mhz"),
                if s("preset").is_empty() { String::new() } else { format!(" ({} preset)", s("preset")) },
                n("raw_samples").unwrap_or(0),
                n("raw_duration_ms").unwrap_or(0)
            ));
        }
        "infrared" => {
            let protos = f("protocols")
                .and_then(Value::as_object)
                .map(|o| o.iter().map(|(k, v)| format!("{k} ({v})")).collect::<Vec<_>>().join(", "));
            out.push(format!(
                "An infrared remote file with {} signal(s); protocols: {}.",
                n("signal_count").unwrap_or(0),
                protos.unwrap_or_default()
            ));
            if let Some(names) = f("signals").and_then(Value::as_array) {
                let list: Vec<&str> = names.iter().filter_map(|x| x.get("name").and_then(Value::as_str)).take(12).collect();
                if !list.is_empty() {
                    out.push(format!("Buttons: {}.", list.join(", ")));
                }
            }
        }
        "nfc" => {
            out.push(format!(
                "An NFC card or tag saved by the NFC app: {}{}.",
                s("device_type"),
                if s("uid").is_empty() { String::new() } else { format!(", UID {} ({} bytes)", s("uid"), n("uid_bytes").unwrap_or(0)) }
            ));
            if !s("mifare_classic_type").is_empty() {
                out.push(format!(
                    "MIFARE Classic {}: {} blocks saved, {} of them unread (sector keys not found).",
                    s("mifare_classic_type"),
                    n("blocks").unwrap_or(0),
                    n("blocks_unread").unwrap_or(0)
                ));
            } else if let Some(p) = n("pages") {
                out.push(format!("{p} memory pages saved."));
            }
            if !s("atqa").is_empty() || !s("sak").is_empty() {
                out.push(format!("ATQA {}, SAK {}.", s("atqa"), s("sak")));
            }
        }
        "rfid" => out.push(format!("A 125 kHz RFID key ({}), {} bytes of ID data.", s("key_type"), n("data_bytes").unwrap_or(0))),
        _ => out.push(format!("An iButton key ({}).", s("protocol"))),
    }
    let places: BTreeSet<&str> = c.locations.iter().map(|l| l.path.as_str()).collect();
    out.push(format!(
        "Seen as {} in {} place(s){}.",
        places.iter().take(3).copied().collect::<Vec<_>>().join(", "),
        c.locations.len(),
        if c.tags.is_empty() { String::new() } else { format!("; tags: {}", c.tags.join(", ")) }
    ));
    out.join(" ")
}
