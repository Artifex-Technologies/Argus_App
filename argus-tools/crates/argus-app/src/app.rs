//! The window: qFlipper's layout and motion (application/components/*.qml), drawn in Yggdrasil's style.
//!
//! - **Layout** as qFlipper's `MainWindow.qml`: a title strip, one content card (830 x 390), a footer with LOGS, CONSOLE,
//!   SETTINGS and the status bar, and a log drawer under it. In the card, the stages of `ApplicationBackend` each have
//!   their overlay: no device (`NoDeviceOverlay`), home with the tabs on the left and the device on the right
//!   (`HomeOverlay`), an operation in progress (`UpdateOverlay`), its end (`FinishOverlay`, with the partial-success
//!   warning) and the full-size screen with the keypad (`StreamOverlay`).
//! - **Motion** as the QML's Behaviors: overlays and dialogs fade in 150 ms (InOutQuad), the device view slides between
//!   the home position and the operation position in 350 ms (InOutQuad), the log drawer opens in 500 ms (InOutQuad), the
//!   connection spinner turns once in 1.5 s, the keypad's long press comes after 350 ms and repeats every 150 ms. egui
//!   asks for frames only while something moves: an idle window does no work (no scanline sweep, which qFlipper's grid
//!   background runs for ever).
//! - **Look** is Yggdrasil's (`theme.rs`): its palette, light or dark from the desktop, rounded shapes, egui's fonts, the
//!   user's accent colour where qFlipper has orange. The device is drawn as plain shapes; no Flipper artwork or logo.
//!
//! Nothing that changes the device runs without its confirmation dialog: every such action becomes an [`Action`] held by
//! a [`Dialog`], and only the dialog's confirm button hands it to [`ArgusApp::execute`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use argus_api::bridge::Bridge;
use argus_api::eventlog::{severity_name, source_name, Severity, Source as LogSource};
use eframe::egui::{
    self, emath::easing, pos2, vec2, Align2, Color32, CornerRadius, FontId, Id, Key, Rect, RichText, Sense, Stroke, StrokeKind,
    TextureHandle, TextureOptions, UiBuilder, WidgetInfo, WidgetType,
};
use flipper_rpc::ops::{Entry, FileKind};
use flipper_rpc::proto::pb_gui::{InputKey, InputType};
use flipper_rpc::screen::Frame;
use flipper_updates::directory::Directory;

use crate::device::{self, Cmd, DeviceInfo, Event, Op, Waker, Worker};
use crate::discovery::{Discovery, Found};
use crate::firmware::{self, UpdateState};
use crate::net::{Net, NetEvent, PackageSource};
use crate::settings::{Settings, Style};
use crate::theme::{self, Palette, Theme};

/// The app's version: the repository's VERSION file, as the Qt app and argus-tools show it.
pub const VERSION: &str = include_str!("../../../../VERSION").trim_ascii();

/// qFlipper's geometry: the window, the content card, the log drawer.
pub const WINDOW: [f32; 2] = [860.0, 500.0];
const STRIP: f32 = 38.0;
const CARD_W: f32 = 830.0;
const CARD_H: f32 = 390.0;
const LOG_H: f32 = 200.0;
/// The device view: its size, and where its left edge sits at home and during an operation (card coordinates).
const DEVICE: [f32; 2] = [360.0, 160.0];
const DEVICE_HOME_X: f32 = 415.0;
/// Centred in the card, where qFlipper puts its device (216 in its 800-wide card, for its own picture).
const DEVICE_BUSY_X: f32 = (CARD_W - DEVICE[0]) / 2.0;
const DEVICE_Y: f32 = 82.0;
/// Where the name, the main button and its links centre on the home screen.
const CENTER_X: f32 = 595.0;

const FADE: f32 = 0.150;
const SLIDE: f32 = 0.350;
const DRAWER: f32 = 0.500;
const LONG_PRESS: f64 = 0.350;
const REPEAT: f64 = 0.150;

/// Everything the app needs from outside; `main` builds it from the command line, the tests from their fakes.
pub struct Config {
    pub discovery: Box<dyn Discovery>,
    /// The settings folder (`~/.config/argus`).
    pub config_dir: PathBuf,
    /// The shared capture datastore and the capture library (the Qt app's and argus-cli's).
    pub datastore: PathBuf,
    pub library_dir: PathBuf,
    pub net: Net,
    /// A fixed style instead of Settings' choice (the tests).
    pub theme: Option<theme::Source>,
    /// Yggdrasil's config folder (appearance.conf), for "follow the system".
    pub yggdrasil_config: Option<PathBuf>,
    /// The RPC timeout.
    pub timeout: Duration,
    /// Where save dialogs start (the home folder).
    pub home: PathBuf,
    /// The control API's token; empty: a new one written to the runtime folder, as the Qt app does.
    pub api_token: String,
    /// The wait between the FUS status polls of a repair (1 s, as the C++ app).
    pub recovery_poll: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Info,
    Actions,
    Files,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stage {
    Waiting,
    Connecting,
    Ready,
    Streaming,
    Busy(Op),
    /// The end of an operation; a warning is a partial success (`DeviceState::warningString`).
    Finished {
        warning: Option<String>,
    },
    Error {
        title: String,
        text: String,
    },
}

/// What a confirmation dialog runs when it is confirmed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Backup,
    Restore,
    Erase,
    Update(PackageSource),
    InstallFile,
    Upload {
        dir: String,
    },
    Mkdir {
        dir: String,
    },
    Rename {
        dir: String,
        old: String,
    },
    Delete {
        path: String,
        dir: bool,
    },
    Download {
        remote: String,
    },
    SaveScreenshot,
    /// The repair with local files (recovery mode only).
    Repair,
    ExitRecovery,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dialog {
    pub title: String,
    pub message: String,
    pub warning: Option<String>,
    pub confirm: String,
    /// The confirm button is red and Cancel is the suggested choice (qFlipper's `RejectRole`).
    pub danger: bool,
    pub fields: Vec<Field>,
    pub action: Action,
    pub error: Option<String>,
}

#[derive(Default)]
struct Files {
    path: String,
    back: Vec<String>,
    forward: Vec<String>,
    entries: Vec<Entry>,
    loaded: Option<String>,
    selected: Option<String>,
    error: Option<String>,
}

impl Files {
    fn is_root(&self) -> bool {
        self.path == "/"
    }
}

/// The keypad's press timing, as qFlipper's `KeypadButton` (press, long after 350 ms, repeat every 150 ms; a release
/// before the long press is a short press).
#[derive(Default)]
struct Keypad {
    down: Option<Press>,
}

#[derive(Clone, Copy)]
struct Press {
    key: InputKey,
    since: f64,
    long: bool,
    last: f64,
    /// Held by the pointer (released when the button goes up), else by the keyboard (released with its key).
    pointer: bool,
}

struct Api {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    port: u16,
}

impl Drop for Api {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

enum DirState {
    Unknown,
    Checking,
    Error(String),
    Ready(Directory),
}

pub struct ArgusApp {
    cfg: Config,
    pub settings: Settings,
    theme: Theme,
    bridge: Arc<Mutex<Bridge>>,
    api: Option<Api>,
    waker: Option<Waker>,
    found: Vec<Found>,
    last_scan: f64,
    /// A device that failed to connect: not connected to by itself again until the list changes.
    failed: Option<Found>,
    current: Option<Found>,
    worker: Option<Worker>,
    stage: Stage,
    info: Option<DeviceInfo>,
    tab: Tab,
    console: bool,
    log_open: bool,
    settings_open: bool,
    log: Vec<String>,
    frame: Option<Frame>,
    texture: Option<TextureHandle>,
    texture_frame: Option<Frame>,
    stream_wanted: bool,
    files: Files,
    dialog: Option<Dialog>,
    /// A file operation or capture pull the worker runs (the big ones are stages).
    busy: Option<Op>,
    note: Option<String>,
    directory: DirState,
    net_tx: Sender<NetEvent>,
    net_rx: Receiver<NetEvent>,
    rec_tx: Sender<Result<device::Outcome, String>>,
    rec_rx: Receiver<Result<device::Outcome, String>>,
    keypad: Keypad,
    console_note: String,
}

fn ease(t: f32) -> f32 {
    easing::quadratic_in_out(t)
}

impl ArgusApp {
    pub fn new(cfg: Config) -> ArgusApp {
        let settings = Settings::load(&cfg.config_dir);
        let source = cfg.theme.clone().unwrap_or_else(|| style_source(settings.style, cfg.yggdrasil_config.clone()));
        let bridge = Arc::new(Mutex::new(Bridge::new(&cfg.datastore, cfg.library_dir.clone())));
        let (net_tx, net_rx) = mpsc::channel();
        let (rec_tx, rec_rx) = mpsc::channel();
        ArgusApp {
            settings,
            theme: Theme::new(source),
            bridge,
            api: None,
            waker: None,
            found: Vec::new(),
            last_scan: f64::NEG_INFINITY,
            failed: None,
            current: None,
            worker: None,
            stage: Stage::Waiting,
            info: None,
            tab: Tab::Info,
            console: false,
            log_open: false,
            settings_open: false,
            log: Vec::new(),
            frame: None,
            texture: None,
            texture_frame: None,
            stream_wanted: false,
            files: Files { path: "/".into(), ..Files::default() },
            dialog: None,
            busy: None,
            note: None,
            directory: DirState::Unknown,
            net_tx,
            net_rx,
            rec_tx,
            rec_rx,
            keypad: Keypad::default(),
            console_note: String::new(),
            cfg,
        }
    }

    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    pub fn dialog(&self) -> Option<&Dialog> {
        self.dialog.as_ref()
    }

    pub fn api_port(&self) -> Option<u16> {
        self.api.as_ref().map(|a| a.port)
    }

    fn logline(&mut self, line: impl Into<String>) {
        self.log.push(line.into());
        if self.log.len() > 500 {
            self.log.remove(0);
        }
    }

    fn send(&self, cmd: Cmd) {
        if let Some(w) = &self.worker {
            w.send(cmd);
        }
    }

    fn save_settings(&mut self) {
        if let Err(e) = self.settings.save(&self.cfg.config_dir) {
            self.logline(format!("Cannot save the settings: {e}"));
        }
        if self.cfg.theme.is_none() {
            self.theme.set_source(style_source(self.settings.style, self.cfg.yggdrasil_config.clone()));
        }
    }

    // ------------------------------------------------------------------ devices

    fn scan(&mut self, now: f64) {
        if self.worker.is_some() || !matches!(self.stage, Stage::Waiting) || now - self.last_scan < 1.0 {
            return;
        }
        self.last_scan = now;
        let found = self.cfg.discovery.scan();
        if found != self.found {
            self.failed = None;
            self.found = found;
        }
        if self.settings.auto_connect && self.found.len() == 1 && self.failed.as_ref() != Some(&self.found[0]) {
            let f = self.found[0].clone();
            self.connect(f);
        }
    }

