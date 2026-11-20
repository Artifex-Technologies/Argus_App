//! The window: qFlipper's layout, look and motion (application/components/*.qml, the DefaultAmber style), with the
//! user's accent where qFlipper has orange.
//!
//! - **Layout** as qFlipper's `MainWindow.qml`: the window controls over one content card (830 x 390), a footer with LOGS,
//!   CONSOLE, TOOLS (the Argus tools window, `app/tools_ui.rs`), SETTINGS and the status bar, and a log drawer under it.
//!   In the card, the stages of `ApplicationBackend` each have their overlay: no device (`NoDeviceOverlay`), home with
//!   the tabs on the left and the device on the right (`HomeOverlay`), an operation in progress (`UpdateOverlay`), its
//!   end or an error (`FinishOverlay`, with the partial-success warning) and the full-size screen with the keypad
//!   (`StreamOverlay`).
//! - **Look** is qFlipper's (`theme.rs`, `fonts.rs`, `app/draw.rs`, `app/icons.rs`): black, the grid card with its bright
//!   border, pixel fonts and capitals, outlined buttons, the dotted tab pane, the device drawn from flipper.svg's
//!   geometry, qFlipper's pixel icons; dark only. No Flipper artwork: no wordmark (the app's name is in its place), no
//!   dolphin, no logo.
//! - **Motion** as the QML's Behaviors: overlays and dialogs fade in 150 ms (InOutQuad), the device view slides between
//!   the home position and the operation position in 350 ms (InOutQuad), the log drawer opens in 500 ms (InOutQuad), the
//!   connection spinner turns once in 1.5 s, the progress dots step every 500 ms, the keypad's long press comes after
//!   350 ms and repeats every 150 ms. egui asks for frames only while something moves: an idle window does no work (no
//!   scanline sweep, which qFlipper's grid background runs for ever).
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
    self, emath::easing, pos2, vec2, Align2, Color32, FontId, Id, Key, Rect, RichText, Sense, Stroke, StrokeKind, TextureHandle,
    TextureOptions, UiBuilder, WidgetInfo, WidgetType,
};
use flipper_rpc::ops::{Entry, FileKind};
use flipper_rpc::proto::pb_gui::{InputKey, InputType};
use flipper_rpc::screen::Frame;
use flipper_updates::directory::Directory;

use crate::device::{self, Cmd, DeviceInfo, Event, Op, Waker, Worker};
use crate::discovery::{Discovery, Found};
use crate::firmware::{self, UpdateState};
use crate::fonts;
use crate::net::{Net, NetEvent, PackageSource};
use crate::settings::{Accent, Settings};
use crate::theme::{self, Palette, Theme};

mod draw;
mod icons;
mod picker;
mod tools_ui;

use draw::{Kind, QButton};

pub use picker::PickFor;
use picker::Picker;

/// The app's version: the repository's VERSION file, as the Qt app and argus-tools show it.
pub const VERSION: &str = include_str!("../../../../VERSION").trim_ascii();

/// qFlipper's geometry: the window, the content card, the log drawer.
pub const WINDOW: [f32; 2] = [860.0, 500.0];
const STRIP: f32 = 38.0;
const CARD_W: f32 = 830.0;
const CARD_H: f32 = 390.0;
const LOG_H: f32 = 200.0;
/// Where the device's left edge sits at home and otherwise (card coordinates): at home from the card's middle, as
/// qFlipper's; otherwise centred (216 in qFlipper's 806-wide card).
const DEVICE_HOME_X: f32 = 415.0;
const DEVICE_BUSY_X: f32 = (CARD_W - draw::DEVICE.x) / 2.0;
const DEVICE_Y: f32 = 82.0;
/// Where the name, the main button and its links centre on the home screen.
const CENTER_X: f32 = 595.0;
/// ConfirmationDialog.qml's width.
const DIALOG_W: f32 = 430.0;

/// Grows or shrinks the window for the log drawer. A request for a new size alone is not enough: a compositor that
/// reports every window as tiled, Hyprland among them, makes winit drop it. Minimum and maximum together are kept by
/// floating windows on any compositor, and they keep the size fixed, as qFlipper's (set in this order so the minimum
/// never exceeds the maximum).
pub fn window_size(ctx: &egui::Context, log_open: bool) {
    let size = vec2(WINDOW[0], WINDOW[1] + if log_open { LOG_H } else { 0.0 });
    let bounds = [egui::ViewportCommand::MinInnerSize(size), egui::ViewportCommand::MaxInnerSize(size)];
    for cmd in if log_open { bounds.into_iter().rev().collect::<Vec<_>>() } else { bounds.to_vec() } {
        ctx.send_viewport_cmd(cmd);
    }
    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
}

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
    /// Where the Argus tools keep their data, the console's pulls, and Limbo's socket.
    pub tools: crate::tools::Dirs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Info,
    Actions,
    Files,
    /// qFlipper's developer tab (Settings, developer mode): the wireless stack and FUS installs.
    Developer,
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
    /// The update through the bootloader (`LegacyUpdateOperation`): a device without SD card or with an old protobuf.
    LegacyUpdate {
        channel: String,
    },
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
    /// The repair with the update server's files (recovery mode only; the main button, as qFlipper's).
    Repair,
    /// The repair with local files (recovery mode only).
    RepairFiles,
    ExitRecovery,
    /// The developer tab: a wireless stack image, or a FUS image (a running firmware is rebooted into its bootloader
    /// first).
    InstallRadio,
    InstallFus,
    /// The tools window: the planned app install, the planned restore from a snapshot, deleting a snapshot.
    ToolsInstall,
    ToolsRestore,
    ToolsDeleteSnapshot {
        id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub label: String,
    pub value: String,
    pub kind: FieldKind,
}

/// What a dialog field holds: text, a file to read (Browse… picks one), or a new file (Browse… picks its folder).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Open,
    Save,
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

struct Pending {
    /// The repair, or the legacy update.
    op: Op,
    /// Taken when the steps are sent to the device.
    finish: Option<crate::recovery::Finish>,
    /// The bootloader part's partial success, shown at the end.
    warning: Option<String>,
    /// When the wait for the device started (the frame clock).
    since: Option<f64>,
}

/// How long the device may take to come back after a repair (the CLIs wait as long for the chip).
const COME_BACK: f64 = 30.0;

/// An operation whose first part ran on the firmware (a reboot into the bootloader), waiting for the bootloader to be
/// listed to run its job there (the C++ `StartRecoveryOperation`, which waits as long).
struct ToBoot {
    op: Op,
    job: Option<crate::recovery::Job>,
    /// When the device left (the frame clock); `None` while the firmware still works on the first part.
    since: Option<f64>,
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
    rec_tx: Sender<Result<crate::recovery::Done, String>>,
    rec_rx: Receiver<Result<crate::recovery::Done, String>>,
    /// A repair whose bootloader part is done, waiting for the device to come back for the steps on the firmware side.
    pending: Option<Pending>,
    /// An operation waiting for the device's bootloader after the firmware rebooted into it.
    to_boot: Option<ToBoot>,
    tools_open: bool,
    tools: crate::tools::State,
    tools_tx: Sender<crate::tools::Reply>,
    tools_rx: Receiver<crate::tools::Reply>,
    picker: Option<Picker>,
    keypad: Keypad,
    console_note: String,
}

fn ease(t: f32) -> f32 {
    easing::quadratic_in_out(t)
}

