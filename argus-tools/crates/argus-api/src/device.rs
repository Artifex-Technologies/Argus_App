//! A Flipper attached to the Rust server or `chymaera watch`: the part of the C++ app's
//! `Bridge::attachDevice` and `startPull` that a headless server needs. One thread owns the RPC
//! session; the server asks it for pulls and reads back what happened, so a long pull never holds
//! up the API. Only what the caller named is opened (`argus-cli --port DEV`); nothing is searched
//! for. Everything here reads from the device; nothing writes to it.
//!
//! The desktop app has its own thread for the device (it does more than pulls); it attaches the
//! device with [`Device::external`] and runs the pulls the API asks for with [`pull`], so the
//! console and the API share one device, one pull at a time, as in the C++ app.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use argus_capture::store::{Severity, Source, Store, StoreError};
use argus_capture::sync;
use flipper_rpc::transfer::Limits;
use flipper_rpc::transport::Transport;
use flipper_rpc::{RpcError, Session};

/// How often an idle worker checks that the device still answers.
const KEEPALIVE: Duration = Duration::from_secs(1);

/// A pull the bridge asks the device's thread for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub roots: Vec<String>,
    pub label: String,
    /// The session the captures are recorded in.
    pub session: Option<i64>,
}

/// What the worker reports.
#[derive(Debug, Clone, PartialEq)]
pub enum Note {
    Event(Severity, Source, String, String),
    /// A pull ended (true when every file was read).
    PullDone(bool),
    /// The device stopped answering; the worker has ended.
    Gone(String),
}

type Submit = Box<dyn FnMut(PullRequest) -> bool + Send>;

pub struct Device {
    name: String,
    /// Hands a pull to the thread that owns the session; false when it is gone.
    submit: Option<Submit>,
    rx: Receiver<Note>,
    pulling: bool,
    worker: Option<JoinHandle<()>>,
}

impl Device {
    /// Starts the RPC session on `io`, reads the device's name (`hardware.name` of `devinfo`, as the
    /// C++ app), and leaves a worker thread in charge of the session. Pulls record into the
    /// datastore at `datastore`.
    pub fn connect<T: Transport + 'static>(io: T, timeout: Duration, datastore: PathBuf) -> Result<Device, RpcError> {
        let mut session = Session::start(io, timeout)?;
        let version = session.protobuf_version()?;
        let (_, info) = session.device_info(version)?;
        let name = info.iter().find(|(k, _)| k == "hardware.name" || k == "hardware_name").map(|(_, v)| v.clone()).unwrap_or_default();
        let (tx, cmd_rx) = mpsc::channel::<PullRequest>();
        let (note_tx, rx) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("argus-device".into())
            .spawn(move || work(session, cmd_rx, note_tx, datastore))
            .map_err(RpcError::Io)?;
        let submit: Submit = Box::new(move |r| tx.send(r).is_ok());
        Ok(Device { name, submit: Some(submit), rx, pulling: false, worker: Some(worker) })
    }

    /// A device whose session another thread owns (the desktop app's). `submit` hands it a pull and
    /// the sender for that pull's notes (see [`pull`]); it returns false when the device is gone.
    /// The owner calls [`crate::bridge::Bridge::detach_device`] when the device leaves.
    pub fn external(name: String, mut submit: impl FnMut(PullRequest, Sender<Note>) -> bool + Send + 'static) -> Device {
        let (note_tx, rx) = mpsc::channel();
        let submit: Submit = Box::new(move |r| submit(r, note_tx.clone()));
        Device { name, submit: Some(submit), rx, pulling: false, worker: None }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_pulling(&self) -> bool {
        self.pulling
    }

    /// Starts a pull; false when one is running already.
    pub fn start_pull(&mut self, roots: Vec<String>, label: String, session: Option<i64>) -> bool {
        if self.pulling {
            return false;
        }
        let sent = self.submit.as_mut().is_some_and(|f| f(PullRequest { roots, label, session }));
        self.pulling = sent;
        sent
    }

    /// What happened since the last call.
    pub fn notes(&mut self) -> Vec<Note> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(n) => {
                    if matches!(n, Note::PullDone(_)) {
                        self.pulling = false;
                    }
                    out.push(n);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if !out.iter().any(|n| matches!(n, Note::Gone(_))) {
                        out.push(Note::Gone("the device worker ended".into()));
                    }
                    break;
                }
            }
        }
        out
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // closing the channel ends the worker at its next wait
        self.submit = None;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Runs one pull on `session` into the datastore at `datastore`, every step a [`Note`] handed to
/// `notes`, ending with [`Note::PullDone`]. An error is returned only when the device stopped answering.
pub fn pull<T: Transport>(
    session: &mut Session<T>,
    datastore: &Path,
    r: &PullRequest,
    notes: &mut dyn FnMut(Note),
) -> Result<bool, RpcError> {
    let store = match Store::open(datastore) {
        Ok(s) => s,
        Err(e) => {
            notes(Note::Event(Severity::Error, Source::Device, format!("Cannot pull {}: {e}", r.label), String::new()));
            notes(Note::PullDone(false));
            return Ok(false);
        }
    };
    let roots: Vec<&str> = r.roots.iter().map(String::as_str).collect();
    let mut log = |sev: Severity, src: Source, msg: &str, detail: &str| -> Result<(), StoreError> {
        notes(Note::Event(sev, src, msg.to_string(), detail.to_string()));
        Ok(())
    };
    let res = sync::pull_logged(session, &store, r.session, &roots, &r.label, &Limits::default(), &mut log);
    let ok = res.as_ref().is_ok_and(|rep| rep.failed.is_empty());
    notes(Note::PullDone(ok));
    match res {
        Err(sync::PullError::Rpc(e)) => Err(e),
        _ => Ok(ok),
    }
}

fn work<T: Transport>(mut session: Session<T>, cmds: Receiver<PullRequest>, notes: Sender<Note>, datastore: PathBuf) {
    loop {
        match cmds.recv_timeout(KEEPALIVE) {
            Ok(r) => {
                if let Err(e) = pull(&mut session, &datastore, &r, &mut |n| {
                    let _ = notes.send(n);
                }) {
                    let _ = notes.send(Note::Gone(e.to_string()));
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                // a cheap read: is the device still there?
                if let Err(e) = session.protobuf_version() {
                    let _ = notes.send(Note::Gone(e.to_string()));
                    return;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}