    fn connect(&mut self, found: Found) {
        self.current = Some(found.clone());
        self.frame = None;
        self.files = Files { path: "/".into(), ..Files::default() };
        if found.is_recovery() {
            self.logline(format!("Recovery mode device at {}", found.location()));
            self.info = None;
            self.stage = Stage::Ready;
            return;
        }
        let Some(waker) = self.waker.clone() else { return };
        self.logline(format!("Connecting to {}", found.location()));
        self.stage = Stage::Connecting;
        self.worker = Some(Worker::spawn(found, self.cfg.timeout, self.bridge.clone(), waker));
    }

    fn recovery(&self) -> bool {
        self.current.as_ref().is_some_and(Found::is_recovery)
    }

    fn update_state(&self) -> UpdateState {
        if self.current.is_none() {
            return UpdateState::Unknown;
        }
        match &self.directory {
            DirState::Unknown => UpdateState::Unknown,
            DirState::Checking => UpdateState::Checking,
            DirState::Error(e) => UpdateState::Error(e.clone()),
            DirState::Ready(d) => {
                let latest = d.channel(&self.settings.update_channel).and_then(|c| c.latest());
                firmware::state(&self.info.clone().unwrap_or_default(), self.recovery(), &self.settings.update_channel, latest)
            }
        }
    }

    fn latest_label(&self) -> Option<String> {
        let DirState::Ready(d) = &self.directory else { return None };
        let v = d.channel(&self.settings.update_channel)?.latest()?;
        Some(firmware::channel_label(&self.settings.update_channel, &v.number))
    }

    fn check_updates(&mut self) {
        if let Some(w) = self.waker.clone() {
            self.directory = DirState::Checking;
            self.cfg.net.fetch_directory(self.net_tx.clone(), w);
        }
    }

    fn poll(&mut self) {
        let events: Vec<Event> = match &self.worker {
            Some(w) => w.rx.try_iter().collect(),
            None => Vec::new(),
        };
        for e in events {
            self.on_event(e);
        }
        let done: Vec<Result<device::Outcome, String>> = self.rec_rx.try_iter().collect();
        for result in done {
            let Stage::Busy(op) = self.stage else { continue };
            self.on_event(Event::Done { op, result });
        }
        let net: Vec<NetEvent> = self.net_rx.try_iter().collect();
        for e in net {
            match e {
                NetEvent::Directory(Ok(d)) => {
                    if d.channel(&self.settings.update_channel).is_none() {
                        if let Some(first) = d.channel_names().first() {
                            self.settings.update_channel = first.to_string();
                        }
                    }
                    self.directory = DirState::Ready(d);
                }
                NetEvent::Directory(Err(e)) => {
                    self.logline(format!("Cannot read the update directory: {e}"));
                    self.directory = DirState::Error(e);
                }
                NetEvent::UpdateReady(Ok(job)) => {
                    if self.stage == Stage::Busy(Op::Update) {
                        if self.worker.is_some() {
                            self.send(Cmd::Update(job));
                        } else {
                            self.stage = Stage::Error {
                                title: "Update Failed".into(),
                                text: "The device left before the update could start.".into(),
                            };
                        }
                    }
                }
                NetEvent::UpdateReady(Err(e)) => {
                    if self.stage == Stage::Busy(Op::Update) {
                        self.stage = Stage::Error { title: "Update Failed".into(), text: e };
                    }
                }
            }
        }
    }

    fn on_event(&mut self, e: Event) {
        match e {
            Event::Connected(info) => {
                self.logline(format!("Connected to {} ({})", info.name, info.firmware_shown()));
                self.info = Some(info);
                if self.stage == Stage::Connecting {
                    self.stage = Stage::Ready;
                }
                self.tab = Tab::Info;
                if self.settings.check_updates {
                    self.check_updates();
                }
            }
            Event::Info(info) => self.info = Some(info),
            Event::Listing { path, result } => {
                if path == self.files.path {
                    self.files.loaded = Some(path);
                    match result {
                        Ok(mut entries) => {
                            entries.retain(|e| !(self.files.path == "/" && e.name == "any"));
                            entries.sort_by(|a, b| {
                                (a.kind != FileKind::Dir, a.name.to_lowercase()).cmp(&(b.kind != FileKind::Dir, b.name.to_lowercase()))
                            });
                            self.files.entries = entries;
                            self.files.error = None;
                        }
                        Err(e) => {
                            self.files.entries.clear();
                            self.files.error = Some(e);
                        }
                    }
                }
            }
            Event::Frame(f) => self.frame = Some(f),
            Event::Done { op, result } => {
                self.busy = None;
                match (&result, op.is_big()) {
                    (Ok(o), true) => {
                        self.logline(o.summary.clone());
                        self.stage = Stage::Finished { warning: o.warning.clone() };
                    }
                    (Err(e), true) => {
                        self.logline(format!("{} failed: {e}", op.title()));
                        self.stage = Stage::Error { title: error_title(op).into(), text: e.clone() };
                    }
                    (Ok(o), false) => {
                        self.logline(o.summary.clone());
                        self.note = Some(o.summary.clone());
                    }
                    (Err(e), false) => {
                        self.logline(format!("{}: {e}", op.title()));
                        self.note = Some(format!("Failed: {e}"));
                    }
                }
                if matches!(op, Op::Push | Op::Mkdir | Op::Remove | Op::Rename) {
                    self.files.loaded = None;
                }
            }
            Event::Gone(why) => {
                self.logline(format!("Device disconnected: {why}"));
                self.worker = None;
                self.info = None;
                self.frame = None;
                self.stream_wanted = false;
                self.busy = None;
                self.directory = DirState::Unknown;
                if matches!(self.stage, Stage::Ready | Stage::Streaming | Stage::Connecting | Stage::Busy(_)) {
                    if let Stage::Busy(op) = self.stage {
                        self.stage =
                            Stage::Error { title: error_title(op).into(), text: format!("The device left during the operation: {why}") };
                    } else {
                        self.stage = Stage::Waiting;
                        self.current = None;
                    }
                }
                self.dialog = None;
            }
            Event::Failed(why) => {
                self.logline(format!("Cannot connect: {why}"));
                self.worker = None;
                self.failed = self.current.take();
                self.stage = Stage::Error { title: "Cannot Connect".into(), text: why };
            }
        }
    }

    /// The screen stream runs while the home screen (not the file manager) or the full-size screen shows, as qFlipper's.
    fn sync_stream(&mut self) {
        let want = self.worker.is_some()
            && self.info.is_some()
            && (self.stage == Stage::Streaming || (self.stage == Stage::Ready && self.tab != Tab::Files && !self.console));
        if want != self.stream_wanted {
            self.stream_wanted = want;
            self.send(Cmd::Screen(want));
        }
        let listing_due = self.files.loaded.as_deref() != Some(self.files.path.as_str());
        if self.stage == Stage::Ready
            && self.tab == Tab::Files
            && !self.console
            && self.worker.is_some()
            && self.busy.is_none()
            && listing_due
        {
            self.files.loaded = Some(self.files.path.clone());
            self.send(Cmd::List(self.files.path.clone()));
        }
    }

    // ------------------------------------------------------------------ actions

    fn open_dialog(&mut self, d: Dialog) {
        self.dialog = Some(d);
    }

    fn device_name(&self) -> String {
        self.info.as_ref().map(|i| i.name.clone()).unwrap_or_else(|| "device".into())
    }

    fn ask_update(&mut self) {
        let Some(info) = self.info.clone() else { return };
        let label = self.latest_label().unwrap_or_default();
        let mut warning = firmware::fork_warning(&info);
        if !info.sd_present {
            warning = Some("There is no SD card in the device: an update through its updater needs one.".into());
        }
        self.open_dialog(Dialog {
            title: "Update firmware?".into(),
            message: format!("New firmware {label} will be installed"),
            warning,
            confirm: "Update".into(),
            danger: false,
            fields: Vec::new(),
            action: Action::Update(PackageSource::Server { channel: self.settings.update_channel.clone() }),
            error: None,
        });
    }

    fn ask(&mut self, action: Action) {
        let home = self.cfg.home.clone();
        let stamp = stamp();
        let d = match &action {
            Action::Backup => Dialog {
                title: "Backup device?".into(),
                message: "Device settings will be backed up".into(),
                warning: None,
                confirm: "Backup".into(),
                danger: false,
                fields: vec![field("Save to", home.join(format!("{}-backup-{stamp}.tgz", self.device_name())))],
                action,
                error: None,
            },
            Action::Restore => Dialog {
                title: "Restore backup?".into(),
                message: "Device settings will be restored from the selected backup".into(),
                warning: None,
                confirm: "Restore".into(),
                danger: false,
                fields: vec![field("Backup file", home.join(""))],
                action,
                error: None,
            },
            Action::Erase => Dialog {
                title: "Erase device?".into(),
                message: "Device settings will be fully erased".into(),
                warning: None,
                confirm: "Erase".into(),
                danger: true,
                fields: Vec::new(),
                action,
                error: None,
            },
            Action::InstallFile => Dialog {
                title: "Install from file?".into(),
                message: "Firmware from an update package (.tgz) will be installed".into(),
                warning: self.info.as_ref().and_then(firmware::fork_warning),
                confirm: "Install".into(),
                danger: false,
                fields: vec![field("Update package", home.join(""))],
                action,
                error: None,
            },
            Action::Upload { dir } => Dialog {
                title: "Upload a file?".into(),
                message: format!("The file will be written to {}", shown_path(dir)),
                warning: None,
                confirm: "Upload".into(),
                danger: false,
                fields: vec![field("File", home.join(""))],
                action,
                error: None,
            },
            Action::Mkdir { dir } => Dialog {
                title: "New folder?".into(),
                message: format!("A folder will be made in {}", shown_path(dir)),
                warning: None,
                confirm: "Create".into(),
                danger: false,
                fields: vec![Field { label: "Name".into(), value: "New Folder".into() }],
                action,
                error: None,
            },
            Action::Rename { old, .. } => Dialog {
                title: format!("Rename \"{old}\"?"),
                message: "The new name".into(),
                warning: None,
                confirm: "Rename".into(),
                danger: false,
                fields: vec![Field { label: "Name".into(), value: old.clone() }],
                action,
                error: None,
            },
            Action::Delete { path, .. } => Dialog {
                title: format!("Delete \"{}\"?", path.rsplit('/').next().unwrap_or(path)),
                message: "This action cannot be undone.".into(),
                warning: None,
                confirm: "Delete".into(),
                danger: true,
                fields: Vec::new(),
                action,
                error: None,
            },
            Action::Download { remote } => Dialog {
                title: "Download?".into(),
                message: format!("{} will be copied to this computer", shown_path(remote)),
                warning: None,
                confirm: "Download".into(),
                danger: false,
                fields: vec![field("Save to", home.join(remote.rsplit('/').next().unwrap_or("download")))],
                action,
                error: None,
            },
            Action::SaveScreenshot => Dialog {
                title: "Save screenshot?".into(),
                message: "The screen will be saved as a PNG picture".into(),
                warning: None,
                confirm: "Save".into(),
                danger: false,
                fields: vec![field("Save to", home.join(format!("Screenshot-{stamp}.png")))],
                action,
                error: None,
            },
            Action::Repair => Dialog {
                title: "Repair Device?".into(),
                message: "The firmware from these files will be installed, the option bytes corrected, and the wireless stack \
                          installed when an image is given. Recovery may erase your progress and settings."
                    .into(),
                warning: None,
                confirm: "Repair".into(),
                danger: false,
                fields: vec![
                    field("Firmware (.dfu)", home.join("")),
                    field("Option bytes (ob.data)", home.join("")),
                    Field { label: "Wireless stack image (.bin, optional)".into(), value: String::new() },
                ],
                action,
                error: None,
            },
            Action::ExitRecovery => Dialog {
                title: "Leave recovery mode?".into(),
                message: "The chip leaves its bootloader and starts its firmware".into(),
                warning: None,
                confirm: "Leave".into(),
                danger: false,
                fields: Vec::new(),
                action,
                error: None,
            },
            Action::Update(_) => return self.ask_update(),
        };
        self.open_dialog(d);
    }