impl ArgusApp {
    pub fn new(cfg: Config) -> ArgusApp {
        let settings = Settings::load(&cfg.config_dir);
        let source = cfg.theme.clone().unwrap_or_else(|| accent_source(settings.accent, cfg.yggdrasil_config.clone()));
        let bridge = Arc::new(Mutex::new(Bridge::new(&cfg.datastore, cfg.library_dir.clone())));
        let (net_tx, net_rx) = mpsc::channel();
        let (rec_tx, rec_rx) = mpsc::channel();
        let (tools_tx, tools_rx) = mpsc::channel();
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
            pending: None,
            to_boot: None,
            tools_open: false,
            tools: crate::tools::State::default(),
            tools_tx,
            tools_rx,
            picker: None,
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
            self.theme.set_source(accent_source(self.settings.accent, self.cfg.yggdrasil_config.clone()));
        }
    }

    // ------------------------------------------------------------------ devices

    fn scan(&mut self, now: f64) {
        if self.pending.is_some() {
            return self.scan_for_finish(now);
        }
        if self.to_boot.is_some() {
            return self.scan_for_bootloader(now);
        }
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

    /// After a repair's bootloader part: the first device that is not in recovery mode is the repaired one (the chip
    /// left its bootloader and started its firmware); connected to by itself, whatever auto-connect says.
    fn scan_for_finish(&mut self, now: f64) {
        if self.worker.is_some() {
            return;
        }
        let Some(p) = &mut self.pending else { return };
        let since = *p.since.get_or_insert(now);
        if now - since > COME_BACK {
            let op = p.op;
            self.pending = None;
            let again = if op == Op::Repair { "repair it once more" } else { "repair it (the backup of /int is kept)" };
            self.stage = Stage::Error {
                title: error_title(op).into(),
                text: format!(
                    "The firmware is written, but the device did not come back within {COME_BACK} s: the databases are not \
                     installed. Connect it again and {again}."
                ),
            };
            return;
        }
        if now - self.last_scan < 1.0 {
            return;
        }
        self.last_scan = now;
        self.found = self.cfg.discovery.scan();
        if let Some(f) = self.found.iter().find(|f| !f.is_recovery()).cloned() {
            self.connect(f);
        }
    }

    /// After the firmware rebooted into its bootloader: the first device in recovery mode is it; the job runs there.
    fn scan_for_bootloader(&mut self, now: f64) {
        if self.worker.is_some() {
            return;
        }
        let Some(t) = &mut self.to_boot else { return };
        let since = *t.since.get_or_insert(now);
        let op = t.op;
        if now - since > COME_BACK {
            self.to_boot = None;
            self.stage = Stage::Error {
                title: error_title(op).into(),
                text: format!("The device did not come back in recovery mode within {COME_BACK} s; nothing was written to the chip."),
            };
            return;
        }
        if now - self.last_scan < 1.0 {
            return;
        }
        self.last_scan = now;
        self.found = self.cfg.discovery.scan();
        let Some(f) = self.found.iter().find(|f| f.is_recovery()).cloned() else { return };
        let Some(job) = self.to_boot.take().and_then(|t| t.job) else { return };
        self.logline(format!("Recovery mode device at {}", f.location()));
        self.current = Some(f.clone());
        let started = match self.waker.clone() {
            Some(w) => crate::recovery::start(f.kind, self.cfg.recovery_poll, job, self.rec_tx.clone(), w),
            None => Err("Not ready yet.".into()),
        };
        if let Err(e) = started {
            self.stage = Stage::Error { title: error_title(op).into(), text: format!("{e} The device is in recovery mode now.") };
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
        if self.pending.is_none() {
            self.stage = Stage::Connecting;
        }
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

    /// Whether a capture pull runs (started from the console or the Sarina API; one at a time, as in the C++ app).
    pub fn pulling(&self) -> bool {
        self.bridge.lock().unwrap_or_else(|e| e.into_inner()).is_pulling()
    }

    /// The console or the tools window covers the card.
    fn covered(&self) -> bool {
        self.console || self.tools_open
    }

    /// What the tools window asked for, as it arrives.
    pub fn tools_state(&self) -> &crate::tools::State {
        &self.tools
    }

    fn poll(&mut self) {
        let replies: Vec<crate::tools::Reply> = self.tools_rx.try_iter().collect();
        for r in replies {
            let done = matches!(r, crate::tools::Reply::Done(_));
            self.tools.apply(r);
            // as the Qt controller: the list again after a snapshot is made or deleted, the search after an index
            if done && self.tools.busy.is_none() {
                match self.tools.tab {
                    crate::tools::Tab::Snapshots => self.refresh_snapshots(),
                    crate::tools::Tab::Captures => self.search_captures(),
                    _ => {}
                }
            }
        }
        // what the device's pulls logged (the API's server thread does the same while it runs)
        self.bridge.lock().unwrap_or_else(|e| e.into_inner()).poll_device();
        let events: Vec<Event> = match &self.worker {
            Some(w) => w.rx.try_iter().collect(),
            None => Vec::new(),
        };
        for e in events {
            self.on_event(e);
        }
        let done: Vec<Result<crate::recovery::Done, String>> = self.rec_rx.try_iter().collect();
        for result in done {
            let Stage::Busy(op) = self.stage else { continue };
            match result {
                Ok(crate::recovery::Done { outcome, finish: Some(finish) }) => {
                    // the chip starts its firmware; the rest runs on the device once it is back
                    self.logline(outcome.summary);
                    self.logline("Waiting for the device to come back for the resources and a restart");
                    self.pending = Some(Pending { op, finish: Some(finish), warning: outcome.warning, since: None });
                    self.current = None;
                    self.last_scan = f64::NEG_INFINITY;
                }
                Ok(d) => self.on_event(Event::Done { op, result: Ok(d.outcome) }),
                Err(e) => self.on_event(Event::Done { op, result: Err(e) }),
            }
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
                NetEvent::LegacyReady(r) => {
                    if self.stage != Stage::Busy(Op::Update) {
                        continue;
                    }
                    match (r, &self.worker) {
                        (Ok(files), Some(_)) => {
                            self.logline(format!("The legacy update: {}", files.radio_note));
                            let info = self.info.clone().unwrap_or_default();
                            // as the C++ app: no backup of /int for the firmware Flipper shipped with
                            let backup = (info.firmware_version != firmware::SHIPPED_VERSION)
                                .then(|| self.cfg.tools.data.join("legacy-backups").join(format!("{}-{}.tgz", info.name, stamp())));
                            if let Some(b) = &backup {
                                self.logline(format!("/int is backed up to {} (kept)", b.display()));
                            }
                            let job = crate::recovery::Job::Legacy { files, backup: backup.clone() };
                            self.to_boot = Some(ToBoot { op: Op::Update, job: Some(job), since: None });
                            self.send(Cmd::ToBootloader { op: Op::Update, backup });
                        }
                        (Ok(_), None) => {
                            self.stage = Stage::Error {
                                title: "Update Failed".into(),
                                text: "The device left before the update could start.".into(),
                            };
                        }
                        (Err(e), _) => self.stage = Stage::Error { title: "Update Failed".into(), text: e },
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
                // the console's and the Sarina API's device, as the C++ app attaches its current device
                if let (Some(w), Some(waker)) = (&self.worker, self.waker.clone()) {
                    let d = w.api_device(info.name.clone(), waker);
                    self.bridge.lock().unwrap_or_else(|e| e.into_inner()).attach_device(d);
                }
                let region = info.region;
                self.info = Some(info);
                if self.stage == Stage::Connecting {
                    self.stage = Stage::Ready;
                }
                self.tab = Tab::Info;
                if let Some((op, finish)) = self.pending.as_mut().and_then(|p| Some((p.op, p.finish.take()?))) {
                    self.send(Cmd::Finish(Box::new(device::FinishJob { op, finish, region, net: self.cfg.net.clone() })));
                    return;
                }
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
            Event::Done { op, mut result } => {
                // the end of a repair's or legacy update's steps on the firmware: the bootloader part's warning stays
                if self.pending.as_ref().is_some_and(|p| p.op == op) {
                    let p = self.pending.take().expect("checked");
                    result = result.map(|o| device::Outcome { warning: p.warning.or(o.warning), ..o });
                }
                // the first part on the firmware failed: nothing goes on in the bootloader
                if result.is_err() && self.to_boot.as_ref().is_some_and(|t| t.op == op) {
                    self.to_boot = None;
                }
                if op != Op::Captures {
                    self.busy = None;
                }
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
                self.bridge.lock().unwrap_or_else(|e| e.into_inner()).detach_device(&why);
                if self.to_boot.is_some() && matches!(self.stage, Stage::Busy(_)) {
                    // rebooted into its bootloader, as asked: the operation goes on there once it is listed
                    self.worker = None;
                    self.info = None;
                    self.frame = None;
                    self.stream_wanted = false;
                    self.current = None;
                    self.directory = DirState::Unknown;
                    self.last_scan = f64::NEG_INFINITY;
                    self.dialog = None;
                    return;
                }
                self.pending = None;
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
                if self.pending.is_some() {
                    // tried again until the wait is over
                    self.current = None;
                    return;
                }
                self.failed = self.current.take();
                self.stage = Stage::Error { title: "Cannot Connect".into(), text: why };
            }
        }
    }

    /// The screen stream runs while the home screen (not the file manager) or the full-size screen shows, as qFlipper's.
    fn sync_stream(&mut self) {
        let want = self.worker.is_some()
            && self.info.is_some()
            && (self.stage == Stage::Streaming || (self.stage == Stage::Ready && self.tab != Tab::Files && !self.covered()));
        if want != self.stream_wanted {
            self.stream_wanted = want;
            self.send(Cmd::Screen(want));
        }
        let listing_due = self.files.loaded.as_deref() != Some(self.files.path.as_str());
        if self.stage == Stage::Ready
            && self.tab == Tab::Files
            && !self.covered()
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
        let channel = self.settings.update_channel.clone();
        let mut warning = firmware::fork_warning(&info);
        // as the C++ FlipperZero::fullUpdate: without SD card (or with an old protobuf) through the bootloader
        let action = if firmware::is_legacy_update(&info) {
            let why =
                if info.sd_present { "The device's firmware is too old for its updater" } else { "There is no SD card in the device" };
            let legacy = format!(
                "{why}: the device restarts into recovery mode and the firmware is written there (the legacy update). /int is \
                 backed up first and put back afterwards."
            );
            warning = Some(match warning {
                Some(fork) => format!("{fork}\n{legacy}"),
                None => legacy,
            });
            Action::LegacyUpdate { channel }
        } else {
            Action::Update(PackageSource::Server { channel })
        };
        self.open_dialog(Dialog {
            title: "Update firmware?".into(),
            message: format!("New firmware {label} will be installed"),
            warning,
            confirm: "Update".into(),
            danger: false,
            fields: Vec::new(),
            action,
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
                fields: vec![save_field("Save to", home.join(format!("{}-backup-{stamp}.tgz", self.device_name())))],
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
                fields: vec![Field { label: "Name".into(), value: "New Folder".into(), kind: FieldKind::Text }],
                action,
                error: None,
            },
            Action::Rename { old, .. } => Dialog {
                title: format!("Rename \"{old}\"?"),
                message: "The new name".into(),
                warning: None,
                confirm: "Rename".into(),
                danger: false,
                fields: vec![Field { label: "Name".into(), value: old.clone(), kind: FieldKind::Text }],
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
                fields: vec![save_field("Save to", home.join(remote.rsplit('/').next().unwrap_or("download")))],
                action,
                error: None,
            },
            Action::SaveScreenshot => Dialog {
                title: "Save screenshot?".into(),
                message: "The screen will be saved as a PNG picture".into(),
                warning: None,
                confirm: "Save".into(),
                danger: false,
                fields: vec![save_field("Save to", home.join(format!("Screenshot-{stamp}.png")))],
                action,
                error: None,
            },
            Action::Repair => Dialog {
                title: "Repair Device?".into(),
                message: format!(
                    "The latest firmware of the {} channel will be downloaded from the update server and installed, with \
                     the wireless stack and the option bytes; then the databases go onto the SD card and the device \
                     restarts. Recovery may erase your progress and settings.",
                    self.settings.update_channel
                ),
                warning: None,
                confirm: "Repair".into(),
                danger: false,
                fields: Vec::new(),
                action,
                error: None,
            },
            Action::RepairFiles => Dialog {
                title: "Repair from files?".into(),
                message: "The firmware from these files will be installed, the option bytes corrected, and the wireless stack \
                          installed when an image is given; with the resources, they then go onto the SD card and the device \
                          restarts. Recovery may erase your progress and settings."
                    .into(),
                warning: None,
                confirm: "Repair".into(),
                danger: false,
                fields: vec![
                    field("Firmware (.dfu)", home.join("")),
                    field("Option bytes (ob.data)", home.join("")),
                    Field { label: "Wireless stack image (.bin, optional)".into(), value: String::new(), kind: FieldKind::Open },
                    Field { label: "Resources (.tgz, optional)".into(), value: String::new(), kind: FieldKind::Open },
                ],
                action,
                error: None,
            },
            Action::InstallRadio => Dialog {
                title: "Install wireless stack?".into(),
                message: "WARNING! This operation can break your Flipper!".into(),
                warning: Some(format!(
                    "{}The image goes below the secure flash start and the FUS installs it; the chip then starts its firmware.",
                    self.restarts_first()
                )),
                confirm: "Install".into(),
                danger: true,
                fields: vec![field("Wireless stack image (.bin)", home.join(""))],
                action,
                error: None,
            },
            Action::InstallFus => Dialog {
                title: "Install FUS?".into(),
                message: "LAST WARNING! This will invalidate your encryption keys! Please reconsider.".into(),
                warning: Some(format!(
                    "{}The image is written at 0x{:08X} and the FUS upgrades itself from it.",
                    self.restarts_first(),
                    flipper_dfu::recovery::FUS_ADDRESS
                )),
                confirm: "Install".into(),
                danger: true,
                fields: vec![field("FUS image (.bin)", home.join(""))],
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
            Action::Update(_) | Action::LegacyUpdate { .. } => return self.ask_update(),
            Action::ToolsInstall | Action::ToolsRestore | Action::ToolsDeleteSnapshot { .. } => return self.ask_tools(action),
        };
        self.open_dialog(d);
    }

    /// The developer installs' note for a running device.
    fn restarts_first(&self) -> &'static str {
        if self.recovery() {
            ""
        } else {
            "The device first restarts into recovery mode. "
        }
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
            Action::LegacyUpdate { channel } => {
                let kind = self.current.as_ref().map(|f| f.kind.clone()).ok_or("No device.")?;
                if let Some(why) = crate::recovery::unreachable_from_firmware(&kind) {
                    return Err(why);
                }
                let info = self.info.clone().ok_or("No device.")?;
                let waker = self.waker.clone().ok_or("Not ready yet.")?;
                self.big(Op::Update);
                // every file fetched and checked (and the region data made) before the device is touched
                self.cfg.net.prepare_legacy(channel, &info, self.net_tx.clone(), waker);
            }
            Action::InstallRadio | Action::InstallFus if !self.recovery() => {
                // from the running firmware: a reboot into the bootloader first, then the same steps there
                let kind = self.current.as_ref().map(|f| f.kind.clone()).ok_or("No device.")?;
                if let Some(why) = crate::recovery::unreachable_from_firmware(&kind) {
                    return Err(why);
                }
                if self.worker.is_none() {
                    return Err("No device.".into());
                }
                let image = crate::recovery::read_image(&path(0)?)?;
                let (op, fus) =
                    if action == Action::InstallFus { (Op::Fus, Some(flipper_dfu::recovery::FUS_ADDRESS)) } else { (Op::Radio, None) };
                self.to_boot = Some(ToBoot { op, job: Some(crate::recovery::Job::Core2 { image, fus }), since: None });
                self.big(op);
                self.send(Cmd::ToBootloader { op, backup: None });
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
            Action::Repair | Action::RepairFiles | Action::ExitRecovery | Action::InstallRadio | Action::InstallFus => {
                let kind = self.current.as_ref().map(|f| f.kind.clone()).ok_or("No device.")?;
                if let Some(why) = crate::recovery::unreachable(&kind) {
                    return Err(why);
                }
                let optional = |i: usize| (!value(i).is_empty()).then(|| PathBuf::from(value(i)));
                let (job, op) = match action {
                    Action::Repair => (
                        crate::recovery::Job::FromServer { net: self.cfg.net.clone(), channel: self.settings.update_channel.clone() },
                        Op::Repair,
                    ),
                    Action::RepairFiles => {
                        let files =
                            crate::recovery::RepairFiles::read(&path(0)?, &path(1)?, optional(2).as_deref(), optional(3).as_deref())?;
                        (crate::recovery::Job::Repair(files), Op::Repair)
                    }
                    Action::InstallRadio => {
                        (crate::recovery::Job::Core2 { image: crate::recovery::read_image(&path(0)?)?, fus: None }, Op::Radio)
                    }
                    Action::InstallFus => (
                        crate::recovery::Job::Core2 {
                            image: crate::recovery::read_image(&path(0)?)?,
                            fus: Some(flipper_dfu::recovery::FUS_ADDRESS),
                        },
                        Op::Fus,
                    ),
                    _ => (crate::recovery::Job::Exit, Op::Exit),
                };
                let waker = self.waker.clone().ok_or("Not ready yet.")?;
                crate::recovery::start(kind, self.cfg.recovery_poll, job, self.rec_tx.clone(), waker)?;
                self.big(op);
            }
            Action::ToolsInstall | Action::ToolsRestore | Action::ToolsDeleteSnapshot { .. } => return self.execute_tools(action, fields),
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
        let p = theme::palette(self.theme.accent);
        Lcd { background: p.accent, pixel: p.dark1 }
    }

    /// The whole window, once a frame. `main` calls it from eframe; the tests call it headless.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.waker.is_none() {
            let c = ctx.clone();
            self.waker = Some(Arc::new(move || c.request_repaint()));
        }
        let Some(pal) = self.theme.frame(&ctx) else {
            ui.painter().rect_filled(ui.max_rect(), 0.0, Color32::BLACK);
            return;
        };
        let now = ctx.input(|i| i.time);
        self.poll();
        self.scan(now);
        self.sync_stream();
        if matches!(self.stage, Stage::Waiting) {
            ctx.request_repaint_after(Duration::from_secs(1));
        }

        // MainWindow.qml: a black window with a dim accent border
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, Color32::BLACK);
        ui.painter().rect_stroke(full, 10.0, Stroke::new(2.0, pal.medium3), StrokeKind::Inside);
        let origin = full.min;

        let card = Rect::from_min_size(origin + vec2(15.0, STRIP), vec2(CARD_W, CARD_H));
        let strip = Rect::from_min_size(origin, vec2(full.width(), STRIP));
        self.title_strip(ui, strip, card, &pal);
        self.card(ui, card, &pal);

        let footer = Rect::from_min_size(pos2(card.left(), card.bottom() + 13.0), vec2(CARD_W, 42.0));
        self.footer(ui, footer, &pal);

        let open = ctx.animate_bool_with_time_and_easing(Id::new("log-drawer"), self.log_open, DRAWER, ease);
        if open > 0.0 {
            // Under the footer when the window grew; if the desktop kept it small, the drawer covers the card from
            // the bottom instead, so the log is never drawn outside the window.
            let h = (LOG_H - 42.0) * open;
            let top = (footer.bottom() + 14.0).min(full.bottom() - 14.0 - h);
            let r = Rect::from_min_size(pos2(card.left(), top), vec2(CARD_W, h));
            if top < footer.bottom() {
                ui.painter().rect_filled(r.expand(6.0), 0.0, Color32::BLACK);
                ui.painter().rect_stroke(r.expand(6.0), 0.0, Stroke::new(2.0, pal.medium3), StrokeKind::Inside);
            }
            self.log_drawer(ui, r, &pal);
        }

        self.dialog_ui(&ctx, &pal);
        self.picker_ui(&ctx, &pal);
        self.settings_ui(&ctx, &pal);
        if let Some(op) = self.busy {
            if !op.is_big() {
                // ProgressDialog.qml: "Please wait" over the window
                modal(Id::new("busy"), &pal).show(&ctx, |ui| {
                    ui.set_width(330.0);
                    dialog_header(ui, "Please wait", &pal);
                    egui::Frame::NONE.inner_margin(20).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
                            let a = (ui.ctx().input(|i| i.time) / 1.5).fract() as f32 * std::f32::consts::TAU;
                            draw::spinner(ui.painter(), r.center(), a, &pal);
                            ui.ctx().request_repaint();
                            ui.label(RichText::new(op.title()).color(pal.accent));
                        });
                    });
                });
            }
        }
    }

    /// WindowControls.qml: minimize and close at the right, over the card's edge; the rest of the strip moves the window.
    fn title_strip(&mut self, ui: &mut egui::Ui, strip: Rect, card: Rect, pal: &Palette) {
        let drag = ui.interact(strip, Id::new("title-drag"), Sense::click_and_drag());
        if drag.drag_started_by(egui::PointerButton::Primary) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        let y = strip.top() + 2.0 + (card.top() - strip.top() - 2.0) / 2.0;
        for (i, (icon, tip)) in [(&icons::MINIMIZE, "Minimize"), (&icons::CLOSE, "Close")].into_iter().enumerate() {
            let r = Rect::from_center_size(pos2(card.right() - 10.0 - (1 - i) as f32 * 32.0, y), vec2(20.0, 20.0));
            let resp = ui.interact(r, Id::new(("window-button", i)), Sense::click());
            resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, tip));
            let down = resp.is_pointer_button_down_on();
            let p = ui.painter();
            // the _hover and _down pictures: filled, the glyph dark; close turns red
            let fill = match (i, down, resp.hovered()) {
                (1, true, _) => Some(pal.red1),
                (1, false, true) => Some(pal.red3),
                (_, true, _) => Some(pal.light1),
                (_, false, true) => Some(pal.accent),
                _ => None,
            };
            if let Some(f) = fill {
                p.rect_filled(r, 0.0, f);
            }
            icon.draw(p, r.center(), if fill.is_some() { pal.dark1 } else { pal.accent });
            if resp.clicked() {
                ui.ctx().send_viewport_cmd(if i == 0 { egui::ViewportCommand::Minimized(true) } else { egui::ViewportCommand::Close });
            }
        }
    }

    fn card(&mut self, ui: &mut egui::Ui, card: Rect, pal: &Palette) {
        let ctx = ui.ctx().clone();
        // GridBackground.qml: a dark gradient, a still grid of 30 cells across (its scanline sweep stays off: an idle
        // window does no work), a 3 px bright border
        let p = ui.painter().with_clip_rect(card);
        draw::gradient_polygon(&p, &draw::rounded(card, 9.0), &[(card.top(), pal.card_top), (card.bottom(), pal.card_bottom)]);
        let inner = card.shrink(3.0);
        let grid = Stroke::new(2.0, pal.grid.gamma_multiply(0.15));
        let cell = inner.width() / 30.0;
        let gp = p.with_clip_rect(inner);
        let mut k = cell;
        while k < inner.width() {
            let x = inner.left() + k.floor();
            gp.line_segment([pos2(x, inner.top()), pos2(x, inner.bottom())], grid);
            k += cell;
        }
        let mut k = cell;
        while k < inner.height() {
            let y = inner.top() + k.floor();
            gp.line_segment([pos2(inner.left(), y), pos2(inner.right(), y)], grid);
            k += cell;
        }
        p.rect_stroke(card, 9.0, Stroke::new(3.0, pal.accent), StrokeKind::Inside);
        // the version, top right, half strength (MainWindow.qml's versionLabel)
        p.text(card.right_top() + vec2(-16.0, 10.0), Align2::RIGHT_TOP, VERSION, fonts::proggy(), pal.accent.gamma_multiply(0.5));

        let fade = |name: &str, on: bool| ctx.animate_bool_with_time_and_easing(Id::new(("overlay", name)), on, FADE, ease);
        let console = fade("console", self.console);
        let tools = fade("tools", self.tools_open && !self.console);
        let waiting = fade("waiting", matches!(self.stage, Stage::Waiting | Stage::Connecting) && !self.covered());
        let home = fade("home", self.stage == Stage::Ready && !self.covered());
        let busy = fade("busy", matches!(self.stage, Stage::Busy(_)) && !self.covered());
        let finished = fade("finished", matches!(self.stage, Stage::Finished { .. }) && !self.covered());
        let error = fade("error", matches!(self.stage, Stage::Error { .. }) && !self.covered());
        let stream = fade("stream", self.stage == Stage::Streaming && !self.covered());

        // the device (DeviceWidget.qml): shown but while streaming or after an error; at home on the right, else
        // centred further left, sliding between the two
        let device_shown = !matches!(self.stage, Stage::Streaming | Stage::Error { .. }) && !self.covered();
        let device_alpha = ctx.animate_bool_with_time_and_easing(Id::new("device-alpha"), device_shown, SLIDE, ease);
        let at_home = ctx.animate_bool_with_time_and_easing(Id::new("device-x"), self.stage == Stage::Ready, SLIDE, ease);
        if device_alpha > 0.0 {
            let x = DEVICE_BUSY_X + (DEVICE_HOME_X - DEVICE_BUSY_X) * at_home;
            let at = card.min + vec2(x.round(), DEVICE_Y);
            self.layer(ui, "device_alpha", card, device_alpha, device_shown, |app, ui| app.device_view(ui, at, pal));
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
        if tools > 0.0 {
            self.layer(ui, "tools", card, tools, self.tools_open && !self.console, |app, ui| app.tools_ui(ui, card, pal));
        }
    }

    /// One overlay at `alpha`; an overlay that is fading out, or under a dialog, takes no input. Each has an explicit id
    /// (an `id_salt` is not enough: egui derives the widgets' automatic ids from the parent's counter), so its widgets
    /// keep their ids while other overlays come and go (a press and its release must meet the same widget).
    fn layer(&mut self, ui: &mut egui::Ui, name: &str, rect: Rect, alpha: f32, active: bool, f: impl FnOnce(&mut Self, &mut egui::Ui)) {
        let under_dialog = self.dialog.is_some() || self.picker.is_some() || self.settings_open;
        ui.scope_builder(UiBuilder::new().id(Id::new(("overlay", name))).max_rect(rect), |ui| {
            ui.set_clip_rect(rect.shrink(3.0));
            ui.multiply_opacity(alpha);
            if !active || under_dialog {
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

    /// The device as qFlipper draws it (flipper.svg's outline, without Flipper's wordmark), its screen live while the
    /// stream runs; a click on the live screen opens it full size.
    fn device_view(&mut self, ui: &mut egui::Ui, at: egui::Pos2, pal: &Palette) {
        let tex = if self.stream_wanted { self.screen_texture(ui.ctx()) } else { None };
        let screen = match (&self.stage, tex) {
            (Stage::Finished { warning: Some(_) }, _) => draw::Screen::Warning,
            (Stage::Finished { warning: None }, _) => draw::Screen::Success,
            _ if self.recovery() => draw::Screen::Recovery,
            (_, Some(t)) => draw::Screen::Live(t),
            _ => draw::Screen::Blank,
        };
        let lcd = draw::device(ui.painter(), at, pal, screen, self.recovery());
        if self.stage == Stage::Ready && tex.is_some() {
            let resp = ui.interact(lcd, Id::new("open-screen"), Sense::click()).on_hover_text("Open the screen");
            resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Open the screen"));
            if resp.hovered() {
                // ExpandWidget.qml: a frame around the screen while the pointer is on it
                ui.painter().rect_stroke(lcd.expand(4.0), 3.0, Stroke::new(2.0, pal.light1), StrokeKind::Middle);
            }
            if resp.clicked() {
                self.stage = Stage::Streaming;
            }
        }
    }

    // ---------------------------------------------------------------- overlays

    /// NoDeviceOverlay.qml: the device, the plug pointing at it, "Connect your Flipper", the spinner while connecting.
    fn waiting_ui(&mut self, ui: &mut egui::Ui, card: Rect, pal: &Palette) {
        draw::plug(ui.painter(), pos2(card.right() - 216.0, card.top() + 145.0), pal);
        caps_at(
            ui,
            pos2(card.center().x, card.top() + 260.0),
            Align2::CENTER_TOP,
            "Connect your Flipper Zero",
            fonts::sporty(fonts::TITLE),
            pal.accent,
        );
        if self.stage == Stage::Connecting {
            // qFlipper's spinner: one turn in 1.5 s, only while a device is being opened
            let a = (ui.ctx().input(|i| i.time) / 1.5).fract() as f32 * std::f32::consts::TAU;
            draw::spinner(ui.painter(), card.right_bottom() + vec2(-43.0, -33.0), a, pal);
            ui.ctx().request_repaint();
        } else if !self.found.is_empty() && (!self.settings.auto_connect || self.found.len() > 1 || self.failed.is_some()) {
            let list = Rect::from_min_size(pos2(card.center().x - 260.0, card.top() + 312.0), vec2(520.0, 70.0));
            ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 1))).max_rect(list), |ui| {
                for f in self.found.clone() {
                    ui.horizontal(|ui| {
                        let what = if f.is_recovery() { "Recovery mode" } else { "Flipper Zero" };
                        ui.label(RichText::new(format!("{what} at {}", f.location())).color(pal.accent));
                        if ui.add(QButton::fit("Connect", Kind::Small { highlighted: true }, pal, 28.0)).clicked() {
                            self.connect(f.clone());
                        }
                    });
                }
            });
        }
    }

    /// HomeOverlay.qml: the tabs and their pane on the left; the name, the connection badges, the main button and its
    /// links over and under the device on the right.
    fn home_ui(&mut self, ui: &mut egui::Ui, card: Rect, pal: &Palette) {
        let name = if self.recovery() { "Recovery mode".to_string() } else { self.device_name() };
        let (state_text, state_colour) = if self.recovery() { ("Recovery mode", pal.blue) } else { ("Connected", pal.green) };
        let loc = self.current.as_ref().map(Found::location).unwrap_or_default();
        // the name ends 4 px left of the centre line, the badges start 4 px right of it; a long location moves both left
        let badge_w = ui.painter().layout_no_wrap(loc.clone(), fonts::proggy(), Color32::WHITE).size().x + 6.0;
        let centre = card.left() + CENTER_X + (card.width() - (CENTER_X + badge_w + 12.0)).min(0.0);
        let top = card.top() + 19.0;
        label_at(ui, pos2(centre - 4.0, top), Align2::RIGHT_TOP, &name, fonts::sporty(fonts::TITLE), pal.accent);
        let baseline = top + 36.0;
        badge(ui, pos2(centre + 4.0, baseline - 35.0), state_text, true, Some(&icons::USB), state_colour);
        badge(ui, pos2(centre + 4.0, baseline - 15.0), &loc, false, None, state_colour);

        // the main button (MainButton.qml): green to update, blue to repair, else the accent; in recovery mode it
        // repairs from the update server
        let state = if self.recovery() { UpdateState::CanRepair } else { self.update_state() };
        let kind = match state {
            UpdateState::CanUpdate => Kind::Green,
            UpdateState::CanRepair => Kind::Blue,
            _ => Kind::Normal,
        };
        let button = Rect::from_min_size(pos2(card.left() + CENTER_X - 140.0, card.top() + 265.0), vec2(280.0, 56.0));
        let enabled = state.enabled() && (self.worker.is_some() || self.recovery()) && !self.pulling();
        let label = state.button().to_uppercase();
        let mut main = QButton::main(&label, kind, pal);
        if matches!(state, UpdateState::Error(_)) {
            main = main.icon(&icons::UPDATE);
        }
        let resp = ui.add_enabled_ui(enabled, |ui| ui.put(button, main)).inner;
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
        // the links under it (LinkButton.qml): the channel's version in the channel's colour, "Install from file"
        let links_y = button.bottom() + 5.0;
        if let Some(text) = self.latest_label() {
            let colour = match self.settings.update_channel.as_str() {
                "release" => pal.green,
                "development" => pal.red,
                "release-candidate" => Color32::from_rgb(0x8a, 0x2b, 0xe2),
                _ => pal.accent,
            };
            let size = draw::link_size(ui, &text);
            let r = Rect::from_min_size(pos2(card.left() + CENTER_X - 6.0 - size.x, links_y), size);
            ui.put(r, QButton::new(&text, Kind::Link(colour), pal, size));
        }
        if !self.recovery() {
            let size = draw::link_size(ui, "Install from file");
            let r = Rect::from_min_size(pos2(card.left() + CENTER_X + 6.0, links_y), size);
            let can = self.worker.is_some();
            if ui.add_enabled_ui(can, |ui| ui.put(r, QButton::new("Install from file", Kind::Link(pal.accent), pal, size))).inner.clicked()
            {
                self.ask(Action::InstallFile);
            }
        }

        // the tabs (TabBar.qml, TabButton.qml): icons, overlapping by their border
        let mut tabs = vec![
            (Tab::Info, "Info", &icons::INFO, "Device information"),
            (Tab::Actions, "Actions", &icons::WRENCH, "Advanced controls"),
            (Tab::Files, "Files", &icons::FILE, "File manager"),
        ];
        if self.settings.developer {
            tabs.push((Tab::Developer, "Developer", &icons::DEVELOPER, "Developer mode. Use with caution!"));
        } else if self.tab == Tab::Developer {
            self.tab = Tab::Info;
        }
        // the pane under them (TabPane.qml), as big as its page; the file manager's is wide and black, over the device
        let (w, h) = match self.tab {
            Tab::Info => (322.0, if self.info.is_some() { 195.0 } else { 104.0 }),
            Tab::Actions => (322.0, if self.recovery() { 196.0 } else { 214.0 }),
            Tab::Files => (749.0, 306.0),
            Tab::Developer => (322.0, 196.0),
        };
        let pane = Rect::from_min_size(card.min + vec2(28.0, 67.0), vec2(w, h));
        draw::pane(ui.painter(), pane, if self.tab == Tab::Files { Color32::BLACK } else { Color32::TRANSPARENT }, pal);
        for (i, (t, label, icon, tip)) in tabs.into_iter().enumerate() {
            let r = Rect::from_min_size(card.min + vec2(28.0 + i as f32 * 45.0, 28.0), vec2(47.0, 41.0));
            let enabled = !(t == Tab::Files && self.recovery());
            if draw::tab(ui, r, icon, label, self.tab == t, enabled, pal).on_hover_text(tip).clicked() {
                self.tab = t;
            }
        }
        let inner = Rect::from_min_max(pane.min + vec2(2.0, 9.0), pane.max - vec2(2.0, 9.0));
        ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 5))).max_rect(inner), |ui| match self.tab {
            Tab::Info => self.info_tab(ui, inner, pal),
            Tab::Actions => self.actions_tab(ui, inner, pal),
            Tab::Files => self.files_tab(ui, inner, pal),
            Tab::Developer => self.developer_tab(ui, inner, pal),
        });
    }

    /// DeviceInfo.qml: labels right-aligned in the dim accent, values in the light one (the firmware row bright).
    fn info_tab(&mut self, ui: &mut egui::Ui, inner: Rect, pal: &Palette) {
        let rows: Vec<(&str, String, bool)> = match self.info.clone() {
            None => vec![("Mode", "Recovery".to_string(), true)],
            Some(i) => {
                let stack = match i.stack_type {
                    1 => "Full".to_string(),
                    2 => "HCI".into(),
                    3 => "Lite".into(),
                    n => n.to_string(),
                };
                vec![
                    ("Firmware", i.firmware_shown().to_string(), true),
                    ("Build Date", short_date(&i.build_date), true),
                    ("SD Card", if i.sd_present { format!("{}% Free", i.sd_free) } else { "Not present".into() }, i.sd_present),
                    ("Databases", if i.assets_installed { "Installed".into() } else { "Missing".into() }, i.assets_installed),
                    ("Hardware", i.hardware.clone(), true),
                    (
                        "Radio FW",
                        if i.radio_version.is_empty() { "Corrupted".into() } else { format!("{} {stack}", i.radio_version) },
                        !i.radio_version.is_empty(),
                    ),
                ]
            }
        };
        let font = fonts::label();
        let width = |t: &str| ui.painter().layout_no_wrap(fonts::pixel_text(t), font.clone(), Color32::WHITE).size().x;
        let lw = rows.iter().map(|r| width(r.0)).fold(0.0, f32::max);
        let vw = rows.iter().map(|r| width(&r.1)).fold(0.0, f32::max);
        let left = inner.left() + ((318.0 - (lw + 30.0 + vw)) / 2.0).floor().max(0.0) + 5.0;
        for (k, (label, value, good)) in rows.iter().enumerate() {
            let y = inner.top() + 10.0 + k as f32 * 27.0;
            let (lc, vc) = if k == 0 { (pal.accent, pal.accent) } else { (pal.medium4, pal.light3) };
            caps_at(ui, pos2(left + lw, y), Align2::RIGHT_TOP, label, font.clone(), lc);
            caps_at(ui, pos2(left + lw + 30.0, y), Align2::LEFT_TOP, value, font.clone(), if *good { vc } else { pal.red3 });
        }
        if self.info.is_none() {
            let r = Rect::from_min_size(pos2(inner.left() + 10.0, inner.top() + 44.0), vec2(inner.width() - 20.0, 40.0));
            ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 12))).max_rect(r), |ui| {
                ui.label(RichText::new("The device is in its bootloader (USB DFU). Repair it from the Actions tab.").color(pal.medium4));
            });
        }
    }

    /// DeviceActions.qml: the update channel, then backup, restore, erase and reinstall as small buttons.
    fn actions_tab(&mut self, ui: &mut egui::Ui, inner: Rect, pal: &Palette) {
        let content = inner.shrink2(vec2(10.0, 10.0));
        let half = vec2((content.width() - 10.0) / 2.0, 34.0);
        let full = vec2(content.width(), 34.0);
        ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 13))).max_rect(content), |ui| {
            ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
            if self.recovery() {
                badge_here(ui, "Recovery", pal.accent);
                if ui
                    .add(QButton::new("Repair…", Kind::Small { highlighted: false }, pal, full).icon(&icons::UPDATE))
                    .on_hover_text("Install the latest firmware from the update server.")
                    .clicked()
                {
                    self.ask(Action::Repair);
                }
                if ui
                    .add(QButton::new("Repair from files…", Kind::Small { highlighted: false }, pal, full).icon(&icons::RESTORE))
                    .on_hover_text("Install firmware from local files.")
                    .clicked()
                {
                    self.ask(Action::RepairFiles);
                }
                if ui.add(QButton::new("Leave recovery mode", Kind::Small { highlighted: false }, pal, full)).clicked() {
                    self.ask(Action::ExitRecovery);
                }
                if let Some(why) = self.current.as_ref().and_then(|f| crate::recovery::unreachable(&f.kind)) {
                    ui.label(RichText::new(why).color(pal.red));
                }
                return;
            }
            // nothing that changes the device starts while a capture pull reads from it
            let connected = self.worker.is_some() && !self.recovery() && !self.pulling();
            badge_here(ui, "Firmware update channel", pal.accent);
            let names: Vec<String> = match &self.directory {
                DirState::Ready(d) => d.channel_names().iter().map(|s| s.to_string()).collect(),
                _ => Vec::new(),
            };
            let mut chosen = self.settings.update_channel.clone();
            ui.add_enabled_ui(!names.is_empty(), |ui| {
                egui::ComboBox::from_id_salt("channel")
                    .selected_text(RichText::new(chosen.to_uppercase()).font(fonts::label()))
                    .width(content.width() - 8.0)
                    .height(200.0)
                    .show_ui(ui, |ui| {
                        for n in &names {
                            ui.selectable_value(&mut chosen, n.clone(), RichText::new(n.to_uppercase()).font(fonts::label()));
                        }
                    })
                    .response
                    .on_hover_text("Change the firmware update channel");
            });
            if chosen != self.settings.update_channel {
                self.settings.update_channel = chosen;
                self.save_settings();
            }
            badge_here(ui, "Backup & Restore", pal.accent);
            let small = Kind::Small { highlighted: false };
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(connected, QButton::new("Backup", small, pal, half).icon(&icons::BACKUP))
                    .on_hover_text("Save the device's internal storage to this computer.")
                    .clicked()
                {
                    self.ask(Action::Backup);
                }
                if ui
                    .add_enabled(connected, QButton::new("Restore", small, pal, half).icon(&icons::RESTORE))
                    .on_hover_text("Write a backup back to the device's internal storage.")
                    .clicked()
                {
                    self.ask(Action::Restore);
                }
            });
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(connected, QButton::new("Erase", Kind::Red, pal, half).icon(&icons::TRASH))
                    .on_hover_text("Revert the device to its default settings. All progress will be lost!")
                    .clicked()
                {
                    self.ask(Action::Erase);
                }
                let reinstall = connected && self.update_state() == UpdateState::NoUpdates;
                if ui
                    .add_enabled(reinstall, QButton::new("Reinstall", small, pal, half).icon(&icons::UPDATE))
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
            });
        });
    }

    /// qFlipper's developer tab (DeveloperActions.qml): the wireless stack and FUS installs, both red, both behind
    /// their dialogs, for a device in recovery mode on a bootloader the app can reach, or a running firmware the app
    /// can follow into its bootloader (it reboots it there first, as the C++ operations do).
    fn developer_tab(&mut self, ui: &mut egui::Ui, inner: Rect, pal: &Palette) {
        let content = inner.shrink2(vec2(10.0, 10.0));
        let why = match &self.current {
            Some(f) if f.is_recovery() => crate::recovery::unreachable(&f.kind),
            Some(f) if self.worker.is_some() => crate::recovery::unreachable_from_firmware(&f.kind),
            _ => Some("No device.".into()),
        };
        let can = why.is_none() && self.stage == Stage::Ready && !self.pulling();
        let size = vec2(content.width(), 34.0);
        ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 14))).max_rect(content), |ui| {
            ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
            badge_here(ui, "Developer Mode", pal.red);
            if ui
                .add_enabled(can, QButton::new("Install wireless fw", Kind::Red, pal, size).icon(&icons::UPDATE))
                .on_hover_text("Install a custom wireless stack. WARNING! This may result in a non-functional Flipper!")
                .clicked()
            {
                self.ask(Action::InstallRadio);
            }
            if ui
                .add_enabled(can, QButton::new("Install FUS firmware", Kind::Red, pal, size).icon(&icons::UPDATE))
                .on_hover_text("Install a custom FUS firmware. !!MEGA WARNING!! It WILL result in encryption keys loss!")
                .clicked()
            {
                self.ask(Action::InstallFus);
            }
            if let Some(w) = why {
                ui.label(RichText::new(w).color(pal.medium4));
            }
        });
    }

    /// FileManager.qml: back, forward and the path on top, the actions, then the list in Share Tech.
    fn files_tab(&mut self, ui: &mut egui::Ui, inner: Rect, pal: &Palette) {
        let content = inner.shrink2(vec2(10.0, 6.0));
        ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 15))).max_rect(content), |ui| self.files_page(ui, pal));
    }

    fn files_page(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let idle = self.busy.is_none() && self.worker.is_some() && !self.pulling();
        let root = self.files.is_root();
        let small = Kind::Small { highlighted: false };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let nav = vec2(30.0, 30.0);
            if ui
                .add_enabled(!self.files.back.is_empty(), QButton::new("<", small, pal, nav).icon(&icons::BACK_SMALL).icon_only())
                .on_hover_text("Back")
                .clicked()
            {
                self.history(true);
            }
            if ui
                .add_enabled(!self.files.forward.is_empty(), QButton::new(">", small, pal, nav).icon(&icons::FORWARD_SMALL).icon_only())
                .on_hover_text("Forward")
                .clicked()
            {
                self.history(false);
            }
            ui.add_space(4.0);
            ui.label(RichText::new(shown_path(&self.files.path)).font(fonts::share(18.0)).color(pal.accent));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(idle, QButton::new("Refresh", small, pal, nav).icon(&icons::REFRESH).icon_only())
                    .on_hover_text("Refresh")
                    .clicked()
                {
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
            ui.spacing_mut().item_spacing.x = 8.0;
            let dir = self.files.path.clone();
            let b = |label: &'static str, kind: Kind| QButton::fit(label, kind, pal, 28.0);
            if ui.add_enabled(idle && !root, b("Upload…", small)).clicked() {
                self.ask(Action::Upload { dir: dir.clone() });
            }
            if ui.add_enabled(idle && !root, b("New folder", small)).clicked() {
                self.ask(Action::Mkdir { dir: dir.clone() });
            }
            let sel = selected.clone();
            if ui.add_enabled(idle && sel.is_some(), b("Download…", small)).clicked() {
                if let Some(e) = &sel {
                    self.ask(Action::Download { remote: device::join(&dir, &e.name) });
                }
            }
            let changeable = idle && !root && sel.is_some();
            if ui.add_enabled(changeable, b("Rename", small)).clicked() {
                if let Some(e) = &sel {
                    self.ask(Action::Rename { dir: dir.clone(), old: e.name.clone() });
                }
            }
            if ui.add_enabled(changeable, b("Delete…", Kind::Red).icon(&icons::TRASH)).clicked() {
                if let Some(e) = &sel {
                    self.ask(Action::Delete { path: device::join(&dir, &e.name), dir: e.kind == FileKind::Dir });
                }
            }
        });
        ui.add_space(2.0);
        if let Some(e) = &self.files.error {
            ui.label(RichText::new(e).color(pal.red));
        }
        let entries: Vec<Entry> =
            self.files.entries.iter().filter(|e| self.settings.show_hidden || !e.name.starts_with('.')).cloned().collect();
        let mut open = None;
        egui::ScrollArea::vertical().max_height(ui.available_height() - 20.0).auto_shrink([false, false]).show(ui, |ui| {
            for e in &entries {
                let is_sel = self.files.selected.as_deref() == Some(e.name.as_str());
                let label = if root { storage_name(&e.name).to_string() } else { e.name.clone() };
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
                resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, is_sel, &label));
                // FileManagerDelegate.qml: the selected row filled with the accent, its text dark
                let (fill, ink) = if is_sel {
                    (Some(pal.accent), pal.dark1)
                } else if resp.hovered() {
                    (Some(pal.medium2), pal.light1)
                } else {
                    (None, pal.accent)
                };
                if let Some(f) = fill {
                    ui.painter().rect_filled(rect, 3.0, f);
                }
                let icon = if e.kind == FileKind::Dir { &icons::FOLDER } else { &icons::FILE_SMALL };
                icon.draw(ui.painter(), rect.left_center() + vec2(16.0, 0.0), ink);
                ui.painter().text(rect.left_center() + vec2(34.0, 0.0), Align2::LEFT_CENTER, &label, fonts::share(16.0), ink);
                if e.kind == FileKind::File {
                    ui.painter().text(
                        rect.right_center() - vec2(8.0, 0.0),
                        Align2::RIGHT_CENTER,
                        size_text(u64::from(e.size)),
                        fonts::share(16.0),
                        if is_sel { pal.dark1 } else { pal.medium4 },
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
                ui.label(RichText::new("Empty").color(pal.medium4));
            }
        });
        if let Some(n) = &self.note {
            ui.label(RichText::new(n).color(pal.medium4));
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

    /// UpdateOverlay.qml: the title over the device, the progress box under it (the library reports no progress, so
    /// qFlipper's indeterminate dots: one more every 500 ms), the message.
    fn busy_ui(&mut self, ui: &mut egui::Ui, card: Rect, pal: &Palette) {
        let Stage::Busy(op) = self.stage else { return };
        label_at(ui, pos2(card.center().x, card.top() + 19.0), Align2::CENTER_TOP, op.title(), fonts::sporty(fonts::TITLE), pal.accent);
        let bar = Rect::from_min_size(pos2((card.center().x - 140.0).round(), card.top() + 270.0), vec2(280.0, 56.0));
        ui.painter().rect_stroke(bar, 9.0, Stroke::new(3.0, pal.accent), StrokeKind::Inside);
        let t = ui.ctx().input(|i| i.time);
        let dots = 1 + (t / 0.5) as usize % 4;
        ui.painter().text(bar.center(), Align2::CENTER_CENTER, ".".repeat(dots), fonts::sporty(fonts::TITLE), pal.accent);
        ui.ctx().request_repaint_after(Duration::from_secs_f64(0.5 - t % 0.5));
        caps_at(ui, pos2(card.center().x, bar.bottom() + 20.0), Align2::CENTER_TOP, "Do not unplug the device", fonts::label(), pal.accent);
    }

    /// FinishOverlay.qml at the end of an operation: the title, the warning of a partial success, CONTINUE.
    fn finished_ui(&mut self, ui: &mut egui::Ui, card: Rect, pal: &Palette) {
        let Stage::Finished { warning } = self.stage.clone() else { return };
        let (title, colour) = if warning.is_some() { ("Done, with a warning", pal.red) } else { ("Success!", pal.accent) };
        label_at(ui, pos2(card.center().x, card.top() + 19.0), Align2::CENTER_TOP, title, fonts::sporty(fonts::TITLE), colour);
        if let Some(w) = &warning {
            let r = Rect::from_center_size(pos2(card.center().x, card.top() + 262.0), vec2(640.0, 36.0));
            ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 6))).max_rect(r), |ui| {
                paragraph(ui, w, w.len() <= 60, pal.red, r.width());
            });
        }
        let b = Rect::from_min_size(pos2((card.center().x - 140.0).round(), card.top() + 300.0), vec2(280.0, 56.0));
        if ui.put(b, QButton::main("CONTINUE", Kind::Normal, pal)).clicked() {
            self.finish();
        }
    }

    /// FinishOverlay.qml after an error: the title over the device showing "!", qFlipper's text box on the right,
    /// BACK at the bottom left.
    fn error_ui(&mut self, ui: &mut egui::Ui, card: Rect, pal: &Palette) {
        let Stage::Error { title, text } = self.stage.clone() else { return };
        let left_w = card.width() - 335.0 - 36.0;
        let cx = card.left() + left_w / 2.0;
        label_at(ui, pos2(cx, card.top() + 36.0), Align2::CENTER_TOP, &title, fonts::sporty(fonts::TITLE), pal.accent);
        draw::device(ui.painter(), pos2((cx - 190.0).round(), card.top() + 110.0), pal, draw::Screen::Error, false);
        // TextBox.qml: Share Tech Mono in a bright outline
        let box_r = Rect::from_min_size(pos2(card.right() - 36.0 - 335.0, card.top() + 90.0), vec2(335.0, 190.0));
        ui.painter().rect_filled(box_r, 6.0, Color32::BLACK);
        ui.painter().rect_stroke(box_r, 6.0, Stroke::new(1.0, pal.accent), StrokeKind::Inside);
        ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 7))).max_rect(box_r.shrink(20.0)), |ui| {
            ui.label(RichText::new(text).font(FontId::monospace(16.0)).color(pal.accent));
        });
        let b = Rect::from_min_size(card.left_bottom() + vec2(25.0, -25.0 - 42.0), vec2(110.0, 42.0));
        if ui.put(b, QButton::new("Back", Kind::Normal, pal, b.size()).icon(&icons::ARROW_BACK)).clicked() {
            self.finish();
        }
    }

    /// StreamOverlay.qml: the screen four times its size in an accent frame, the round keypad (DirectionalKeypad.qml)
    /// on the right, BACK and SAVE SCREENSHOT (and COPY) under the screen.
    fn stream_ui(&mut self, ui: &mut egui::Ui, card: Rect, pal: &Palette) {
        let lcd = self.lcd();
        let tex = self.screen_texture(ui.ctx());
        let screen = Rect::from_min_size(card.min + vec2(38.0, 32.0), vec2(512.0, 256.0));
        let bg = screen.expand(11.0);
        ui.painter().rect_filled(bg, 10.0, lcd.background);
        ui.painter().rect_stroke(bg, 10.0, Stroke::new(3.0, pal.medium1), StrokeKind::Inside);
        if let Some(tex) = tex {
            ui.painter().image(tex, screen, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        // the keypad: a 168 px disc, the four triangles and OK on it; back below it on the right
        let pad = card.min + vec2(590.0, 52.0);
        let c = pad + vec2(84.0, 84.0);
        ui.painter().circle_filled(c, 84.0, pal.dark1);
        ui.painter().circle_stroke(c, 83.0, Stroke::new(2.0, pal.light1));
        let now = ui.ctx().input(|i| i.time);
        let keys = [
            (InputKey::Up, c + vec2(0.0, -51.0), "Up"),
            (InputKey::Left, c + vec2(-51.0, 0.0), "Left"),
            (InputKey::Ok, c, "OK"),
            (InputKey::Right, c + vec2(51.0, 0.0), "Right"),
            (InputKey::Down, c + vec2(0.0, 51.0), "Down"),
            (InputKey::Back, c + vec2(76.0, 112.0), "Back key"),
        ];
        let mut pressed_now = None;
        let mut released = false;
        for (key, centre, label) in keys {
            let r = Rect::from_center_size(centre, vec2(52.0, 52.0));
            let resp = ui.interact(r, Id::new(("key", label)), Sense::click_and_drag());
            resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
            let down = resp.is_pointer_button_down_on();
            let held = self.keypad.down.map(|d| d.key) == Some(key);
            // the _hover and _down pictures: filled dim, or filled bright
            let fill = if down || held {
                Some(pal.accent)
            } else if resp.hovered() {
                Some(pal.medium2)
            } else {
                None
            };
            key_shape(ui.painter(), key, centre, fill, pal);
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

        let y = card.bottom() - 22.0 - 42.0;
        let back = Rect::from_min_size(pos2(bg.left(), y), vec2(110.0, 42.0));
        if ui.put(back, QButton::new("Back", Kind::Normal, pal, back.size()).icon(&icons::ARROW_BACK)).clicked() {
            self.release_keypad();
            self.stage = Stage::Ready;
        }
        let has = self.frame.is_some();
        let save = Rect::from_min_size(pos2(bg.right() - 230.0, y), vec2(230.0, 42.0));
        if ui
            .add_enabled_ui(has, |ui| ui.put(save, QButton::new("Save Screenshot", Kind::Normal, pal, save.size()).icon(&icons::SAVE)))
            .inner
            .clicked()
        {
            self.ask(Action::SaveScreenshot);
        }
        let copy = Rect::from_min_size(pos2(save.left() - 20.0 - 90.0, y), vec2(90.0, 42.0));
        if ui.add_enabled_ui(has, |ui| ui.put(copy, QButton::new("Copy", Kind::Normal, pal, copy.size()))).inner.clicked() {
            if let Some(f) = &self.frame {
                ui.ctx().copy_image(frame_image(f, &lcd, 4));
            }
        }
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

    /// ChymaeraConsole.qml's content in the card: Share Tech Mono, the accent on black.
    fn console_ui(&mut self, ui: &mut egui::Ui, card: Rect, pal: &Palette) {
        cover(ui, card, pal);
        let inner = card.shrink(18.0);
        let connected = self.worker.is_some() && self.info.is_some();
        let pulling = self.pulling();
        let (session, datastore) = {
            let b = self.bridge.lock().unwrap_or_else(|e| e.into_inner());
            (b.session().map(|(_, n)| n.to_string()), b.datastore_path().to_string())
        };
        let small = Kind::Small { highlighted: false };
        ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 9))).max_rect(inner), |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Console").font(fonts::sporty(30.0)).color(pal.accent));
                ui.label(
                    RichText::new(match &session {
                        Some(n) => format!("Session: {n}"),
                        None => "No active session".into(),
                    })
                    .color(pal.medium4),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if self.api.is_some() { "Stop API" } else { "Start API" };
                    if ui.add(QButton::fit(label, Kind::Small { highlighted: true }, pal, 28.0)).clicked() {
                        self.toggle_api();
                    }
                    let (text, colour) = match self.api_port() {
                        Some(p) => (format!("API :{p}"), pal.green),
                        None => ("API off".to_string(), pal.medium4),
                    };
                    ui.label(RichText::new(text).color(colour));
                });
            });
            ui.label(RichText::new(format!("Datastore: {datastore}")).font(FontId::monospace(13.0)).color(pal.medium4));
            ui.horizontal(|ui| {
                let text = if session.is_some() { "End session" } else { "Start session" };
                if ui.add(QButton::fit(text, small, pal, 28.0)).clicked() {
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
                    if ui.add_enabled(can, QButton::fit(label, small, pal, 28.0)).clicked() {
                        // the C++ console's pullAll/pullPath: through the bridge, so the API sees the same pull
                        let what = if roots.len() > 1 { "all capture folders".to_string() } else { roots[0].clone() };
                        self.bridge.lock().unwrap_or_else(|e| e.into_inner()).start_pull(roots, &what);
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
                            _ => pal.accent,
                        };
                        let _ = severity_name(e.severity);
                        (source_name(e.source).to_uppercase(), msg, colour)
                    })
                    .collect()
            };
            let h = ui.available_height() - 40.0;
            let mono = |size: f32| FontId::monospace(size);
            ui.columns(2, |cols| {
                cols[0].label(RichText::new(format!("Captures ({})", rows.len())).color(pal.medium4));
                egui::ScrollArea::vertical().id_salt("captures").max_height(h - 20.0).auto_shrink([false, false]).show(
                    &mut cols[0],
                    |ui| {
                        for r in &rows {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&r.kind).font(mono(14.0)).color(pal.light1));
                                ui.label(RichText::new(r.name.clone().unwrap_or_default()).font(mono(14.0)).color(pal.accent));
                                ui.label(RichText::new(size_text(r.size.unwrap_or(0).max(0) as u64)).font(mono(14.0)).color(pal.medium4));
                            });
                        }
                        if rows.is_empty() {
                            ui.label(RichText::new("No captures yet").color(pal.medium4));
                        }
                    },
                );
                cols[1].label(RichText::new("Events").color(pal.medium4));
                egui::ScrollArea::vertical().id_salt("events").max_height(h - 20.0).stick_to_bottom(true).auto_shrink([false, false]).show(
                    &mut cols[1],
                    |ui| {
                        for (src, msg, colour) in &lines {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(src).font(mono(13.0)).color(pal.medium4));
                                ui.label(RichText::new(msg).font(mono(14.0)).color(*colour));
                            });
                        }
                    },
                );
            });
            ui.horizontal(|ui| {
                let field = ui
                    .add(egui::TextEdit::singleline(&mut self.console_note).hint_text("Log a note…").desired_width(inner.width() - 190.0));
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if ui.add(QButton::fit("Add", small, pal, 28.0)).clicked() || enter {
                    let text = self.console_note.trim().to_string();
                    if !text.is_empty() {
                        self.bridge.lock().unwrap_or_else(|e| e.into_inner()).append(Severity::Info, LogSource::User, &text, "");
                        self.console_note.clear();
                    }
                }
                if ui.add(QButton::fit("Clear", small, pal, 28.0)).clicked() {
                    self.bridge.lock().unwrap_or_else(|e| e.into_inner()).clear_log();
                }
            });
        });
    }

    // ---------------------------------------------------------------- footer, log, dialogs

    /// MainWindow.qml's footer: LOGS (and CONSOLE, TOOLS, SETTINGS, Argus's own) as outlined buttons, then the status
    /// bar (StatusBar.qml).
    fn footer(&mut self, ui: &mut egui::Ui, r: Rect, pal: &Palette) {
        ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 10))).max_rect(r), |ui| self.footer_row(ui, r, pal));
    }

    fn footer_row(&mut self, ui: &mut egui::Ui, r: Rect, pal: &Palette) {
        let mut x = r.left();
        for (label, which, w) in [("LOGS", 0, 110.0), ("CONSOLE", 1, 104.0), ("TOOLS", 3, 90.0), ("SETTINGS", 2, 110.0)] {
            let on = match which {
                0 => false,
                1 => self.console,
                3 => self.tools_open,
                _ => self.settings_open,
            };
            let mut b = QButton::new(label, Kind::Toggle(on), pal, vec2(w, r.height()));
            if which == 0 {
                b = b.icon(if self.log_open { &icons::ARROW_UP } else { &icons::ARROW_DOWN });
            }
            let rect = Rect::from_min_size(pos2(x, r.top()), vec2(w, r.height()));
            x += w + 15.0;
            if ui.put(rect, b).clicked() {
                match which {
                    0 => {
                        self.log_open = !self.log_open;
                        window_size(ui.ctx(), self.log_open);
                    }
                    1 => {
                        self.console = !self.console;
                        self.tools_open = false;
                    }
                    3 => {
                        self.tools_open = !self.tools_open;
                        self.console = false;
                    }
                    _ => self.settings_open = !self.settings_open,
                }
            }
        }
        // the status bar: the dark accent, dark red while something went wrong (its colour changes in 150 ms, OutQuad)
        let (text, bad) = self.status_text();
        let t = ui.ctx().animate_bool_with_time_and_easing(Id::new("status-bad"), bad, FADE, easing::quadratic_out);
        let rest = Rect::from_min_max(pos2(x, r.top()), r.max);
        let p = ui.painter().with_clip_rect(rest);
        p.rect_filled(rest, 0.0, pal.dark1.lerp_to_gamma(pal.red_dark2, t));
        let ink = pal.accent.lerp_to_gamma(pal.red4, t);
        let icon_c = rest.left_center() + vec2(10.0 + 11.0, 0.0);
        if bad {
            // info-small.svg turned upside down: an exclamation mark
            icons::INFO_SMALL.draw_flipped(&p, icon_c, ink);
        } else {
            icons::INFO_SMALL.draw(&p, icon_c, ink);
        }
        // qFlipper's label font when the text fits; a longer one (Argus's statuses can be) in the body font, on two lines
        let room = rest.right() - 10.0 - (icon_c.x + 21.0);
        let mut shown = p.layout_no_wrap(fonts::pixel_text(&text), fonts::label(), ink);
        if shown.size().x > room {
            shown = p.layout(text.to_uppercase(), FontId::proportional(fonts::BODY), ink, room);
        }
        p.galley(pos2(icon_c.x + 21.0, (rest.center().y - shown.size().y / 2.0).round()), shown, ink);
        let resp = ui.allocate_rect(rest, Sense::hover());
        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &text));
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

    /// TextView.qml: the log in Share Tech Mono, the accent on black.
    fn log_drawer(&mut self, ui: &mut egui::Ui, r: Rect, pal: &Palette) {
        ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 11))).max_rect(r), |ui| {
            ui.set_clip_rect(r);
            egui::ScrollArea::vertical().id_salt("log").stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                for l in &self.log {
                    ui.label(RichText::new(l).font(FontId::monospace(fonts::MONO)).color(pal.accent));
                }
            });
        });
    }

    /// ConfirmationDialog.qml (CustomDialog.qml): a black box with a bright outline, the title inverted in its header,
    /// the message in capitals, then OK and Cancel side by side (the suggested one filled; a dangerous OK is red).
    fn dialog_ui(&mut self, ctx: &egui::Context, pal: &Palette) {
        let Some(mut d) = self.dialog.take() else { return };
        let mut result: Option<bool> = None;
        let mut browse: Option<usize> = None;
        let resp = modal(Id::new("confirm"), pal).show(ctx, |ui| {
            let t = ctx.animate_bool_with_time_and_easing(Id::new("confirm-fade"), true, FADE, ease);
            ui.multiply_opacity(t);
            ui.set_width(DIALOG_W);
            dialog_header(ui, &d.title, pal);
            egui::Frame::NONE.inner_margin(egui::Margin { left: 20, right: 20, top: 20, bottom: 20 }).show(ui, |ui| {
                ui.set_width(DIALOG_W - 40.0);
                paragraph(ui, &d.message, d.message.len() <= 60, pal.accent, DIALOG_W - 40.0);
                if let Some(w) = &d.warning {
                    ui.add_space(6.0);
                    paragraph(ui, w, w.len() <= 60, pal.red, DIALOG_W - 40.0);
                }
                for (i, f) in d.fields.iter_mut().enumerate() {
                    ui.add_space(8.0);
                    ui.label(RichText::new(&f.label).color(pal.medium4));
                    if f.kind == FieldKind::Text {
                        ui.add(egui::TextEdit::singleline(&mut f.value).id(Id::new(("dialog-field", i))).desired_width(f32::INFINITY));
                    } else {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut f.value).id(Id::new(("dialog-field", i))).desired_width(DIALOG_W - 180.0),
                            );
                            if ui
                                .add(QButton::fit("Browse…", Kind::Small { highlighted: false }, pal, 26.0))
                                .on_hover_text("Choose the file")
                                .clicked()
                            {
                                browse = Some(i);
                            }
                        });
                    }
                }
                if let Some(e) = &d.error {
                    ui.add_space(6.0);
                    ui.label(RichText::new(e).color(pal.red));
                }
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 30.0;
                    let size = vec2((DIALOG_W - 40.0 - 30.0) / 2.0, 42.0);
                    let ok = if d.danger { Kind::Red } else { Kind::Small { highlighted: true } };
                    if ui.add(QButton::new(&d.confirm, ok, pal, size).radius(7.0)).clicked() {
                        result = Some(true);
                    }
                    if ui.add(QButton::new("Cancel", Kind::Small { highlighted: d.danger }, pal, size).radius(7.0)).clicked() {
                        result = Some(false);
                    }
                });
            });
        });
        // the picker on top: the dialog stays open under it (Escape or a click outside closes the picker, not the dialog)
        if self.picker.is_some() {
            self.dialog = Some(d);
            return;
        }
        if resp.should_close() && result.is_none() {
            result = Some(false);
        }
        if let (Some(i), None) = (browse, result) {
            self.dialog = Some(d);
            return self.open_picker(PickFor::Field(i));
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
        let resp = modal(Id::new("settings"), pal).show(ctx, |ui| {
            ui.set_width(DIALOG_W);
            dialog_header(ui, "Settings", pal);
            egui::Frame::NONE.inner_margin(20).show(ui, |ui| {
                ui.set_width(DIALOG_W - 40.0);
                ui.horizontal(|ui| {
                    ui.label("Accent");
                    for (v, label) in [(Accent::System, "Follow the system"), (Accent::Purple, "Purple")] {
                        ui.radio_value(&mut s.accent, v, label);
                    }
                });
                ui.checkbox(&mut s.auto_connect, "Connect by itself when one device is found");
                ui.checkbox(&mut s.check_updates, "Look for firmware updates when a device connects");
                ui.checkbox(&mut s.show_hidden, "Show hidden files");
                ui.checkbox(&mut s.developer, "Developer mode (the Developer tab)");
                ui.horizontal(|ui| {
                    ui.label("Control API port");
                    ui.add(egui::DragValue::new(&mut s.api_port).range(1024..=65535));
                });
                ui.label(RichText::new(format!("Update server: {}", self.cfg.net.server)).color(pal.medium4));
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(QButton::new("Done", Kind::Small { highlighted: true }, pal, vec2(120.0, 34.0))).clicked() {
                        self.settings_open = false;
                    }
                });
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
        self.bridge.lock().unwrap_or_else(|e| e.into_inner()).detach_device("the app closed");
        self.worker = None;
    }
}

/// Painted text that is also a label for assistive technology (and for the tests, which find widgets the same way).
fn label_at(ui: &egui::Ui, pos: egui::Pos2, align: Align2, text: &str, font: FontId, color: Color32) -> Rect {
    let r = ui.painter().text(pos.round(), align, text, font, color);
    let resp = ui.interact(r, ui.id().with(("text", text)), Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, ui.is_enabled(), text));
    r
}

