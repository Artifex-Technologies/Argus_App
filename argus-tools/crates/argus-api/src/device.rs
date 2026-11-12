//! A Flipper attached to the Rust server or `chymaera watch`: the part of the C++ app's
//! `Bridge::attachDevice` and `startPull` that a headless server needs. One thread owns the RPC
//! session; the server asks it for pulls and reads back what happened, so a long pull never holds
//! up the API. Only what the caller named is opened (`argus-cli --port DEV`); nothing is searched
//! for. Everything here reads from the device; nothing writes to it.

use std::path::PathBuf;
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

enum Cmd {
    Pull { roots: Vec<String>, label: String, session: Option<i64> },
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

pub struct Device {
    name: String,
    tx: Option<Sender<Cmd>>,
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
        let (tx, cmd_rx) = mpsc::channel();
        let (note_tx, rx) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("argus-device".into())
            .spawn(move || work(session, cmd_rx, note_tx, datastore))
            .map_err(RpcError::Io)?;
        Ok(Device { name, tx: Some(tx), rx, pulling: false, worker: Some(worker) })
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
        let sent = self.tx.as_ref().is_some_and(|tx| tx.send(Cmd::Pull { roots, label, session }).is_ok());
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
        self.tx = None;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

fn work<T: Transport>(mut session: Session<T>, cmds: Receiver<Cmd>, notes: Sender<Note>, datastore: PathBuf) {
    loop {
        match cmds.recv_timeout(KEEPALIVE) {
            Ok(Cmd::Pull { roots, label, session: sid }) => {
                let store = match Store::open(&datastore) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ =
                            notes.send(Note::Event(Severity::Error, Source::Device, format!("Cannot pull {label}: {e}"), String::new()));
                        let _ = notes.send(Note::PullDone(false));
                        continue;
                    }
                };
                let roots: Vec<&str> = roots.iter().map(String::as_str).collect();
                let mut log = |sev: Severity, src: Source, msg: &str, detail: &str| -> Result<(), StoreError> {
                    let _ = notes.send(Note::Event(sev, src, msg.to_string(), detail.to_string()));
                    Ok(())
                };
                let r = sync::pull_logged(&mut session, &store, sid, &roots, &label, &Limits::default(), &mut log);
                let ok = r.as_ref().is_ok_and(|rep| rep.failed.is_empty());
                let _ = notes.send(Note::PullDone(ok));
                if let Err(sync::PullError::Rpc(e)) = r {
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