    /// Runs a confirmed action. Returns why it cannot run (the dialog stays open and shows it).
    pub fn execute(&mut self, action: Action, fields: &[Field]) -> Result<(), String> {
        let value = |i: usize| fields.get(i).map(|f| f.value.trim().to_string()).unwrap_or_default();
        let path = |i: usize| -> Result<PathBuf, String> {
            let v = value(i);
            if v.is_empty() || v.ends_with('/') {
                return Err("Name a file.".into());
            }
            Ok(PathBuf::from(v))
        };
        let new_path = |i: usize| -> Result<PathBuf, String> {
            let p = path(i)?;
            if std::fs::symlink_metadata(&p).is_ok() {
                return Err(format!("{} exists already.", p.display()));
            }
            Ok(p)
        };
        match action {
            Action::Backup => {
                let p = new_path(0)?;
                self.big(Op::Backup);
                self.send(Cmd::Backup(p));
            }
            Action::Restore => {
                let p = path(0)?;
                self.big(Op::Restore);
                self.send(Cmd::Restore(p));
            }
            Action::Erase => {
                self.big(Op::Erase);
                self.send(Cmd::Erase);
            }
            Action::Update(source) => {
                let region = self.info.as_ref().map(|i| i.region).unwrap_or(0);
                self.big(Op::Update);
                if let Some(w) = self.waker.clone() {
                    self.cfg.net.prepare_update(source, region, self.net_tx.clone(), w);
                }
            }
            Action::InstallFile => {
                let p = path(0)?;
                return self.execute(Action::Update(PackageSource::File(p)), &[]);
            }
            Action::Upload { dir } => {
                let p = path(0)?;
                if !p.is_file() {
                    return Err(format!("{} is not a file.", p.display()));
                }
                self.busy = Some(Op::Push);
                self.send(Cmd::Push { local: p, remote_dir: dir });
            }
            Action::Mkdir { dir } => {
                let name = value(0);
                device::valid_name(&name)?;
                self.busy = Some(Op::Mkdir);
                self.send(Cmd::Mkdir(device::join(&dir, &name)));
            }
            Action::Rename { dir, old } => {
                let name = value(0);
                device::valid_name(&name)?;
                if name == old {
                    return Ok(());
                }
                self.busy = Some(Op::Rename);
                self.send(Cmd::Rename { from: device::join(&dir, &old), to: device::join(&dir, &name) });
            }
            Action::Delete { path, dir } => {
                self.busy = Some(Op::Remove);
                self.send(Cmd::Remove { path, recursive: dir });
            }
            Action::Download { remote } => {
                let p = new_path(0)?;
                self.busy = Some(Op::Pull);
                self.send(Cmd::Pull { remote, local: p });
            }
            Action::Repair | Action::ExitRecovery => {
                let kind = self.current.as_ref().map(|f| f.kind.clone()).ok_or("No device.")?;
                if let Some(why) = crate::recovery::unreachable(&kind) {
                    return Err(why);
                }
                let (job, op) = if action == Action::Repair {
                    let radio = value(2);
                    let files = crate::recovery::RepairFiles::read(&path(0)?, &path(1)?, (!radio.is_empty()).then(|| Path::new(&radio)))?;
                    (crate::recovery::Job::Repair(files), Op::Repair)
                } else {
                    (crate::recovery::Job::Exit, Op::Exit)
                };
                let waker = self.waker.clone().ok_or("Not ready yet.")?;
                crate::recovery::start(kind, self.cfg.recovery_poll, job, self.rec_tx.clone(), waker)?;
                self.big(op);
            }
            Action::SaveScreenshot => {
                let p = new_path(0)?;
                let frame = self.frame.clone().ok_or("There is no screen to save yet.")?;
                save_png(&p, &frame, &self.lcd()).map_err(|e| format!("{}: {e}", p.display()))?;
                self.logline(format!("Screenshot saved to {}", p.display()));
            }
        }
        Ok(())
    }

    fn big(&mut self, op: Op) {
        self.note = None;
        self.stage = Stage::Busy(op);
    }

    fn finish(&mut self) {
        // Continue / Back: home again if the device is still here, else wait for it (it comes back after a restart)
        self.stage = if self.worker.is_some() { Stage::Ready } else { Stage::Waiting };
        if self.worker.is_none() {
            self.current = None;
            self.last_scan = f64::NEG_INFINITY;
        }
        if self.worker.is_some() {
            self.send(Cmd::Refresh);
        }
    }

    // ------------------------------------------------------------------ API

    fn toggle_api(&mut self) {
        if self.api.take().is_some() {
            return;
        }
        let port = self.settings.api_port;
        let server = {
            let mut b = self.bridge.lock().unwrap_or_else(|e| e.into_inner());
            argus_api::server::Server::start(&mut b, port, &self.cfg.api_token)
        };
        match server {
            Ok(server) => {
                let stop = Arc::new(AtomicBool::new(false));
                let port = server.port();
                let (bridge, flag) = (self.bridge.clone(), stop.clone());
                let join = std::thread::Builder::new()
                    .name("argus-api".into())
                    .spawn(move || {
                        server.run_shared(&bridge, &flag);
                        server.stop(&mut bridge.lock().unwrap_or_else(|e| e.into_inner()));
                    })
                    .ok();
                self.api = Some(Api { stop, join, port });
            }
            Err(e) => self.logline(e),
        }
    }

    // ------------------------------------------------------------------ drawing

    fn lcd(&self) -> Lcd {
        let p = theme::palette(self.theme.dark, self.theme.accent);
        Lcd { background: mix(p.accent, Color32::WHITE, 0.62), pixel: mix(p.accent, Color32::BLACK, 0.85) }
    }

    /// The whole window, once a frame. `main` calls it from eframe; the tests call it headless.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.waker.is_none() {
            let c = ctx.clone();
            self.waker = Some(Arc::new(move || c.request_repaint()));
        }
        let pal = self.theme.frame(&ctx);
        let now = ctx.input(|i| i.time);
        self.poll();
        self.scan(now);
        self.sync_stream();
        if matches!(self.stage, Stage::Waiting) {
            ctx.request_repaint_after(Duration::from_secs(1));
        }

        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, pal.bg);
        let origin = full.min;

        // the title strip: name, version, window buttons; dragging it moves the window
        let strip = Rect::from_min_size(origin, vec2(full.width(), STRIP));
        self.title_strip(ui, strip, &pal);

        let card = Rect::from_min_size(origin + vec2(15.0, STRIP), vec2(CARD_W, CARD_H));
        self.card(ui, card, &pal);

        let footer = Rect::from_min_size(pos2(card.left(), card.bottom() + 13.0), vec2(CARD_W, 42.0));
        self.footer(ui, footer, &pal);

        let open = ctx.animate_bool_with_time_and_easing(Id::new("log-drawer"), self.log_open, DRAWER, ease);
        if open > 0.0 {
            let r = Rect::from_min_size(pos2(card.left(), footer.bottom() + 14.0), vec2(CARD_W, (LOG_H - 28.0) * open));
            self.log_drawer(ui, r, &pal);
        }

