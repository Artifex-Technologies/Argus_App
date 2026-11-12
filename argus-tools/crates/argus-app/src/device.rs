//! One connected Flipper: a worker thread owns its RPC session (`flipper_rpc::Session`) and does what the UI asks, one
//! command at a time, as the C++ app's operation queue does. Between commands it keeps the screen stream open while the
//! UI wants it, and otherwise asks the protobuf version once a second to notice an unplugged device (the C++ app learns
//! it from USB hot-plug). Everything the worker learns goes back to the UI as an [`Event`], followed by a repaint
//! request, so an idle window does no work.
//!
//! The worker only ever opens the device it was handed ([`crate::discovery::Found`]); it never looks for one. Commands
//! that change the device are sent only after the UI's confirmation dialog was answered (see `app.rs`); the worker
//! itself does not ask.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use argus_api::bridge::Bridge;
use flipper_rpc::ops::{Entry, InfoSource};
use flipper_rpc::proto::pb_gui::{InputKey, InputType};
use flipper_rpc::screen::Frame;
use flipper_rpc::transport::{SerialTransport, Transport};
use flipper_rpc::{RpcError, Session};

use crate::discovery::{Found, Kind};

/// After a reboot, factory reset or update the device leaves; this long is waited for it to go.
const GONE_WAIT: Duration = Duration::from_secs(5);
/// How long one poll for a screen frame waits before the worker looks at its commands again.
const FRAME_POLL: Duration = Duration::from_millis(40);
/// The idle keepalive.
const KEEPALIVE: Duration = Duration::from_secs(1);

/// What the app shows of a device: the C++ `DeviceInfo` as `VCPDeviceInfoHelper` fills it (without setting the clock,
/// which the C++ app does on every connection).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub firmware_version: String,
    pub firmware_commit: String,
    pub firmware_branch: String,
    /// `dd-MM-yyyy`, as the device reports it.
    pub build_date: String,
    /// `firmware.origin.fork`: "Official", or a fork such as "Argus".
    pub fork: String,
    /// "12.f7b9c6" (version, target, body, connect), as the Qt app shows it.
    pub hardware: String,
    pub region: u32,
    /// "1.17.3", empty when the radio is not alive.
    pub radio_version: String,
    pub stack_type: i32,
    pub sd_present: bool,
    /// Percent free on the SD card (rounded down, as the C++ app).
    pub sd_free: u64,
    pub assets_installed: bool,
    pub protobuf: (u32, u32),
    /// Every key the device reported, in its order.
    pub pairs: Vec<(String, String)>,
}

impl DeviceInfo {
    fn get(&self, dotted: &str) -> String {
        let legacy = dotted.replace('.', "_");
        self.pairs.iter().find(|(k, _)| *k == dotted || *k == legacy).map(|(_, v)| v.clone()).unwrap_or_default()
    }

    /// Builds the information from the device's pairs (`devinfo`, or the old request's underscore keys).
    pub fn from_pairs(pairs: Vec<(String, String)>, source: InfoSource, protobuf: (u32, u32)) -> DeviceInfo {
        let mut d = DeviceInfo { pairs, protobuf, ..DeviceInfo::default() };
        d.name = d.get("hardware.name");
        d.firmware_version = d.get("firmware.version");
        d.firmware_commit = d.get("firmware.commit.hash");
        d.firmware_branch = match source {
            InfoSource::Property => d.get("firmware.branch.name"),
            _ => d.get("firmware.branch"),
        };
        d.build_date = d.get("firmware.build.date");
        d.fork = d.get("firmware.origin.fork");
        d.hardware =
            format!("{}.f{}b{}c{}", d.get("hardware.ver"), d.get("hardware.target"), d.get("hardware.body"), d.get("hardware.connect"));
        d.region = flipper_rpc::assets::builtin_region(&d.pairs).or_else(|| d.get("hardware.region").parse().ok()).unwrap_or(0);
        if d.get("radio.alive") == "true" {
            d.radio_version = format!("{}.{}.{}", d.get("radio.stack.major"), d.get("radio.stack.minor"), d.get("radio.stack.sub"));
            d.stack_type = d.get("radio.stack.type").parse().unwrap_or(0);
        }
        d
    }

