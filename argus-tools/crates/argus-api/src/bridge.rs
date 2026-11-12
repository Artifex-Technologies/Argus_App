//! The command side of the Sarina API: the port of `Chymaera::Bridge` (backend/chymaera/
//! chymaerabridge.cpp) without a device attached, as `qFlipper-cli chymaera serve` runs it. Every
//! answer, event and datastore row is the C++ app's; `tests/api.rs` replays requests recorded from
//! the C++ server.

use std::path::{Component, Path, PathBuf};

use argus_capture::meta;
use argus_capture::store::{Capture, CaptureRow, Store};
use argus_capture::sync::{is_capture_root, CAPTURE_ROOTS};

use crate::device::{Device, Note};
use crate::eventlog::{now_ms, severity_from_name, Entry, EventLog, Severity, Source};
use crate::qjson::{self, qs, Object, QString, Value};

/// The app's version (the `VERSION` file, as `APP_VERSION` in the C++ build).
pub const VERSION: &str = include_str!("../../../../VERSION").trim_ascii();

/// File kinds `capture.import` takes over the API: the Flipper's capture formats and packet
/// captures. Anything else could be one of the owner's own files read for a caller who only holds
/// the token (the generic parser hands back every `Key: Value` line of a file).
pub const API_IMPORT_SUFFIXES: &[&str] = &["nfc", "sub", "rfid", "ir", "ibtn", "pcap", "pcapng"];

/// The commands, in `help`'s order.
pub const COMMANDS: &[&str] = &[
    "ping",
    "status",
    "help",
    "log.tail",
    "log.add",
    "session.start",
    "session.end",
    "capture.list",
    "capture.import",
    "device.status",
    "capture.pull",
    "capture.pullAll",
    "capture.library",
    "capture.explain",
];

type EntryHook = Box<dyn FnMut(&Entry) + Send>;

pub struct Bridge {
    store: Option<Store>,
    store_path: String,
    log: EventLog,
    session: Option<(i64, String)>,
    /// The listening port, 0 when the server is not running.
    pub server_port: u16,
    /// Where argus-tools keeps the capture library (`$XDG_DATA_HOME/argus/captures`).
    library_dir: PathBuf,
    /// Called for each new entry (the CLI mirrors them to stdout).
    on_entry: Option<EntryHook>,
    /// The Flipper this server reads from, when one was named.
    device: Option<Device>,
}

/// `$XDG_DATA_HOME`, or `~/.local/share` (Qt's GenericDataLocation).
pub fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
}

fn ok() -> Object {
    Object::new().with("ok", Value::Bool(true))
}

fn fail(error: impl Into<Value>) -> Object {
    Object::new().with("ok", Value::Bool(false)).with("error", error.into())
}

fn opt_str(v: &Option<String>) -> Value {
    match v {
        Some(s) => Value::from(s.as_str()),
        None => Value::Null,
    }
}

fn opt_int(v: Option<i64>) -> Value {
    v.map_or(Value::Null, Value::Int)
}

/// One `capture.list` row. SQL NULL (no session; `sha256`/`local_path` of rows from before v1.2)
/// is JSON null, as docs/SARINA_API.md says.
pub fn row_json(r: &CaptureRow) -> Value {
    Object::new()
        .with("id", Value::Int(r.id))
        .with("session_id", opt_int(r.session_id))
        .with("ts", Value::Int(r.ts))
        .with("type", Value::from(r.kind.as_str()))
        .with("name", opt_str(&r.name))
        .with("source_path", opt_str(&r.source_path))
        .with("size", opt_int(r.size))
        .with("meta", opt_str(&r.meta))
        .with("sha256", opt_str(&r.sha256))
        .with("local_path", opt_str(&r.local_path))
        .into()
}

/// SHA-256 and size of a regular file, read in pieces (`sha256OfFile`); `None` for anything else.
fn hash_file(path: &str) -> Option<(String, u64)> {
    use sha2::{Digest, Sha256};
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut f = std::fs::File::open(path).ok()?;
    let mut hash = Sha256::new();
    let size = std::io::copy(&mut f, &mut hash).ok()?;
    Some((hex::encode(hash.finalize()), size))
}