        self.dialog_ui(&ctx, &pal);
        self.settings_ui(&ctx, &pal);
        if let Some(op) = self.busy {
            if !op.is_big() && op != Op::Captures {
                egui::Modal::new(Id::new("busy")).show(&ctx, |ui| {
                    ui.set_width(300.0);
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(18.0));
                        ui.label(RichText::new("Please wait").strong());
                    });
                    ui.label(op.title());
                });
            }
        }
    }

    fn title_strip(&mut self, ui: &mut egui::Ui, strip: Rect, pal: &Palette) {
        let drag = ui.interact(strip, Id::new("title-drag"), Sense::click_and_drag());
        if drag.drag_started_by(egui::PointerButton::Primary) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        let p = ui.painter();
        p.text(strip.left_center() + vec2(18.0, 0.0), Align2::LEFT_CENTER, "Argus", FontId::proportional(15.0), pal.text);
        p.text(strip.left_center() + vec2(68.0, 1.0), Align2::LEFT_CENTER, VERSION, FontId::proportional(12.0), pal.dim);
        // traffic lights in Yggdrasil's order (minimize, maximize, close)
        let colours = [
            (Color32::from_rgb(0xfe, 0xbc, 0x2e), "Minimize"),
            (Color32::from_rgb(0x28, 0xc8, 0x40), "Maximize"),
            (Color32::from_rgb(0xff, 0x5f, 0x57), "Close"),
        ];
        for (i, (c, tip)) in colours.iter().enumerate() {
            let centre = pos2(strip.right() - 76.0 + i as f32 * 21.0, strip.center().y);
            let r = Rect::from_center_size(centre, vec2(16.0, 16.0));
            let resp = ui.interact(r, Id::new(("window-button", i)), Sense::click()).on_hover_text(*tip);
            resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, *tip));
            ui.painter().circle_filled(centre, 6.5, *c);
            if resp.clicked() {
                let cmd = match i {
                    0 => egui::ViewportCommand::Minimized(true),
                    1 => egui::ViewportCommand::Maximized(!ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false))),
                    _ => egui::ViewportCommand::Close,
                };
                ui.ctx().send_viewport_cmd(cmd);
            }
        }
    }

    fn card(&mut self, ui: &mut egui::Ui, card: Rect, pal: &Palette) {
        let ctx = ui.ctx().clone();
        let p = ui.painter().with_clip_rect(card);
        p.rect_filled(card, 12.0, pal.sidebar);
        // qFlipper's grid, still and faint, in the accent colour
        let grid = Color32::from_rgba_unmultiplied(pal.accent.r(), pal.accent.g(), pal.accent.b(), 14);
        let cell = CARD_W / 30.0;
        let mut x = card.left() + cell;
        while x < card.right() {
            p.line_segment([pos2(x.floor(), card.top()), pos2(x.floor(), card.bottom())], Stroke::new(1.0, grid));
            x += cell;
        }
        let mut y = card.top() + cell;
        while y < card.bottom() {
            p.line_segment([pos2(card.left(), y.floor()), pos2(card.right(), y.floor())], Stroke::new(1.0, grid));
            y += cell;
        }
        p.rect_stroke(card, 12.0, Stroke::new(1.0, pal.border), StrokeKind::Inside);

        let fade = |name: &str, on: bool| ctx.animate_bool_with_time_and_easing(Id::new(("overlay", name)), on, FADE, ease);
        let console = fade("console", self.console);
        let waiting = fade("waiting", matches!(self.stage, Stage::Waiting | Stage::Connecting) && !self.console);
        let home = fade("home", self.stage == Stage::Ready && !self.console);
        let busy = fade("busy", matches!(self.stage, Stage::Busy(_)) && !self.console);
        let finished = fade("finished", matches!(self.stage, Stage::Finished { .. }) && !self.console);
        let error = fade("error", matches!(self.stage, Stage::Error { .. }) && !self.console);
        let stream = fade("stream", self.stage == Stage::Streaming && !self.console);

        // the device view: at home on the right, during an operation further left; it slides between them
        let device_shown = matches!(self.stage, Stage::Ready | Stage::Busy(_) | Stage::Finished { .. }) && !self.console;
        let device_alpha = ctx.animate_bool_with_time_and_easing(Id::new("device-alpha"), device_shown, SLIDE, ease);
        let at_home = ctx.animate_bool_with_time_and_easing(Id::new("device-x"), self.stage == Stage::Ready, SLIDE, ease);
        if device_alpha > 0.0 {
            let x = DEVICE_BUSY_X + (DEVICE_HOME_X - DEVICE_BUSY_X) * at_home;
            let r = Rect::from_min_size(card.min + vec2(x, DEVICE_Y), vec2(DEVICE[0], DEVICE[1]));
            self.layer(ui, "device_alpha", card, device_alpha, device_shown, |app, ui| app.device_view(ui, r, pal));
        }
        if waiting > 0.0 {
            self.layer(ui, "waiting", card, waiting, matches!(self.stage, Stage::Waiting | Stage::Connecting), |app, ui| {
                app.waiting_ui(ui, card, pal)
            });
        }
        if home > 0.0 {
            self.layer(ui, "home", card, home, self.stage == Stage::Ready, |app, ui| app.home_ui(ui, card, pal));
        }
        if busy > 0.0 {
            self.layer(ui, "busy", card, busy, true, |app, ui| app.busy_ui(ui, card, pal));
        }
        if finished > 0.0 {
            self.layer(ui, "finished", card, finished, matches!(self.stage, Stage::Finished { .. }), |app, ui| {
                app.finished_ui(ui, card, pal)
            });
        }
        if error > 0.0 {
            self.layer(ui, "error", card, error, matches!(self.stage, Stage::Error { .. }), |app, ui| app.error_ui(ui, card, pal));
        }
        if stream > 0.0 {
            self.layer(ui, "stream", card, stream, self.stage == Stage::Streaming, |app, ui| app.stream_ui(ui, card, pal));
        }
        if console > 0.0 {
            self.layer(ui, "console", card, console, self.console, |app, ui| app.console_ui(ui, card, pal));
        }
    }

    /// One overlay at `alpha`; an overlay that is fading out takes no input. Each has an explicit id (an `id_salt` is
    /// not enough: egui derives the widgets' automatic ids from the parent's counter), so its widgets keep their ids
    /// while other overlays come and go (a press and its release must meet the same widget).
    fn layer(&mut self, ui: &mut egui::Ui, name: &str, rect: Rect, alpha: f32, active: bool, f: impl FnOnce(&mut Self, &mut egui::Ui)) {
        ui.scope_builder(UiBuilder::new().id(Id::new(("overlay", name))).max_rect(rect), |ui| {
            ui.set_clip_rect(rect);
            ui.multiply_opacity(alpha);
            if !active {
                ui.disable();
            }
            f(self, ui);
        });
    }

    // ---------------------------------------------------------------- the device view

    fn screen_texture(&mut self, ctx: &egui::Context) -> Option<egui::TextureId> {
        let frame = self.frame.as_ref()?;
        if self.texture_frame.as_ref() != Some(frame) || self.texture.is_none() {
            let img = frame_image(frame, &self.lcd(), 1);
            match &mut self.texture {
                Some(t) => t.set(img, TextureOptions::NEAREST),
                None => self.texture = Some(ctx.load_texture("flipper-screen", img, TextureOptions::NEAREST)),
            }
            self.texture_frame = Some(frame.clone());
        }
        self.texture.as_ref().map(TextureHandle::id)
    }

    /// The device, in neutral shapes: a rounded body, the screen (live when the stream runs), a round keypad.
    fn device_view(&mut self, ui: &mut egui::Ui, r: Rect, pal: &Palette) {
        let lcd = self.lcd();
        let tex = if self.stream_wanted { self.screen_texture(ui.ctx()) } else { None };
        let p = ui.painter();
        p.rect_filled(r, 30.0, pal.field);
        p.rect_stroke(r, 30.0, Stroke::new(1.5, pal.border), StrokeKind::Inside);
        let bezel = Rect::from_min_size(r.min + vec2(20.0, 16.0), vec2(256.0 + 16.0, 128.0 + 16.0));
        p.rect_filled(bezel, 10.0, pal.strip);
        let screen = Rect::from_min_size(bezel.min + vec2(8.0, 8.0), vec2(256.0, 128.0));
        p.rect_filled(screen, 3.0, lcd.background);
        match (&self.stage, tex) {
            (Stage::Finished { warning, .. }, _) => {
                let c = screen.center();
                let ink = Stroke::new(6.0, lcd.pixel);
                if warning.is_some() {
                    p.text(c, Align2::CENTER_CENTER, "!", FontId::proportional(64.0), lcd.pixel);
                } else {
                    p.line_segment([c + vec2(-34.0, 0.0), c + vec2(-10.0, 24.0)], ink);
                    p.line_segment([c + vec2(-10.0, 24.0), c + vec2(36.0, -26.0)], ink);
                }
            }
            (_, Some(tex)) => {
                p.image(tex, screen, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            _ => {
                p.text(screen.center(), Align2::CENTER_CENTER, "ARGUS", FontId::monospace(26.0), lcd.pixel);
            }
        }
        // the keypad: a ring with its centre button, and back below it
        let c = pos2(r.right() - 46.0, r.top() + 66.0);
        p.circle_stroke(c, 30.0, Stroke::new(1.5, pal.border));
        p.circle_filled(c, 11.0, pal.strip);
        p.circle_filled(pos2(r.right() - 26.0, r.bottom() - 34.0), 9.0, pal.strip);
        if self.recovery() {
            p.circle_filled(pos2(r.right() - 70.0, r.top() + 18.0), 4.0, pal.blue);
        }
        // the live screen opens full size on a click, as qFlipper's
        if self.stage == Stage::Ready && tex.is_some() {
            let resp = ui.interact(screen, Id::new("open-screen"), Sense::click()).on_hover_text("Open the screen");
            resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Open the screen"));
            if resp.hovered() {
                ui.painter().rect_stroke(screen.expand(2.0), 4.0, Stroke::new(2.0, pal.accent), StrokeKind::Outside);
            }
            if resp.clicked() {
                self.stage = Stage::Streaming;
            }
        }
    }

    // ---------------------------------------------------------------- overlays

    fn waiting_ui(&mut self, ui: &mut egui::Ui, card: Rect, pal: &Palette) {
        let c = card.center();
        let p = ui.painter();
        // a plug: the cable and the connector, plain shapes
        let plug = Rect::from_center_size(c + vec2(0.0, -70.0), vec2(56.0, 34.0));
        p.line_segment([plug.left_center() + vec2(-120.0, 0.0), plug.left_center()], Stroke::new(4.0, pal.dim));
        p.rect_filled(plug, 8.0, pal.field);
        p.rect_stroke(plug, 8.0, Stroke::new(2.0, pal.accent), StrokeKind::Inside);
        p.rect_filled(Rect::from_min_size(plug.right_center() + vec2(0.0, -8.0), vec2(16.0, 16.0)), 3.0, pal.accent);
        label_at(ui, c + vec2(0.0, -6.0), Align2::CENTER_CENTER, "Connect your Flipper Zero", FontId::proportional(24.0), pal.text);
        if self.stage == Stage::Connecting {
            // qFlipper's spinner: one turn in 1.5 s, only while a device is being opened
            let t = ui.ctx().input(|i| i.time);
            let a = (t / 1.5).fract() as f32 * std::f32::consts::TAU;
            let centre = c + vec2(0.0, 40.0);
            for k in 0..8 {
                let ang = a + k as f32 * std::f32::consts::TAU / 8.0;
                let col = Color32::from_rgba_unmultiplied(pal.accent.r(), pal.accent.g(), pal.accent.b(), 40 + k * 26);
                ui.painter().circle_filled(centre + vec2(ang.cos(), ang.sin()) * 14.0, 3.0, col);
            }
            ui.ctx().request_repaint();
        } else if !self.found.is_empty() && (!self.settings.auto_connect || self.found.len() > 1 || self.failed.is_some()) {
            let list = Rect::from_center_size(c + vec2(0.0, 70.0), vec2(460.0, 110.0));
            ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 1))).max_rect(list), |ui| {
                for f in self.found.clone() {
                    ui.horizontal(|ui| {
                        let what = if f.is_recovery() { "Recovery mode" } else { "Flipper Zero" };
                        ui.label(RichText::new(format!("{what} at {}", f.location())).color(pal.dim));
                        if ui.button("Connect").clicked() {
                            self.connect(f.clone());
                        }
                    });
                }
            });
        }
    }

    fn home_ui(&mut self, ui: &mut egui::Ui, card: Rect, pal: &Palette) {
        let name = if self.recovery() { "Recovery mode".to_string() } else { self.device_name() };
        let top = card.top() + 19.0;
        label_at(ui, pos2(card.left() + CENTER_X - 4.0, top + 22.0), Align2::RIGHT_BOTTOM, &name, FontId::proportional(28.0), pal.text);
        let (state, colour) = if self.recovery() { ("Recovery mode", pal.blue) } else { ("Connected", pal.green) };
        label_at(ui, pos2(card.left() + CENTER_X + 6.0, top + 4.0), Align2::LEFT_TOP, state, FontId::proportional(13.0), colour);
        let loc = self.current.as_ref().map(Found::location).unwrap_or_default();
        label_at(ui, pos2(card.left() + CENTER_X + 6.0, top + 20.0), Align2::LEFT_TOP, &loc, FontId::proportional(12.0), pal.dim);

        // the main button and its links; in recovery mode it repairs from local files
        let state = if self.recovery() { UpdateState::CanRepair } else { self.update_state() };
        let fill = match state {
            UpdateState::CanUpdate => pal.green,
            UpdateState::CanRepair => pal.blue,
            _ => pal.accent,
        };
        let button = Rect::from_center_size(pos2(card.left() + CENTER_X, card.top() + 290.0), vec2(220.0, 48.0));
        let enabled = state.enabled() && (self.worker.is_some() || self.recovery());
        let resp = ui.put(
            button,
            egui::Button::new(RichText::new(state.button().to_uppercase()).size(20.0).color(if enabled {
                pal.on_accent()
            } else {
                pal.dim
            }))
            .fill(if enabled { fill } else { pal.field })
            .corner_radius(CornerRadius::same(12)),
        );
        let resp = if state.tooltip().is_empty() { resp } else { resp.on_hover_text(state.tooltip()) };
        if resp.clicked() && enabled {
            if let UpdateState::Error(_) = state {
                self.check_updates();
            } else if state == UpdateState::CanRepair {
                self.ask(Action::Repair);
            } else {
                self.ask_update();
            }
        }
        let links_y = button.bottom() + 16.0;
        if let Some(label) = self.latest_label() {
            let r = Rect::from_min_size(pos2(card.left() + CENTER_X - 206.0, links_y - 9.0), vec2(200.0, 18.0));
            ui.scope_builder(
                UiBuilder::new().id(Id::new(("scope", 2))).max_rect(r).layout(egui::Layout::right_to_left(egui::Align::Center)),
                |ui| {
                    ui.label(RichText::new(label).color(pal.accent));
                },
            );
        }
        if !self.recovery() {
            let r = Rect::from_min_size(pos2(card.left() + CENTER_X + 6.0, links_y - 9.0), vec2(200.0, 18.0));
            ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 3))).max_rect(r), |ui| {
                if ui.add_enabled(self.worker.is_some(), egui::Button::new("Install from file").frame(false)).clicked() {
                    self.ask(Action::InstallFile);
                }
            });
        }

        // the tabs and their pane on the left
        let tabs = Rect::from_min_size(card.min + vec2(28.0, 22.0), vec2(360.0, 34.0));
        ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 4))).max_rect(tabs), |ui| {
            ui.horizontal(|ui| {
                for (t, label) in [(Tab::Info, "Info"), (Tab::Actions, "Actions"), (Tab::Files, "Files")] {
                    let enabled = !(t == Tab::Files && self.recovery());
                    let on = self.tab == t;
                    let b = egui::Button::new(RichText::new(label).color(if on { pal.on_accent() } else { pal.text }))
                        .fill(if on { pal.accent } else { pal.field })
                        .min_size(vec2(80.0, 30.0));
                    if ui.add_enabled(enabled, b).clicked() {
                        self.tab = t;
                    }
                }
            });
        });
        let pane = Rect::from_min_size(card.min + vec2(28.0, 62.0), vec2(372.0, 306.0));
        ui.painter().rect_filled(pane, 12.0, pal.bg);
        ui.painter().rect_stroke(pane, 12.0, Stroke::new(1.0, pal.border), StrokeKind::Inside);
        let inner = pane.shrink(10.0);
        ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 5))).max_rect(inner), |ui| match self.tab {
            Tab::Info => self.info_tab(ui, pal),
            Tab::Actions => self.actions_tab(ui, pal),
            Tab::Files => self.files_tab(ui, pal),
        });
    }

    fn info_tab(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let Some(i) = self.info.clone() else {
            ui.label(RichText::new("Recovery mode").color(pal.blue));
            ui.label(RichText::new("The device is in its bootloader (USB DFU). Repair it from the Actions tab.").color(pal.dim));
            return;
        };
        let stack = match i.stack_type {
            1 => "Full".to_string(),
            2 => "HCI".into(),
            3 => "Lite".into(),
            n => n.to_string(),
        };
        let rows = [
            ("Firmware", i.firmware_shown().to_string()),
            ("Build Date", short_date(&i.build_date)),
            ("SD Card", if i.sd_present { format!("{}% Free", i.sd_free) } else { "Not present".into() }),
            ("Databases", if i.assets_installed { "Installed".into() } else { "Missing".into() }),
            ("Hardware", i.hardware.clone()),
            ("Radio FW", if i.radio_version.is_empty() { "Corrupted".into() } else { format!("{} {stack}", i.radio_version) }),
        ];
        egui::Grid::new("info-grid").num_columns(2).spacing(vec2(18.0, 10.0)).show(ui, |ui| {
            for (k, v) in rows {
                ui.label(RichText::new(k).color(pal.dim).size(15.0));
                ui.label(RichText::new(v).size(15.0));
                ui.end_row();
            }
        });
    }

    fn actions_tab(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        if self.recovery() {
            ui.label(RichText::new("Recovery").color(pal.dim));
            let size = vec2(150.0, 32.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new("Repair…").min_size(size)).on_hover_text("Install firmware from local files.").clicked() {
                    self.ask(Action::Repair);
                }
                if ui.add(egui::Button::new("Leave recovery mode").min_size(size)).clicked() {
                    self.ask(Action::ExitRecovery);
                }
            });
            if let Some(why) = self.current.as_ref().and_then(|f| crate::recovery::unreachable(&f.kind)) {
                ui.label(RichText::new(why).color(pal.amber));
            }
            return;
        }
        let connected = self.worker.is_some() && !self.recovery();
        ui.label(RichText::new("Firmware update channel").color(pal.dim));
        let names: Vec<String> = match &self.directory {
            DirState::Ready(d) => d.channel_names().iter().map(|s| s.to_string()).collect(),
            _ => Vec::new(),
        };
        let mut chosen = self.settings.update_channel.clone();
        ui.add_enabled_ui(!names.is_empty(), |ui| {
            egui::ComboBox::from_id_salt("channel").selected_text(&chosen).width(200.0).show_ui(ui, |ui| {
                for n in &names {
                    ui.selectable_value(&mut chosen, n.clone(), n);
                }
            });
        });
        if chosen != self.settings.update_channel {
            self.settings.update_channel = chosen;
            self.save_settings();
        }
        ui.add_space(10.0);
        ui.label(RichText::new("Backup & Restore").color(pal.dim));
        let size = vec2(150.0, 32.0);
        egui::Grid::new("actions-grid").num_columns(2).spacing(vec2(10.0, 10.0)).show(ui, |ui| {
            if ui
                .add_enabled(connected, egui::Button::new("Backup").min_size(size))
                .on_hover_text("Save the device's internal storage to this computer.")
                .clicked()
            {
                self.ask(Action::Backup);
            }
            if ui
                .add_enabled(connected, egui::Button::new("Restore").min_size(size))
                .on_hover_text("Write a backup back to the device's internal storage.")
                .clicked()
            {
                self.ask(Action::Restore);
            }
            ui.end_row();
            let erase = egui::Button::new(RichText::new("Erase").color(if connected { pal.red } else { pal.dim })).min_size(size);
            if ui
                .add_enabled(connected, erase)
                .on_hover_text("Revert the device to its default settings. All progress will be lost!")
                .clicked()
            {
                self.ask(Action::Erase);
            }
            let reinstall = connected && self.update_state() == UpdateState::NoUpdates;
            if ui
                .add_enabled(reinstall, egui::Button::new("Reinstall").min_size(size))
                .on_hover_text("Install the current firmware version again.")
                .clicked()
            {
                self.ask_update();
                if let Some(d) = &mut self.dialog {
                    d.title = "Reinstall firmware?".into();
                    d.message = "The current firmware version will be reinstalled".into();
                    d.confirm = "Reinstall".into();
                }
            }
            ui.end_row();
        });
    }

    fn files_tab(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let idle = self.busy.is_none() && self.worker.is_some();
        let root = self.files.is_root();
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.files.back.is_empty(), egui::Button::new("<")).on_hover_text("Back").clicked() {
                self.history(true);
            }
            if ui.add_enabled(!self.files.forward.is_empty(), egui::Button::new(">")).on_hover_text("Forward").clicked() {
                self.history(false);
            }
            ui.label(RichText::new(shown_path(&self.files.path)).color(pal.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(idle, egui::Button::new("Refresh")).clicked() {
                    self.files.loaded = None;
                }
                let mut hidden = self.settings.show_hidden;
                if ui.checkbox(&mut hidden, "Hidden").changed() {
                    self.settings.show_hidden = hidden;
                    self.save_settings();
                }
            });
        });
        let selected = self.files.selected.clone().and_then(|n| self.files.entries.iter().find(|e| e.name == n).cloned());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.spacing_mut().button_padding = vec2(5.0, 2.0);
            ui.style_mut().override_font_id = Some(FontId::proportional(13.0));
            let dir = self.files.path.clone();
            if ui.add_enabled(idle && !root, egui::Button::new("Upload…")).clicked() {
                self.ask(Action::Upload { dir: dir.clone() });
            }
            if ui.add_enabled(idle && !root, egui::Button::new("New folder")).clicked() {
                self.ask(Action::Mkdir { dir: dir.clone() });
            }
            let sel = selected.clone();
            if ui.add_enabled(idle && sel.is_some(), egui::Button::new("Download…")).clicked() {
                if let Some(e) = &sel {
                    self.ask(Action::Download { remote: device::join(&dir, &e.name) });
                }
            }
            let changeable = idle && !root && sel.is_some();
            if ui.add_enabled(changeable, egui::Button::new("Rename")).clicked() {
                if let Some(e) = &sel {
                    self.ask(Action::Rename { dir: dir.clone(), old: e.name.clone() });
                }
            }
            if ui
                .add_enabled(changeable, egui::Button::new(RichText::new("Delete…").color(if changeable { pal.red } else { pal.dim })))
                .clicked()
            {
                if let Some(e) = &sel {
                    self.ask(Action::Delete { path: device::join(&dir, &e.name), dir: e.kind == FileKind::Dir });
                }
            }
        });
        ui.separator();
        if let Some(e) = &self.files.error {
            ui.label(RichText::new(e).color(pal.red));
        }
        let entries: Vec<Entry> =
            self.files.entries.iter().filter(|e| self.settings.show_hidden || !e.name.starts_with('.')).cloned().collect();
        let mut open = None;
        egui::ScrollArea::vertical().max_height(ui.available_height() - 22.0).auto_shrink([false, false]).show(ui, |ui| {
            for e in &entries {
                let is_sel = self.files.selected.as_deref() == Some(e.name.as_str());
                let label = if root { storage_name(&e.name).to_string() } else { e.name.clone() };
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
                resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, is_sel, &label));
                if is_sel {
                    ui.painter().rect_filled(rect, 6.0, pal.select);
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 6.0, pal.hover);
                }
                let icon = Rect::from_center_size(rect.left_center() + vec2(14.0, 0.0), vec2(14.0, 11.0));
                if e.kind == FileKind::Dir {
                    ui.painter().rect_filled(icon, 2.0, pal.folder);
                } else {
                    ui.painter().rect_stroke(icon.shrink2(vec2(2.0, 0.0)), 1.0, Stroke::new(1.2, pal.dim), StrokeKind::Inside);
                }
                ui.painter().text(rect.left_center() + vec2(30.0, 0.0), Align2::LEFT_CENTER, &label, FontId::proportional(14.0), pal.text);
                if e.kind == FileKind::File {
                    ui.painter().text(
                        rect.right_center() - vec2(6.0, 0.0),
                        Align2::RIGHT_CENTER,
                        size_text(u64::from(e.size)),
                        FontId::proportional(12.0),
                        pal.dim,
                    );
                }
                if resp.clicked() {
                    self.files.selected = Some(e.name.clone());
                }
                if resp.double_clicked() && e.kind == FileKind::Dir {
                    open = Some(e.name.clone());
                }
            }
            if entries.is_empty() && self.files.error.is_none() {
                ui.label(RichText::new("Empty").color(pal.dim));
            }
        });
        if let Some(n) = &self.note {
            ui.label(RichText::new(n).color(pal.dim).size(12.0));
        }
        if let Some(name) = open {
            let to = device::join(&self.files.path, &name);
            self.cd(to);
        }
        // the Qt file manager's keys
        if self.dialog.is_none() && idle {
            let (back, enter, del, refresh) = ui.ctx().input(|i| {
                (
                    i.key_pressed(Key::Backspace),
                    i.key_pressed(Key::Enter),
                    i.key_pressed(Key::Delete),
                    i.modifiers.command && i.key_pressed(Key::G),
                )
            });
            if back && !self.files.back.is_empty() {
                self.history(true);
            }
            if refresh {
                self.files.loaded = None;
            }
            if let Some(e) = selected {
                if enter && e.kind == FileKind::Dir {
                    self.cd(device::join(&self.files.path, &e.name));
                } else if del && !root {
                    self.ask(Action::Delete { path: device::join(&self.files.path, &e.name), dir: e.kind == FileKind::Dir });
                }
            }
        }
    }

    fn cd(&mut self, to: String) {
        let from = std::mem::replace(&mut self.files.path, to);
        self.files.back.push(from);
        self.files.forward.clear();
        self.files.selected = None;
        self.files.entries.clear();
        self.files.loaded = None;
    }

    fn history(&mut self, back: bool) {
        let (from, to) =
            if back { (&mut self.files.back, &mut self.files.forward) } else { (&mut self.files.forward, &mut self.files.back) };
        if let Some(p) = from.pop() {
            to.push(std::mem::replace(&mut self.files.path, p));
            self.files.selected = None;
            self.files.entries.clear();
            self.files.loaded = None;
        }
    }

    fn busy_ui(&mut self, ui: &mut egui::Ui, card: Rect, pal: &Palette) {
        let Stage::Busy(op) = self.stage else { return };
        label_at(ui, card.center_top() + vec2(0.0, 40.0), Align2::CENTER_CENTER, op.title(), FontId::proportional(26.0), pal.text);
        // indeterminate progress (the library reports none): a bar sweeping once a second, only while working
        let bar = Rect::from_center_size(pos2(card.center().x, card.top() + 290.0), vec2(460.0, 10.0));
        ui.painter().rect_filled(bar, 5.0, pal.field);
        let t = (ui.ctx().input(|i| i.time) % 1.4 / 1.4) as f32;
        let w = 120.0;
        let x0 = bar.left() - w + (bar.width() + w) * ease(t);
        let seg = Rect::from_min_max(pos2(x0.max(bar.left()), bar.top()), pos2((x0 + w).min(bar.right()), bar.bottom()));
        if seg.width() > 0.0 {
            ui.painter().rect_filled(seg, 5.0, pal.accent);
        }
        label_at(
            ui,
            bar.center_bottom() + vec2(0.0, 18.0),
            Align2::CENTER_CENTER,
            "Do not unplug the device",
            FontId::proportional(14.0),
            pal.dim,
        );
        ui.ctx().request_repaint();
    }

    fn finished_ui(&mut self, ui: &mut egui::Ui, card: Rect, pal: &Palette) {
        let Stage::Finished { warning } = self.stage.clone() else { return };
        let title = if warning.is_some() { "Done, with a warning" } else { "Success!" };
        label_at(ui, card.center_top() + vec2(0.0, 40.0), Align2::CENTER_CENTER, title, FontId::proportional(26.0), pal.text);
        let text = Rect::from_center_size(pos2(card.center().x, card.top() + 278.0), vec2(600.0, 40.0));
        ui.scope_builder(
            UiBuilder::new().id(Id::new(("scope", 6))).max_rect(text).layout(egui::Layout::top_down(egui::Align::Center)),
            |ui| {
                if let Some(w) = &warning {
                    ui.label(RichText::new(w).color(pal.amber));
                }
            },
        );
        let b = Rect::from_center_size(pos2(card.center().x, card.top() + 338.0), vec2(200.0, 42.0));
        let resp = ui.put(
            b,
            egui::Button::new(RichText::new("CONTINUE").size(18.0).color(pal.on_accent()))
                .fill(pal.accent)
                .corner_radius(CornerRadius::same(12)),
        );
        if resp.clicked() {
            self.finish();
        }
    }

    fn error_ui(&mut self, ui: &mut egui::Ui, card: Rect, pal: &Palette) {
        let Stage::Error { title, text } = self.stage.clone() else { return };
        let c = card.center();
        ui.painter().circle_stroke(c + vec2(0.0, -110.0), 26.0, Stroke::new(3.0, pal.red));
        ui.painter().text(c + vec2(0.0, -110.0), Align2::CENTER_CENTER, "!", FontId::proportional(30.0), pal.red);
        label_at(ui, c + vec2(0.0, -56.0), Align2::CENTER_CENTER, &title, FontId::proportional(26.0), pal.text);
        let r = Rect::from_center_size(c + vec2(0.0, 10.0), vec2(620.0, 70.0));
        ui.scope_builder(
            UiBuilder::new().id(Id::new(("scope", 7))).max_rect(r).layout(egui::Layout::top_down(egui::Align::Center)),
            |ui| {
                ui.label(RichText::new(text).color(pal.dim));
            },
        );
        let b = Rect::from_center_size(c + vec2(0.0, 120.0), vec2(160.0, 38.0));
        if ui.put(b, egui::Button::new(RichText::new("Back").size(16.0)).corner_radius(CornerRadius::same(10))).clicked() {
            self.finish();
        }
    }

    fn stream_ui(&mut self, ui: &mut egui::Ui, card: Rect, pal: &Palette) {
        let lcd = self.lcd();
        let tex = self.screen_texture(ui.ctx());
        let frame = Rect::from_min_size(card.min + vec2(32.0, 36.0), vec2(512.0 + 20.0, 256.0 + 20.0));
        ui.painter().rect_filled(frame, 14.0, pal.field);
        ui.painter().rect_stroke(frame, 14.0, Stroke::new(1.0, pal.border), StrokeKind::Inside);
        let screen = Rect::from_min_size(frame.min + vec2(10.0, 10.0), vec2(512.0, 256.0));
        ui.painter().rect_filled(screen, 4.0, lcd.background);
        if let Some(tex) = tex {
            ui.painter().image(tex, screen, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        // the keypad, as qFlipper's DirectionalKeypad: a cross with OK in the middle, Back below right
        let c = pos2(card.right() - 150.0, card.top() + 164.0);
        let now = ui.ctx().input(|i| i.time);
        let keys = [
            (InputKey::Up, c + vec2(0.0, -62.0), "Up"),
            (InputKey::Left, c + vec2(-62.0, 0.0), "Left"),
            (InputKey::Ok, c, "OK"),
            (InputKey::Right, c + vec2(62.0, 0.0), "Right"),
            (InputKey::Down, c + vec2(0.0, 62.0), "Down"),
            (InputKey::Back, c + vec2(74.0, 118.0), "Back key"),
        ];
        ui.painter().circle_filled(c, 96.0, pal.field);
        ui.painter().circle_stroke(c, 96.0, Stroke::new(1.0, pal.border));
        let mut pressed_now = None;
        let mut released = false;
        for (key, centre, label) in keys {
            let r = Rect::from_center_size(centre, vec2(50.0, 50.0));
            let resp = ui.interact(r, Id::new(("key", label)), Sense::click_and_drag());
            resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
            let down = resp.is_pointer_button_down_on();
            let held = self.keypad.down.map(|d| d.key) == Some(key);
            let fill = if down || held {
                pal.accent
            } else if resp.hovered() {
                pal.hover
            } else {
                pal.bg
            };
            ui.painter().circle_filled(centre, 24.0, fill);
            ui.painter().circle_stroke(centre, 24.0, Stroke::new(1.0, pal.border));
            let ink = if down || held { pal.on_accent() } else { pal.text };
            draw_key_glyph(ui.painter(), key, centre, ink);
            if down && self.keypad.down.is_none() {
                pressed_now = Some((key, true));
            }
            if held && self.keypad.down.is_some_and(|d| d.pointer) && !ui.ctx().input(|i| i.pointer.any_down()) {
                released = true;
            }
        }
        // the keyboard, as qFlipper's (arrows, HJKL, WASD; Enter, Space, E, Z for OK; Q, X, Escape, Backspace for back)
        let events = ui.ctx().input(|i| i.events.clone());
        for e in events {
            if let egui::Event::Key { key, pressed, repeat: false, .. } = e {
                if self.dialog.is_some() {
                    continue;
                }
                if let Some(k) = keyboard_key(key) {
                    if pressed && self.keypad.down.is_none() && pressed_now.is_none() {
                        pressed_now = Some((k, false));
                    } else if !pressed && self.keypad.down.is_some_and(|d| d.key == k && !d.pointer) {
                        released = true;
                    }
                }
            }
        }
        if let Some((key, pointer)) = pressed_now {
            self.keypad.down = Some(Press { key, since: now, long: false, last: now, pointer });
            self.send(Cmd::Input(key, InputType::Press));
        }
        if let Some(mut d) = self.keypad.down {
            if released {
                self.release_keypad();
            } else if !d.long && now - d.since >= LONG_PRESS {
                self.send(Cmd::Input(d.key, InputType::Long));
                d.long = true;
                d.last = now;
                self.keypad.down = Some(d);
                ui.ctx().request_repaint_after(Duration::from_secs_f64(REPEAT));
            } else if d.long && now - d.last >= REPEAT {
                self.send(Cmd::Input(d.key, InputType::Repeat));
                d.last = now;
                self.keypad.down = Some(d);
                ui.ctx().request_repaint_after(Duration::from_secs_f64(REPEAT));
            } else {
                let due = if d.long { d.last + REPEAT } else { d.since + LONG_PRESS };
                ui.ctx().request_repaint_after(Duration::from_secs_f64((due - now).max(0.0)));
            }
        }

        let row = Rect::from_min_size(pos2(frame.left(), frame.bottom() + 16.0), vec2(frame.width(), 34.0));
        ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 8))).max_rect(row), |ui| {
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new("Back").min_size(vec2(90.0, 32.0))).clicked() {
                    self.release_keypad();
                    self.stage = Stage::Ready;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(self.frame.is_some(), egui::Button::new("Save Screenshot").min_size(vec2(140.0, 32.0))).clicked() {
                        self.ask(Action::SaveScreenshot);
                    }
                    if ui.add_enabled(self.frame.is_some(), egui::Button::new("Copy").min_size(vec2(70.0, 32.0))).clicked() {
                        if let Some(f) = &self.frame {
                            ui.ctx().copy_image(frame_image(f, &lcd, 4));
                        }
                    }
                });
            });
        });
    }

    /// The end of a press: short when it was released before the long press, then the release.
    fn release_keypad(&mut self) {
        if let Some(d) = self.keypad.down.take() {
            if !d.long {
                self.send(Cmd::Input(d.key, InputType::Short));
            }
            self.send(Cmd::Input(d.key, InputType::Release));
        }
    }

    // ---------------------------------------------------------------- the console (captures and the Sarina API)

    fn console_ui(&mut self, ui: &mut egui::Ui, card: Rect, pal: &Palette) {
        ui.painter().rect_filled(card, 12.0, pal.sidebar);
        let inner = card.shrink(18.0);
        let connected = self.worker.is_some() && self.info.is_some();
        let pulling = self.busy == Some(Op::Captures);
        let (session, datastore) = {
            let b = self.bridge.lock().unwrap_or_else(|e| e.into_inner());
            (b.session().map(|(_, n)| n.to_string()), b.datastore_path().to_string())
        };
        ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 9))).max_rect(inner), |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Console").size(20.0));
                ui.label(
                    RichText::new(match &session {
                        Some(n) => format!("Session: {n}"),
                        None => "No active session".into(),
                    })
                    .color(pal.dim),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if self.api.is_some() { "Stop API" } else { "Start API" };
                    if ui.button(label).clicked() {
                        self.toggle_api();
                    }
                    let (text, colour) = match self.api_port() {
                        Some(p) => (format!("API :{p}"), pal.green),
                        None => ("API off".to_string(), pal.dim),
                    };
                    ui.label(RichText::new(text).color(colour));
                });
            });
            ui.label(RichText::new(format!("Datastore: {datastore}")).color(pal.dim).size(12.0));
            ui.horizontal(|ui| {
                let text = if session.is_some() { "End session" } else { "Start session" };
                if ui.button(text).clicked() {
                    let mut b = self.bridge.lock().unwrap_or_else(|e| e.into_inner());
                    if session.is_some() {
                        b.end_session();
                    } else {
                        b.start_session(&session_name(), "");
                    }
                }
                ui.add_space(12.0);
                let can = connected && !pulling && self.busy.is_none();
                let all = if pulling { "Pulling…" } else { "Pull all captures" };
                for (label, roots) in [
                    (all, argus_capture::sync::CAPTURE_ROOTS.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
                    ("Pull NFC", vec!["/ext/nfc".to_string()]),
                    ("Pull Sub-GHz", vec!["/ext/subghz".to_string()]),
                ] {
                    if ui.add_enabled(can, egui::Button::new(label)).clicked() {
                        let what = if roots.len() > 1 { "all capture folders".to_string() } else { roots[0].clone() };
                        self.busy = Some(Op::Captures);
                        self.send(Cmd::Captures { roots, label: what });
                    }
                }
            });
            ui.separator();
            let rows = self.bridge.lock().unwrap_or_else(|e| e.into_inner()).capture_rows(200);
            let lines: Vec<(String, String, Color32)> = {
                let b = self.bridge.lock().unwrap_or_else(|e| e.into_inner());
                b.event_log()
                    .tail(200)
                    .iter()
                    .map(|e| {
                        let mut msg = String::from_utf16_lossy(&e.message);
                        let detail = String::from_utf16_lossy(&e.detail);
                        if !detail.is_empty() {
                            msg.push_str("  —  ");
                            msg.push_str(&detail);
                        }
                        let colour = match e.severity {
                            Severity::Error | Severity::Critical => pal.red,
                            Severity::Warning => pal.amber,
                            Severity::Notice => pal.green,
                            _ => pal.text,
                        };
                        let _ = severity_name(e.severity);
                        (source_name(e.source).to_uppercase(), msg, colour)
                    })
                    .collect()
            };
            let h = ui.available_height() - 40.0;
            ui.columns(2, |cols| {
                cols[0].label(RichText::new(format!("Captures ({})", rows.len())).color(pal.dim));
                egui::ScrollArea::vertical().id_salt("captures").max_height(h - 20.0).auto_shrink([false, false]).show(
                    &mut cols[0],
                    |ui| {
                        for r in &rows {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&r.kind).color(pal.accent).size(12.0));
                                ui.label(RichText::new(r.name.clone().unwrap_or_default()).size(12.0));
                                ui.label(RichText::new(size_text(r.size.unwrap_or(0).max(0) as u64)).color(pal.dim).size(12.0));
                            });
                        }
                        if rows.is_empty() {
                            ui.label(RichText::new("No captures yet").color(pal.dim));
                        }
                    },
                );
                cols[1].label(RichText::new("Events").color(pal.dim));
                egui::ScrollArea::vertical().id_salt("events").max_height(h - 20.0).stick_to_bottom(true).auto_shrink([false, false]).show(
                    &mut cols[1],
                    |ui| {
                        for (src, msg, colour) in &lines {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(src).color(pal.dim).size(11.0));
                                ui.label(RichText::new(msg).color(*colour).size(12.0));
                            });
                        }
                    },
                );
            });
            ui.horizontal(|ui| {
                let field = ui
                    .add(egui::TextEdit::singleline(&mut self.console_note).hint_text("Log a note…").desired_width(inner.width() - 150.0));
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if ui.button("Add").clicked() || enter {
                    let text = self.console_note.trim().to_string();
                    if !text.is_empty() {
                        self.bridge.lock().unwrap_or_else(|e| e.into_inner()).append(Severity::Info, LogSource::User, &text, "");
                        self.console_note.clear();
                    }
                }
                if ui.button("Clear").clicked() {
                    self.bridge.lock().unwrap_or_else(|e| e.into_inner()).clear_log();
                }
            });
        });
    }

    // ---------------------------------------------------------------- footer, log, dialogs

    fn footer(&mut self, ui: &mut egui::Ui, r: Rect, pal: &Palette) {
        ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 10))).max_rect(r), |ui| {
            ui.horizontal_centered(|ui| {
                let size = vec2(100.0, r.height());
                for (label, which) in [("LOGS", 0), ("CONSOLE", 1), ("SETTINGS", 2)] {
                    let on = match which {
                        0 => self.log_open,
                        1 => self.console,
                        _ => self.settings_open,
                    };
                    let b = egui::Button::new(RichText::new(label).color(if on { pal.on_accent() } else { pal.text }))
                        .fill(if on { pal.accent } else { pal.field })
                        .min_size(size)
                        .corner_radius(CornerRadius::same(10));
                    if ui.add(b).clicked() {
                        match which {
                            0 => {
                                self.log_open = !self.log_open;
                                let h = WINDOW[1] + if self.log_open { LOG_H } else { 0.0 };
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(WINDOW[0], h)));
                            }
                            1 => self.console = !self.console,
                            _ => self.settings_open = !self.settings_open,
                        }
                    }
                }
                // the status bar, red while something went wrong (its colour changes in 150 ms, OutQuad)
                let (text, bad) = self.status_text();
                let t = ui.ctx().animate_bool_with_time_and_easing(Id::new("status-bad"), bad, FADE, easing::quadratic_out);
                let rest = ui.available_rect_before_wrap();
                let fill = mix(pal.field, pal.red, 0.35 * t);
                ui.painter().rect_filled(rest, 10.0, fill);
                ui.painter().rect_stroke(rest, 10.0, Stroke::new(1.0, pal.border), StrokeKind::Inside);
                ui.painter().text(rest.left_center() + vec2(14.0, 0.0), Align2::LEFT_CENTER, &text, FontId::proportional(14.0), pal.text);
                let resp = ui.allocate_rect(rest, Sense::hover());
                resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &text));
            });
        });
    }

    /// qFlipper's status bar text.
    pub fn status_text(&self) -> (String, bool) {
        let s = match &self.stage {
            Stage::Error { title, .. } => return (title.clone(), true),
            Stage::Waiting => "Waiting for devices ...",
            Stage::Connecting => "Connecting ...",
            Stage::Busy(_) => "Do not unplug the device ...",
            Stage::Finished { warning: Some(_), .. } => "Operation has finished, but not all of it: see the warning.",
            Stage::Finished { .. } => "Operation has finished successfully.",
            Stage::Ready | Stage::Streaming => match &self.directory {
                DirState::Error(_) if !self.recovery() => return ("Cannot connect to update server".into(), true),
                DirState::Checking => "Checking for firmware updates...",
                _ => "Ready.",
            },
        };
        (s.to_string(), false)
    }

    fn log_drawer(&mut self, ui: &mut egui::Ui, r: Rect, pal: &Palette) {
        ui.painter().rect_filled(r, 10.0, pal.field);
        ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 11))).max_rect(r.shrink(8.0)), |ui| {
            ui.set_clip_rect(r);
            egui::ScrollArea::vertical().id_salt("log").stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                for l in &self.log {
                    ui.label(RichText::new(l).monospace().size(12.0).color(pal.dim));
                }
            });
        });
    }

    fn dialog_ui(&mut self, ctx: &egui::Context, pal: &Palette) {
        let Some(mut d) = self.dialog.take() else { return };
        let mut result: Option<bool> = None;
        let resp = egui::Modal::new(Id::new("confirm")).show(ctx, |ui| {
            let t = ctx.animate_bool_with_time_and_easing(Id::new("confirm-fade"), true, FADE, ease);
            ui.multiply_opacity(t);
            ui.set_width(420.0);
            ui.label(RichText::new(&d.title).size(20.0).strong());
            ui.add_space(4.0);
            ui.label(&d.message);
            if let Some(w) = &d.warning {
                ui.add_space(4.0);
                ui.label(RichText::new(w).color(pal.amber));
            }
            for (i, f) in d.fields.iter_mut().enumerate() {
                ui.add_space(6.0);
                ui.label(RichText::new(&f.label).color(pal.dim));
                ui.add(egui::TextEdit::singleline(&mut f.value).id(Id::new(("dialog-field", i))).desired_width(f32::INFINITY));
            }
            if let Some(e) = &d.error {
                ui.add_space(4.0);
                ui.label(RichText::new(e).color(pal.red));
            }
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (fill, text) = if d.danger { (pal.red, Color32::WHITE) } else { (pal.accent, pal.on_accent()) };
                if ui.add(egui::Button::new(RichText::new(&d.confirm).color(text)).fill(fill).min_size(vec2(100.0, 32.0))).clicked() {
                    result = Some(true);
                }
                if ui.add(egui::Button::new("Cancel").min_size(vec2(100.0, 32.0))).clicked() {
                    result = Some(false);
                }
            });
        });
        if resp.should_close() && result.is_none() {
            result = Some(false);
        }
        match result {
            Some(true) => {
                let fields = d.fields.clone();
                match self.execute(d.action.clone(), &fields) {
                    Ok(()) => {
                        ctx.animate_bool_with_time(Id::new("confirm-fade"), false, 0.0);
                    }
                    Err(e) => {
                        d.error = Some(e);
                        self.dialog = Some(d);
                    }
                }
            }
            Some(false) => {
                ctx.animate_bool_with_time(Id::new("confirm-fade"), false, 0.0);
            }
            None => self.dialog = Some(d),
        }
    }

    fn settings_ui(&mut self, ctx: &egui::Context, pal: &Palette) {
        if !self.settings_open {
            return;
        }
        let mut s = self.settings.clone();
        let resp = egui::Modal::new(Id::new("settings")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(RichText::new("Settings").size(20.0).strong());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Style");
                for (v, label) in [(Style::System, "Follow the system"), (Style::Light, "Light"), (Style::Dark, "Dark")] {
                    ui.radio_value(&mut s.style, v, label);
                }
            });
            ui.checkbox(&mut s.auto_connect, "Connect by itself when one device is found");
            ui.checkbox(&mut s.check_updates, "Look for firmware updates when a device connects");
            ui.checkbox(&mut s.show_hidden, "Show hidden files");
            ui.horizontal(|ui| {
                ui.label("Control API port");
                ui.add(egui::DragValue::new(&mut s.api_port).range(1024..=65535));
            });
            ui.label(RichText::new(format!("Update server: {}", self.cfg.net.server)).color(pal.dim).size(12.0));
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(RichText::new("Done").color(pal.on_accent())).fill(pal.accent).min_size(vec2(90.0, 30.0)))
                    .clicked()
                {
                    self.settings_open = false;
                }
            });
        });
        if resp.should_close() {
            self.settings_open = false;
        }
        if s != self.settings {
            self.settings = s;
            self.save_settings();
        }
    }
}