    /// The firmware line of the info pane: the commit on the dev branch, else the version.
    pub fn firmware_shown(&self) -> &str {
        if self.firmware_branch == "dev" {
            &self.firmware_commit
        } else {
            &self.firmware_version
        }
    }
}

/// The C++ helper's steps without the clock: protobuf version, device information, SD card space, `/ext/Manifest`.
pub fn read_info<T: Transport>(s: &mut Session<T>) -> Result<DeviceInfo, RpcError> {
    let version = s.protobuf_version()?;
    let (source, pairs) = s.device_info(version)?;
    let mut d = DeviceInfo::from_pairs(pairs, source, version);
    if d.name.is_empty() {
        return Err(RpcError::Unexpected("the device information names no device".into()));
    }
    match s.storage_info("/ext")? {
        Some(space) if space.total > 0 => {
            d.sd_present = true;
            d.sd_free = space.free.saturating_mul(100) / space.total;
            d.assets_installed = s.storage_stat("/ext/Manifest")?.is_some();
        }
        _ => {}
    }
    Ok(d)
}

/// What the busy screen says, by operation (the Qt `UpdateOverlay` titles).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Backup,
    Restore,
    Erase,
    Update,
    Pull,
    Push,
    Mkdir,
    Remove,
    Rename,
    Captures,
    Repair,
}

impl Op {
    pub fn title(self) -> &'static str {
        match self {
            Op::Backup => "Creating Backup",
            Op::Restore => "Restoring Backup",
            Op::Erase => "Performing Factory Reset",
            Op::Update => "Updating your device",
            Op::Repair => "Repairing your device",
            Op::Pull | Op::Push | Op::Mkdir | Op::Remove | Op::Rename => "File operation in progress",
            Op::Captures => "Pulling captures",
        }
    }

    /// Operations that take the whole window (the Qt app's update overlay); file operations and capture pulls do not.
    pub fn is_big(self) -> bool {
        matches!(self, Op::Backup | Op::Restore | Op::Erase | Op::Update | Op::Repair)
    }
}

/// An update, ready to go: the package (already read and checked) and the region data (fetched before, or why not).
pub struct UpdateJob {
    pub package: flipper_rpc::update::UpdatePackage,
    /// `None`: the device is of the development region and needs none.
    pub region: Option<Result<Vec<u8>, String>>,
}

pub enum Cmd {
    Refresh,
    List(String),
    /// The UI wants the screen stream on or off.
    Screen(bool),
    Input(InputKey, InputType),
    Pull {
        remote: String,
        local: PathBuf,
    },
    Push {
        local: PathBuf,
        remote_dir: String,
    },
    Mkdir(String),
    Remove {
        path: String,
        recursive: bool,
    },
    Rename {
        from: String,
        to: String,
    },
    Backup(PathBuf),
    Restore(PathBuf),
    Erase,
    Update(Box<UpdateJob>),
    /// Capture pull into the shared datastore, logged through the console's bridge.
    Captures {
        roots: Vec<String>,
        label: String,
    },
    Disconnect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub summary: String,
    /// A partial success (`DeviceState::warningString`).
    pub warning: Option<String>,
}

pub enum Event {
    Connected(DeviceInfo),
    Info(DeviceInfo),
    Listing {
        path: String,
        result: Result<Vec<Entry>, String>,
    },
    Frame(Frame),
    Done {
        op: Op,
        result: Result<Outcome, String>,
    },
    /// The session ended: the device left (reboot, update, unplug) or broke.
    Gone(String),
    /// The device could not be opened or did not answer the handshake.
    Failed(String),
}

pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// The UI's end of a worker.
pub struct Worker {
    tx: Sender<Cmd>,
    pub rx: Receiver<Event>,
    join: Option<JoinHandle<()>>,
}