/// As [`label_at`], drawn in capitals as qFlipper's `TextLabel` (assistive technology reads the text as it is).
fn caps_at(ui: &egui::Ui, pos: egui::Pos2, align: Align2, text: &str, font: FontId, color: Color32) -> Rect {
    let r = ui.painter().text(pos.round(), align, fonts::pixel_text(text), font, color);
    let resp = ui.interact(r, ui.id().with(("text", text)), Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, ui.is_enabled(), text));
    r
}

/// A paragraph wrapped to `width`, taking its place in the layout: in capitals in qFlipper's label font (short texts),
/// or as it is in the body font (the longer messages of Argus's dialogs, which would not fit the window in capitals).
fn paragraph(ui: &mut egui::Ui, text: &str, caps: bool, color: Color32, width: f32) {
    let (shown, font, line) =
        if caps { (fonts::pixel_text(text), fonts::label(), 22.0) } else { (text.to_string(), FontId::proportional(fonts::BODY), 18.0) };
    let mut job = egui::text::LayoutJob::simple(shown, font, color, width);
    job.halign = egui::Align::Center;
    job.sections.iter_mut().for_each(|s| s.format.line_height = Some(line));
    let galley = ui.painter().layout_job(job);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, galley.size().y), Sense::hover());
    ui.painter().galley(pos2(rect.center().x, rect.top()).round(), galley, color);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, ui.is_enabled(), text));
}