impl Drop for ArgusApp {
    fn drop(&mut self) {
        self.api = None;
        self.worker = None;
    }
}

/// Painted text that is also a label for assistive technology (and for the tests, which find widgets the same way).
fn label_at(ui: &egui::Ui, pos: egui::Pos2, align: Align2, text: &str, font: FontId, color: Color32) -> Rect {
    let r = ui.painter().text(pos, align, text, font, color);
    let resp = ui.interact(r, ui.id().with(("text", text)), Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, ui.is_enabled(), text));
    r
}

fn style_source(style: Style, ygg: Option<PathBuf>) -> theme::Source {
    match style {
        Style::Light => theme::Source::Fixed { dark: false, accent: None },
        Style::Dark => theme::Source::Fixed { dark: true, accent: None },
        Style::System => match ygg {
            Some(d) => theme::Source::System(d),
            None => theme::Source::Fixed { dark: true, accent: None },
        },
    }
}

fn error_title(op: Op) -> &'static str {
    match op {
        Op::Backup => "Backup Failed",
        Op::Restore => "Restore Failed",
        Op::Update => "Update Failed",
        Op::Repair => "Repair Failed",
        _ => "Operation Error",
    }
}

fn field(label: &str, value: PathBuf) -> Field {
    Field { label: label.into(), value: value.display().to_string() }
}