impl Worker {
    /// Opens `found` on a new thread and starts its session. `bridge` is the console's, for capture pulls.
    pub fn spawn(found: Found, timeout: Duration, bridge: Arc<Mutex<Bridge>>, waker: Waker) -> Worker {
        let (tx, crx) = mpsc::channel();
        let (etx, rx) = mpsc::channel();
        let join = std::thread::Builder::new()
            .name("argus-device".into())
            .spawn(move || run(found, timeout, crx, etx, bridge, waker))
            .expect("spawn the device worker");
        Worker { tx, rx, join: Some(join) }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Disconnect);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Opens the transport `found` names: a serial port (checked to be a character device first) or a Unix socket.
pub fn open(found: &Found, timeout: Duration) -> Result<Box<dyn Transport>, String> {
    use std::os::unix::fs::FileTypeExt;
    match &found.kind {
        Kind::Serial(p) => {
            let ft = std::fs::metadata(p).map_err(|e| format!("{}: {e}", p.display()))?.file_type();
            if !ft.is_char_device() {
                return Err(format!("{} is not a serial device", p.display()));
            }
            Ok(Box::new(SerialTransport::open(p, timeout).map_err(|e| format!("{}: {e}", p.display()))?))
        }
        Kind::Socket(p) => {
            let ft = std::fs::metadata(p).map_err(|e| format!("{}: {e}", p.display()))?.file_type();
            if !ft.is_socket() {
                return Err(format!("{} is not a socket", p.display()));
            }
            Ok(Box::new(std::os::unix::net::UnixStream::connect(p).map_err(|e| format!("{}: {e}", p.display()))?))
        }
        Kind::RecoveryUsb(_) | Kind::RecoverySocket(_) => Err("a device in recovery mode has no RPC session".into()),
    }
}

/// A local file to send: a regular file (a FIFO or device would hang or never end), at most `max` bytes.
pub fn read_local(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if meta.len() > max {
        return Err(format!("{} is larger than {max} bytes", path.display()));
    }
    let mut data = Vec::new();
    std::fs::File::open(path).and_then(|f| f.take(max + 1).read_to_end(&mut data)).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(data)
}

/// A name for a new file or folder on the device: what the Qt app's editor accepts (printable ASCII), without `/`, and
/// not `.` or `..`.
pub fn valid_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name == "." || name == ".." {
        return Err("Give a name.".into());
    }
    if !name.bytes().all(|b| (0x20..0x7f).contains(&b)) || name.contains('/') || name.contains('\\') {
        return Err("Names on the device are printable ASCII without / or \\.".into());
    }
    if name.len() > 255 {
        return Err("The name is longer than 255 characters.".into());
    }
    Ok(())
}

/// `dir/name` on the device.
pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn run(found: Found, timeout: Duration, rx: Receiver<Cmd>, tx: Sender<Event>, bridge: Arc<Mutex<Bridge>>, waker: Waker) {
    let send = |e: Event| {
        let _ = tx.send(e);
        waker();
    };
    let io = match open(&found, timeout) {
        Ok(io) => io,
        Err(e) => return send(Event::Failed(e)),
    };
    let mut s = match Session::start(io, timeout) {
        Ok(s) => s,
        Err(e) => return send(Event::Failed(e.to_string())),
    };
    match read_info(&mut s) {
        Ok(info) => send(Event::Connected(info)),
        Err(e) => return send(Event::Failed(format!("Failed to read the device information: {e}"))),
    }
    let mut want_stream = false;
    let mut streaming = false;
    let mut last_alive = Instant::now();
    loop {
        if want_stream != streaming {
            let r = if want_stream { s.screen_start() } else { s.screen_stop() };
            if let Err(e) = r {
                return send(Event::Gone(e.to_string()));
            }
            streaming = want_stream;
        }
        let cmd = if streaming {
            match rx.try_recv() {
                Ok(c) => c,
                Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => {
                    match s.next_screen_frame(FRAME_POLL) {
                        Ok(Some(f)) => send(Event::Frame(f)),
                        Ok(None) => {}
                        Err(e) => return send(Event::Gone(e.to_string())),
                    }
                    continue;
                }
            }
        } else {
            match rx.recv_timeout(KEEPALIVE.saturating_sub(last_alive.elapsed())) {
                Ok(c) => c,
                Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {
                    last_alive = Instant::now();
                    if let Err(e) = s.protobuf_version() {
                        return send(Event::Gone(e.to_string()));
                    }
                    continue;
                }
            }
        };
        match cmd {
            Cmd::Disconnect => {
                if streaming {
                    let _ = s.screen_stop();
                }
                return;
            }
            Cmd::Screen(on) => want_stream = on,
            Cmd::Input(key, kind) => {
                if let Err(e) = s.gui_input(key, kind) {
                    if !matches!(e, RpcError::Device(_)) {
                        return send(Event::Gone(e.to_string()));
                    }
                }
            }
            other => {
                // every other request runs with the stream stopped, as the C++ app pauses its streamer
                if streaming {
                    if let Err(e) = s.screen_stop() {
                        return send(Event::Gone(e.to_string()));
                    }
                    streaming = false;
                }
                let (event, left) = handle(&mut s, other, &bridge);
                let broken = matches!(&event, Some(Event::Listing { result: Err(_), .. }) | Some(Event::Done { result: Err(_), .. }))
                    && s.protobuf_version().is_err();
                if let Some(e) = event {
                    send(e);
                }
                if let Some(why) = left {
                    s.wait_until_gone(GONE_WAIT);
                    return send(Event::Gone(why));
                }
                if broken {
                    return send(Event::Gone("the device stopped answering".into()));
                }
                last_alive = Instant::now();
            }
        }
    }
}