/// TransparentLabel.qml: ProggySquare in a colour on a fifth of it, an optional icon after the text; `at` is its top
/// left corner.
fn badge(ui: &egui::Ui, at: egui::Pos2, text: &str, caps: bool, icon: Option<&'static icons::Icon>, colour: Color32) -> Rect {
    let shown = if caps { fonts::pixel_text(text) } else { text.to_string() };
    let p = ui.painter();
    let galley = p.layout_no_wrap(shown, fonts::proggy(), colour);
    let icon_w = icon.map_or(0.0, |i| i.w as f32 + 5.0);
    let r = Rect::from_min_size(at.round(), vec2(galley.size().x + icon_w + 6.0, 15.0));
    p.rect_filled(r, 0.0, colour.gamma_multiply(0.2));
    let text_pos = pos2(r.left() + 3.0, (r.center().y - galley.size().y / 2.0).round() + 1.0);
    let text_w = galley.size().x;
    p.galley(text_pos, galley, colour);
    if let Some(i) = icon {
        i.draw(p, pos2(r.left() + 3.0 + text_w + 5.0 + i.w as f32 / 2.0, r.center().y), colour);
    }
    let resp = ui.interact(r, ui.id().with(("badge", text)), Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, ui.is_enabled(), text));
    r
}

/// A badge in the layout, as the section titles of DeviceActions.qml.
fn badge_here(ui: &mut egui::Ui, text: &str, colour: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 15.0), Sense::hover());
    badge(ui, rect.min, text, true, None, colour);
}