/// `yyyyMMdd-HHmmss` in UTC (file names; the Qt app uses local time).
fn stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (y, m, d) = firmware::civil_date(secs);
    let s = secs.rem_euclid(86_400);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", s / 3600, s / 60 % 60, s % 60)
}

fn session_name() -> String {
    let st = stamp();
    format!("{}-{}-{} {}:{}", &st[0..4], &st[4..6], &st[6..8], &st[9..11], &st[11..13])
}

/// The device path as the Qt file manager shows it: `/ext/x` is "SD Card/x/", `/int` "Internal Flash/".
pub fn shown_path(path: &str) -> String {
    if path == "/" {
        return "/".into();
    }
    for (prefix, name) in [("/ext", "SD Card"), ("/int", "Internal Flash")] {
        if let Some(rest) = path.strip_prefix(prefix) {
            if rest.is_empty() || rest.starts_with('/') {
                return format!("{name}{rest}/");
            }
        }
    }
    path.to_string()
}

fn storage_name(name: &str) -> &str {
    match name {
        "ext" => "SD Card",
        "int" => "Internal Flash",
        n => n,
    }
}

/// `dd-MM-yyyy` as Qt's C locale short date (`d/M/yy`).
fn short_date(s: &str) -> String {
    let parts: Vec<&str> = s.split('-').collect();
    match parts.as_slice() {
        [d, m, y] if y.len() == 4 => format!("{}/{}/{}", d.trim_start_matches('0'), m.trim_start_matches('0'), &y[2..]),
        _ => s.to_string(),
    }
}