/// `QFileInfo::fileName()`.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `QFileInfo::absoluteFilePath()`: absolute, `.` and `..` resolved by name (not through links).
fn absolute(path: &str) -> String {
    let p = Path::new(path);
    let full = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(p) };
    let mut out: Vec<String> = Vec::new();
    for c in full.components() {
        match c {
            Component::Normal(s) => out.push(s.to_string_lossy().into_owned()),
            Component::ParentDir => {
                out.pop();
            }
            _ => {}
        }
    }
    format!("/{}", out.join("/"))
}

/// The lower-cased suffix after the last dot of the file name (`QFileInfo::suffix().toLower()`).
fn suffix(name: &str) -> String {
    name.rsplit_once('.').map(|(_, s)| s.to_lowercase()).unwrap_or_default()
}

/// `QString::toUtf8()` as a process argument: cut at a NUL (the C++ app hands it to argus-tools
/// through `QProcess`).
fn as_argument(s: &[u16]) -> String {
    let bytes = qjson::to_utf8(s);
    let cut = bytes.split(|&b| b == 0).next().unwrap_or_default();
    String::from_utf8_lossy(cut).into_owned()
}

/// `slimCapture` (backend/argus/arguscontroller.cpp): header facts, tags and where a capture was
/// seen; never a local path.
fn slim_capture(c: &Object) -> Value {
    let get = |o: &Object, k: &str| o.get(k).cloned();
    let mut paths = Vec::new();
    if let Some(Value::Array(locations)) = c.get("locations") {
        for l in locations {
            let empty = Object::new();
            let o = l.as_object().unwrap_or(&empty);
            let snapshot = o.get("origin").map(Value::to_text).as_deref() == Some("snapshot");
            paths.push(Value::Object(
                Object::new()
                    .with("origin", get(o, "origin"))
                    .with("source", if snapshot { get(o, "source") } else { Some(Value::from("card")) })
                    .with("path", get(o, "path")),
            ));
        }
    }
    let fields = c.get("fields").and_then(Value::as_object).cloned().unwrap_or_default();
    Object::new()
        .with("id", get(c, "id"))
        .with("sha256", get(c, "sha256"))
        .with("kind", get(c, "kind"))
        .with("name", get(c, "name"))
        .with("summary", get(c, "summary"))
        .with("fields", Value::Object(fields))
        .with("tags", get(c, "tags"))
        .with("locations", Value::Array(paths))
        .into()
}

impl Bridge {
    /// Opens the datastore at `datastore` (the C++ app's `defaultDatastorePath` unless a test
    /// names another) and logs the start, as the C++ constructor.
    pub fn new(datastore: &Path, library_dir: PathBuf) -> Bridge {
        let store = Store::open(datastore).ok();
        let store_path = if store.is_some() { datastore.display().to_string() } else { String::new() };
        let mut b = Bridge {
            store,
            store_path,
            log: EventLog::default(),
            session: None,
            server_port: 0,
            library_dir,
            on_entry: None,
            device: None,
        };
        let detail = if b.store.is_some() { format!("datastore: {}", b.store_path) } else { "datastore unavailable".into() };
        b.append(Severity::Info, Source::System, &format!("Chymaera subsystem {VERSION} ready"), &detail);
        b
    }

    /// The shared datastore and capture library under the data home.
    pub fn open_default() -> Option<Bridge> {
        let home = data_home()?;
        Some(Bridge::new(&home.join("Argus/chymaera/chymaera.db"), home.join("argus/captures")))
    }

    pub fn on_entry(&mut self, f: impl FnMut(&Entry) + Send + 'static) {
        self.on_entry = Some(Box::new(f));
    }

    pub fn datastore_path(&self) -> &str {
        &self.store_path
    }

    pub fn event_log(&self) -> &EventLog {
        &self.log
    }