/// CustomDialog.qml's frame: black, a bright 2 px outline, round corners, the window dimmed to a fifth behind it.
fn modal(id: Id, pal: &Palette) -> egui::Modal {
    egui::Modal::new(id)
        .backdrop_color(Color32::from_black_alpha(204))
        .frame(egui::Frame::NONE.fill(Color32::BLACK).stroke(Stroke::new(2.0, pal.accent)).corner_radius(7).inner_margin(2))
}

/// CustomDialog.qml's header: 42 px of the accent with the title in capitals, dark.
fn dialog_header(ui: &mut egui::Ui, title: &str, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::hover());
    ui.painter().rect_filled(rect, egui::CornerRadius { nw: 5, ne: 5, sw: 0, se: 0 }, pal.accent);
    caps_at(ui, rect.center(), Align2::CENTER_CENTER, title, fonts::label(), pal.dark1);
}

/// What covers the card for the console and the tools window: the card's own background, without its grid.
fn cover(ui: &egui::Ui, card: Rect, pal: &Palette) {
    let p = ui.painter();
    draw::gradient_polygon(p, &draw::rounded(card, 9.0), &[(card.top(), pal.card_top), (card.bottom(), pal.card_bottom)]);
    p.rect_stroke(card, 9.0, Stroke::new(3.0, pal.accent), StrokeKind::Inside);
}