pub fn size_text(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1 << 20 {
        format!("{:.1} KiB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", n as f64 / (1u64 << 20) as f64)
    }
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

fn keyboard_key(k: Key) -> Option<InputKey> {
    Some(match k {
        Key::ArrowLeft | Key::H | Key::A => InputKey::Left,
        Key::ArrowRight | Key::L | Key::D => InputKey::Right,
        Key::ArrowUp | Key::K | Key::W => InputKey::Up,
        Key::ArrowDown | Key::J | Key::S => InputKey::Down,
        Key::Enter | Key::Space | Key::E | Key::Z => InputKey::Ok,
        Key::Q | Key::X | Key::Escape | Key::Backspace => InputKey::Back,
        _ => return None,
    })
}

fn draw_key_glyph(p: &egui::Painter, key: InputKey, c: egui::Pos2, ink: Color32) {
    let tri = |pts: [egui::Vec2; 3]| egui::Shape::convex_polygon(pts.iter().map(|v| c + *v).collect(), ink, Stroke::NONE);
    match key {
        InputKey::Up => {
            p.add(tri([vec2(0.0, -8.0), vec2(-8.0, 6.0), vec2(8.0, 6.0)]));
        }
        InputKey::Down => {
            p.add(tri([vec2(0.0, 8.0), vec2(8.0, -6.0), vec2(-8.0, -6.0)]));
        }
        InputKey::Left => {
            p.add(tri([vec2(-8.0, 0.0), vec2(6.0, 8.0), vec2(6.0, -8.0)]));
        }
        InputKey::Right => {
            p.add(tri([vec2(8.0, 0.0), vec2(-6.0, -8.0), vec2(-6.0, 8.0)]));
        }
        InputKey::Ok => {
            p.circle_filled(c, 7.0, ink);
        }
        InputKey::Back => {
            p.line_segment([c + vec2(6.0, -5.0), c + vec2(-6.0, -5.0)], Stroke::new(2.0, ink));
            p.line_segment([c + vec2(-6.0, -5.0), c + vec2(-2.0, -9.0)], Stroke::new(2.0, ink));
            p.line_segment([c + vec2(6.0, -5.0), c + vec2(6.0, 5.0)], Stroke::new(2.0, ink));
            p.line_segment([c + vec2(6.0, 5.0), c + vec2(-4.0, 5.0)], Stroke::new(2.0, ink));
        }
    }
}

/// The screen's colours: the accent made light for the background, made dark for lit pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lcd {
    pub background: Color32,
    pub pixel: Color32,
}