    /// `ChymaeraEventLog::append`: the datastore first, then the ring.
    pub fn append(&mut self, severity: Severity, source: Source, message: &str, detail: &str) {
        self.append_q(severity, source, qs(message), qs(detail));
    }

    fn append_q(&mut self, severity: Severity, source: Source, message: QString, detail: QString) {
        if let Some(store) = &self.store {
            let session = self.session.as_ref().map(|s| s.0);
            let _ = store.log_event_in(session, severity, source, &String::from_utf16_lossy(&message), &String::from_utf16_lossy(&detail));
        }
        let e = Entry { epoch_ms: now_ms(), severity, source, message, detail };
        if let Some(f) = &mut self.on_entry {
            f(&e);
        }
        self.log.push(e);
    }

    pub fn start_session(&mut self, name: &str, notes: &str) -> bool {
        let id = self.store.as_ref().and_then(|s| s.start_session(name, notes).ok());
        match id {
            Some(id) => {
                self.session = Some((id, name.to_string()));
                self.append(Severity::Notice, Source::System, &format!("Session started: {name} (#{id})"), "");
                true
            }
            None => {
                self.append(Severity::Error, Source::System, &format!("Failed to start session '{name}'"), "");
                false
            }
        }
    }

    pub fn end_session(&mut self) {
        let Some((id, name)) = self.session.take() else { return };
        if let Some(store) = &self.store {
            let _ = store.end_session(id);
        }
        self.append(Severity::Notice, Source::System, &format!("Session ended: {name}"), "");
    }