fn done(op: Op, r: Result<String, String>) -> Option<Event> {
    Some(Event::Done { op, result: r.map(|summary| Outcome { summary, warning: None }) })
}

/// Runs one command; returns the event to send and, when the device leaves after it, why.
fn handle<T: Transport>(s: &mut Session<T>, cmd: Cmd, bridge: &Arc<Mutex<Bridge>>) -> (Option<Event>, Option<String>) {
    let err = |e: RpcError| e.to_string();
    match cmd {
        Cmd::Refresh => match read_info(s) {
            Ok(i) => (Some(Event::Info(i)), None),
            Err(e) => (Some(Event::Gone(e.to_string())), None),
        },
        Cmd::List(path) => {
            let result = s.storage_list(&path).map_err(err);
            (Some(Event::Listing { path, result }), None)
        }
        Cmd::Pull { remote, local } => {
            let r = s.pull(&remote, &local, &flipper_rpc::transfer::Limits::default()).map_err(err).map(|r| {
                let refused = if r.refused.is_empty() { String::new() } else { format!(", {} names refused", r.refused.len()) };
                format!("Downloaded {} file(s), {} bytes, to {}{refused}", r.files, r.bytes, local.display())
            });
            (done(Op::Pull, r), None)
        }
        Cmd::Push { local, remote_dir } => {
            let r = (|| {
                let name = local.file_name().and_then(|n| n.to_str()).ok_or("the file name is not UTF-8")?.to_string();
                valid_name(&name)?;
                let data = read_local(&local, flipper_rpc::write::MAX_WRITE as u64)?;
                let remote = join(&remote_dir, &name);
                s.storage_write(&remote, &data).map_err(err)?;
                Ok(format!("Uploaded {name} ({} bytes) to {remote_dir}", data.len()))
            })();
            (done(Op::Push, r), None)
        }
        Cmd::Mkdir(path) => (done(Op::Mkdir, s.storage_mkdir(&path).map_err(err).map(|_| format!("Made {path}"))), None),
        Cmd::Remove { path, recursive } => {
            (done(Op::Remove, s.storage_remove(&path, recursive).map_err(err).map(|_| format!("Deleted {path}"))), None)
        }
        Cmd::Rename { from, to } => {
            (done(Op::Rename, s.storage_rename(&from, &to).map_err(err).map(|_| format!("Renamed {from} to {to}"))), None)
        }
        Cmd::Backup(out) => {
            let r = s
                .backup(&out, &flipper_rpc::transfer::Limits::backup(), now_secs())
                .map_err(err)
                .map(|r| format!("Backed up {} file(s) in {} folder(s), {} bytes, to {}", r.files, r.dirs, r.bytes, out.display()));
            (done(Op::Backup, r), None)
        }
        Cmd::Restore(file) => {
            // checked whole before anything is sent; a refused archive changes nothing on the device
            let archive = match std::fs::File::open(&file).map_err(|e| format!("{}: {e}", file.display())).and_then(|f| {
                flipper_rpc::restore::read_backup(std::io::BufReader::new(f), &flipper_rpc::restore::ArchiveLimits::default())
                    .map_err(|e| e.to_string())
            }) {
                Ok(a) => a,
                Err(e) => return (done(Op::Restore, Err(e)), None),
            };
            match s.restore(&archive) {
                Ok(r) => {
                    // as the C++ app: the device restarts to load the restored settings
                    let rebooted = s.system_reboot(flipper_rpc::write::RebootMode::Os);
                    let summary = format!("Restored {} file(s), {} bytes", r.files, r.bytes);
                    match rebooted {
                        Ok(()) => (done(Op::Restore, Ok(summary)), Some("the device restarts".into())),
                        Err(e) => (done(Op::Restore, Err(format!("{summary}, but the restart failed: {e}"))), None),
                    }
                }
                Err(e) => (done(Op::Restore, Err(e.to_string())), None),
            }
        }
        Cmd::Erase => match s.factory_reset() {
            Ok(()) => (done(Op::Erase, Ok("The device is erased and restarts".into())), Some("the device restarts".into())),
            Err(e) => (done(Op::Erase, Err(e.to_string())), None),
        },
        Cmd::Update(job) => {
            // as the C++ FullUpdateOperation: the region data first; a failure there is reported and the update goes on
            let mut notes = Vec::new();
            match &job.region {
                None => {}
                Some(Ok(bytes)) => {
                    if let Err(e) = s.storage_write(flipper_rpc::assets::REGION_DATA_PATH, bytes) {
                        if !matches!(e, RpcError::Device(_)) {
                            return (done(Op::Update, Err(e.to_string())), None);
                        }
                        notes.push(format!("the region data was not written: {e}"));
                    }
                }
                Some(Err(e)) => notes.push(format!("the region data was not written: {e}")),
            }
            match s.update(&job.package) {
                Ok(r) => {
                    let mut summary = format!(
                        "Sent {} file(s) ({} bytes, {} already there); the device's updater takes over",
                        r.files_sent, r.bytes, r.files_kept
                    );
                    for n in notes {
                        summary.push_str("; ");
                        summary.push_str(&n);
                    }
                    (done(Op::Update, Ok(summary)), Some("the device restarts into its updater".into()))
                }
                Err(e) => (done(Op::Update, Err(e.to_string())), None),
            }
        }
        Cmd::Captures { roots, label } => {
            let path = bridge.lock().unwrap_or_else(|e| e.into_inner()).datastore_path().to_string();
            let r = (|| {
                let store = argus_capture::store::Store::open(Path::new(&path)).map_err(|e| e.0)?;
                let session = bridge.lock().unwrap_or_else(|e| e.into_inner()).session().map(|(id, _)| id);
                let roots: Vec<&str> = roots.iter().map(String::as_str).collect();
                bridge.lock().unwrap_or_else(|e| e.into_inner()).append(
                    argus_api::eventlog::Severity::Notice,
                    argus_api::eventlog::Source::Device,
                    &format!("Pulling {label} from device…"),
                    "",
                );
                let mut log = |sev, src, msg: &str, detail: &str| {
                    bridge.lock().unwrap_or_else(|e| e.into_inner()).append(sev, src, msg, detail);
                    Ok(())
                };
                argus_capture::sync::pull_logged(s, &store, session, &roots, &label, &flipper_rpc::transfer::Limits::default(), &mut log)
                    .map(|r| format!("{} new capture(s), {} already stored, {} failed", r.new, r.duplicates, r.failed.len()))
                    .map_err(|e| e.to_string())
            })();
            (done(Op::Captures, r), None)
        }
        Cmd::Disconnect | Cmd::Screen(_) | Cmd::Input(..) => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_information_reads_both_key_styles() {
        let fake = flipper_fake::FakeFlipper::standard();
        let d = DeviceInfo::from_pairs(fake.devinfo.clone(), InfoSource::Property, (0, 25));
        assert_eq!(d.name, "Fakeflip");
        assert_eq!(d.hardware, "12.f7b9c6");
        assert_eq!(d.radio_version, "1.17.3");
        assert_eq!(d.stack_type, 3);
        assert_eq!(d.firmware_shown(), "ARG-1.5.1r2");
        let old = DeviceInfo::from_pairs(fake.legacy_info.clone(), InfoSource::Legacy, (0, 13));
        assert_eq!((old.name.as_str(), old.firmware_branch.as_str(), old.radio_version.as_str()), ("Fakeflip", "dev", ""));
    }

    #[test]
    fn names_for_the_device_are_checked() {
        assert!(valid_name("notes.txt").is_ok());
        for bad in ["", ".", "..", "a/b", "a\\b", "caf\u{e9}", "tab\there"] {
            assert!(valid_name(bad).is_err(), "{bad:?}");
        }
        assert_eq!(join("/ext", "a"), "/ext/a");
        assert_eq!(join("/", "ext"), "/ext");
    }
}