/// A frame as a picture, `scale` pixels per screen pixel, turned the way the device says it is held.
pub fn frame_image(f: &Frame, lcd: &Lcd, scale: usize) -> egui::ColorImage {
    use flipper_rpc::screen::{HEIGHT, WIDTH};
    let vertical = matches!(f.orientation, 2 | 3);
    let (w, h) = if vertical { (HEIGHT, WIDTH) } else { (WIDTH, HEIGHT) };
    let mut pixels = vec![lcd.background; w * h * scale * scale];
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = match f.orientation {
                1 => (WIDTH - 1 - x, HEIGHT - 1 - y),
                2 => (WIDTH - 1 - y, x),
                3 => (y, HEIGHT - 1 - x),
                _ => (x, y),
            };
            if f.pixel(sx, sy) {
                for dy in 0..scale {
                    for dx in 0..scale {
                        pixels[(y * scale + dy) * w * scale + x * scale + dx] = lcd.pixel;
                    }
                }
            }
        }
    }
    egui::ColorImage::new([w * scale, h * scale], pixels)
}

/// Saves a frame as a PNG at four times its size, as qFlipper's "Save Screenshot". The file must not exist.
pub fn save_png(path: &Path, f: &Frame, lcd: &Lcd) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let img = frame_image(f, lcd, 4);
    let file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o644).open(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), img.size[0] as u32, img.size[1] as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(std::io::Error::other)?;
    let data: Vec<u8> = img.pixels.iter().flat_map(|c| [c.r(), c.g(), c.b()]).collect();
    w.write_image_data(&data).map_err(std::io::Error::other)?;
    w.finish().map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_show_as_in_the_qt_file_manager() {
        assert_eq!(shown_path("/ext"), "SD Card/");
        assert_eq!(shown_path("/ext/nfc"), "SD Card/nfc/");
        assert_eq!(shown_path("/int"), "Internal Flash/");
        assert_eq!(shown_path("/extra"), "/extra");
        assert_eq!(short_date("09-11-2026"), "9/11/26");
    }

    #[test]
    fn frames_turn_with_the_device() {
        let mut data = vec![0u8; 1024];
        data[0] = 1; // (0, 0)
        let lcd = Lcd { background: Color32::WHITE, pixel: Color32::BLACK };
        let f = Frame { data: data.clone(), orientation: 0 };
        let img = frame_image(&f, &lcd, 1);
        assert_eq!(img.pixels[0], Color32::BLACK);
        let flipped = frame_image(&Frame { data, orientation: 1 }, &lcd, 1);
        assert_eq!(*flipped.pixels.last().unwrap(), Color32::BLACK);
        assert_eq!(frame_image(&Frame { data: vec![0; 1024], orientation: 2 }, &lcd, 2).size, [128, 256]);
    }
}