    /// `recordCapture`: hashes the file, records it unless this content from this path is there
    /// already, and returns the summary (empty when it could not be read or was a duplicate).
    /// Only a regular file is read, and a file over `meta::MAX_PARSE` is kept unparsed.
    fn record_capture(&mut self, path: &str, display_name: &str, origin: &str) -> (String, bool) {
        let Some((sha256, size)) = hash_file(path) else {
            self.append(Severity::Warning, Source::Capture, &format!("Cannot read capture: {display_name}"), "");
            return (String::new(), false);
        };
        let Some(store) = &self.store else { return (String::new(), false) };
        if store.has_capture(&sha256, origin).unwrap_or(false) {
            return (String::new(), true);
        }
        let data = if size <= meta::MAX_PARSE as u64 { std::fs::read(path).ok() } else { None };
        let (kind, meta_json) = if let Some(data) = &data {
            meta::describe(display_name, data)
        } else {
            let k = meta::raw_kind(display_name);
            (k.to_string(), format!(r#"{{"kind":"{k}"}}"#))
        };
        let summary = meta::summary(display_name, size, &meta_json);
        let c = Capture {
            kind,
            name: display_name.to_string(),
            source_path: origin.to_string(),
            size,
            meta: meta_json,
            sha256,
            local_path: absolute(path),
        };
        let session = self.session.as_ref().map(|s| s.0);
        if store.add_capture_in(session, &c).is_err() {
            return (String::new(), false);
        }
        self.append(Severity::Info, Source::Capture, &format!("Recorded capture: {summary}"), origin);
        (summary, false)
    }

    /// `importCapture`: the summary, `Already recorded: NAME`, or empty on failure.
    pub fn import_capture(&mut self, path: &str) -> String {
        let name = file_name(path).to_string();
        let (summary, duplicate) = self.record_capture(path, &name, path);
        if duplicate {
            format!("Already recorded: {name}")
        } else {
            summary
        }
    }

    /// `capture.library` / `capture.explain` through argus-tools' own code, answering as the C++
    /// app does after running `argus-tools capture search|explain`.
    fn library(&self, explain: Option<&str>, query: &str, limit: i32) -> Object {
        let run = || -> argus_tools::Result<serde_json::Value> {
            let lib = argus_tools::capture::Library::open(&self.library_dir)?;
            let idx = lib.load()?;
            Ok(match explain {
                Some(id) => {
                    let c = lib.find(&idx, id)?;
                    serde_json::json!({"ok": true, "capture": c, "explanation": argus_tools::capture::explain(c)})
                }
                None => {
                    let hits = argus_tools::capture::search(&idx, query, limit.clamp(1, 500) as usize);
                    serde_json::json!({"ok": true, "total": idx.captures.len(), "captures": hits})
                }
            })
        };
        let r = match run() {
            Ok(v) => qjson::from_serde(&v),
            Err(e) => return fail(e.message),
        };
        let Value::Object(r) = r else { return fail("") };
        match explain {
            Some(_) => {
                let empty = Object::new();
                Object::new()
                    .with("ok", Value::Bool(true))
                    .with("capture", slim_capture(r.get("capture").and_then(Value::as_object).unwrap_or(&empty)))
                    .with("explanation", r.get("explanation").cloned())
            }
            None => {
                let captures = match r.get("captures") {
                    Some(Value::Array(a)) => a.iter().map(|c| slim_capture(c.as_object().unwrap_or(&Object::new()))).collect(),
                    _ => Vec::new(),
                };
                Object::new().with("ok", Value::Bool(true)).with("total", r.get("total").cloned()).with("captures", Value::Array(captures))
            }
        }
    }

    /// Logs an authorised request, as the server's `requestReceived` signal does.
    pub fn note_request(&mut self, cmd: &[u16]) {
        let mut m = qs("API request: ");
        m.extend_from_slice(cmd);
        self.append_q(Severity::Debug, Source::Sarina, m, QString::new());
    }

    /// `startPull` with no device attached (`serve` has none): logged and refused.
    /// `startPull`: refused (and logged) without a device or while a pull runs; otherwise the
    /// pull starts on the device's worker and this returns at once, as in the C++ app.
    fn start_pull(&mut self, roots: Vec<String>, label: &str) -> bool {
        let session = self.session.as_ref().map(|s| s.0);
        let why = match &mut self.device {
            None => "no device attached",
            Some(d) if d.is_pulling() => "a pull is already running",
            Some(d) => {
                if d.start_pull(roots, label.to_string(), session) {
                    self.append(Severity::Notice, Source::Device, &format!("Pulling {label} from device…"), "");
                    return true;
                }
                "the device is gone"
            }
        };
        self.append(Severity::Warning, Source::Device, &format!("Cannot pull {label}: {why}"), "");
        false
    }

    /// `attachDevice`.
    pub fn attach_device(&mut self, device: Device) {
        let name = device.name().to_string();
        self.device = Some(device);
        self.append(Severity::Notice, Source::Device, &format!("Device attached: {name}"), "");
    }

    pub fn has_device(&self) -> bool {
        self.device.is_some()
    }

    /// Takes in what the device's worker reported: pull steps, the end of a pull, a lost device.
    pub fn poll_device(&mut self) {
        let Some(d) = &mut self.device else { return };
        let notes = d.notes();
        for n in notes {
            match n {
                Note::Event(sev, src, msg, detail) => self.append(sev, src, &msg, &detail),
                Note::PullDone(_) => {}
                Note::Gone(why) => {
                    self.device = None;
                    self.append(Severity::Notice, Source::Device, "Device offline", &why);
                    self.append(Severity::Notice, Source::Device, "Device detached", "");
                }
            }
        }
    }

    /// `Bridge::handleApiRequest`.
    pub fn handle(&mut self, request: &Object) -> Object {
        let field = |k: &str| request.get(k).cloned().unwrap_or(Value::Null);
        let cmd = field("cmd").to_qstring();
        match String::from_utf16_lossy(&cmd).as_str() {
            "ping" => return ok().with("pong", Value::Bool(true)).with("version", Value::from(VERSION)),
            "help" => return ok().with("commands", Value::Array(COMMANDS.iter().map(|c| Value::from(*c)).collect())),
            "status" => {
                let count = self.store.as_ref().and_then(|s| s.event_count().ok()).unwrap_or(0);
                return ok()
                    .with("serverRunning", Value::Bool(self.server_port != 0))
                    .with("serverPort", Value::Int(i64::from(self.server_port)))
                    .with("sessionActive", Value::Bool(self.session.is_some()))
                    .with("sessionName", Value::from(self.session.as_ref().map(|s| s.1.clone()).unwrap_or_default()))
                    .with("datastorePath", Value::from(self.store_path.clone()))
                    .with("eventCount", Value::Int(count.min(i64::from(i32::MAX))))
                    .with("version", Value::from(VERSION));
            }
            "log.tail" => {
                let limit = field("limit").to_int(50);
                let entries = self.log.tail(limit).into_iter().map(Entry::to_json).collect();
                return ok().with("entries", Value::Array(entries));
            }
            "log.add" => {
                let message = field("message").to_qstring();
                if message.is_empty() {
                    return fail("message is required");
                }
                let severity = severity_from_name(&field("severity").to_text(), Severity::Info);
                self.append_q(severity, Source::Sarina, message, QString::new());
                return ok();
            }
            "session.start" => {
                let name = field("name").to_text();
                if name.is_empty() {
                    return fail("name is required");
                }
                if !self.start_session(&name, &field("notes").to_text()) {
                    return fail("could not start session");
                }
                let id = self.session.as_ref().map_or(-1, |s| s.0);
                return ok().with("sessionId", qjson::double_value(id as f64));
            }
            "session.end" => {
                self.end_session();
                return ok();
            }
            "capture.list" => {
                let limit = field("limit").to_int(50);
                let rows = self.store.as_ref().and_then(|s| s.capture_rows(i64::from(limit)).ok()).unwrap_or_default();
                return ok().with("captures", Value::Array(rows.iter().map(row_json).collect()));
            }
            "capture.import" => {
                let path = field("path").to_text();
                if path.is_empty() {
                    return fail("path is required");
                }
                if !API_IMPORT_SUFFIXES.contains(&suffix(file_name(&path)).as_str()) {
                    let list: Vec<String> = API_IMPORT_SUFFIXES.iter().map(|s| format!(".{s}")).collect();
                    return fail(format!("only capture files may be imported: {}", list.join(", ")));
                }
                let summary = self.import_capture(&path);
                if summary.is_empty() {
                    return fail("could not parse capture");
                }
                return ok().with("summary", Value::from(summary));
            }
            "device.status" => {
                let name = self.device.as_ref().map(|d| d.name().to_string()).unwrap_or_default();
                return ok().with("connected", Value::Bool(self.device.is_some())).with("name", Value::from(name));
            }
            "capture.pull" => {
                let path = field("path").to_text();
                if path.is_empty() {
                    return fail("path is required");
                }
                if !is_capture_root(&path) {
                    return fail(format!("only the capture folders may be pulled: {}", CAPTURE_ROOTS.join(", ")));
                }
                if !self.start_pull(vec![path.clone()], &path) {
                    return fail("no device attached or staging failed");
                }
                // Asynchronous: log.tail shows the progress and the end.
                return ok().with("started", Value::Bool(true)).with("path", Value::from(path));
            }
            "capture.pullAll" => {
                let roots: Vec<String> = CAPTURE_ROOTS.iter().map(|r| r.to_string()).collect();
                if !self.start_pull(roots, "all capture folders") {
                    return fail("no device attached, a pull is already running, or staging failed");
                }
                let paths = CAPTURE_ROOTS.iter().map(|r| Value::from(*r)).collect();
                return ok().with("started", Value::Bool(true)).with("paths", Value::Array(paths));
            }
            "capture.library" => {
                let query = as_argument(&field("query").to_qstring());
                return self.library(None, &query, field("limit").to_int(50));
            }
            "capture.explain" => {
                let id = field("id").to_qstring();
                if id.is_empty() {
                    return fail("id is required");
                }
                return self.library(Some(&as_argument(&id)), "", 0);
            }
            _ => {}
        }
        let mut message = qs("unknown command: ");
        message.extend_from_slice(&cmd);
        fail(Value::Str(message))
    }
}