/// A key of DirectionalKeypad.qml (controls/dpad: triangle, circle, back), filled when hovered or pressed.
fn key_shape(p: &egui::Painter, key: InputKey, c: egui::Pos2, fill: Option<Color32>, pal: &Palette) {
    let stroke = Stroke::new(2.0, pal.accent);
    let ink = if fill == Some(pal.accent) { pal.dark1 } else { pal.accent };
    let tri = |pts: [egui::Vec2; 3]| {
        let pts: Vec<egui::Pos2> = pts.iter().map(|v| c + *v).collect();
        egui::Shape::convex_polygon(pts, fill.unwrap_or(Color32::TRANSPARENT), stroke)
    };
    match key {
        // triangle.svg (32 x 28), turned to point its way
        InputKey::Up => {
            p.add(tri([vec2(0.0, -12.0), vec2(14.0, 12.0), vec2(-14.0, 12.0)]));
        }
        InputKey::Down => {
            p.add(tri([vec2(0.0, 12.0), vec2(-14.0, -12.0), vec2(14.0, -12.0)]));
        }
        InputKey::Left => {
            p.add(tri([vec2(-12.0, 0.0), vec2(12.0, -14.0), vec2(12.0, 14.0)]));
        }
        InputKey::Right => {
            p.add(tri([vec2(12.0, 0.0), vec2(-12.0, 14.0), vec2(-12.0, -14.0)]));
        }
        // circle.svg (52 x 52)
        InputKey::Ok => {
            if let Some(f) = fill {
                p.circle_filled(c, 25.0, f);
            }
            p.circle_stroke(c, 25.0, stroke);
        }
        // back.svg: a circle with a turning arrow
        InputKey::Back => {
            p.circle_filled(c, 25.0, fill.unwrap_or(pal.dark1));
            p.circle_stroke(c, 25.0, Stroke::new(2.0, pal.light1));
            let s = Stroke::new(2.0, ink);
            p.add(egui::Shape::line(
                vec![
                    c + vec2(-8.0, -8.0),
                    c + vec2(6.0, -8.0),
                    c + vec2(13.0, -4.0),
                    c + vec2(13.0, 6.0),
                    c + vec2(6.0, 10.0),
                    c + vec2(-10.0, 10.0),
                ],
                s,
            ));
            p.add(egui::Shape::line(vec![c + vec2(-8.0, -15.0), c + vec2(-16.0, -8.0), c + vec2(-8.0, -1.0)], s));
        }
    }
}

fn accent_source(accent: Accent, ygg: Option<PathBuf>) -> theme::Source {
    match (accent, ygg) {
        (Accent::System, Some(d)) => theme::Source::System(d),
        _ => theme::Source::Fixed { accent: None },
    }
}

fn error_title(op: Op) -> &'static str {
    match op {
        Op::Backup => "Backup Failed",
        Op::Restore => "Restore Failed",
        Op::Update => "Update Failed",
        Op::Repair => "Repair Failed",
        Op::Radio | Op::Fus => "Install Failed",
        _ => "Operation Error",
    }
}

/// A field naming a file to read.
fn field(label: &str, value: PathBuf) -> Field {
    Field { label: label.into(), value: value.display().to_string(), kind: FieldKind::Open }
}

/// A field naming a new file.
fn save_field(label: &str, value: PathBuf) -> Field {
    Field { label: label.into(), value: value.display().to_string(), kind: FieldKind::Save }
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

/// The screen's colours, as qFlipper's ScreenCanvas: the accent for the background, its dark step for lit pixels.
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
