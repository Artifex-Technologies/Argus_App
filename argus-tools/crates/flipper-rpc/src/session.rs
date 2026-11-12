//! One RPC session with a Flipper: the text-CLI handshake that switches the serial port into
//! RPC mode, then request/response exchanges matched by `command_id`.
//!
//! Mirrors `ProtobufSession`, `SerialInitHelper`, `SkipMOTDOperation` and `StartRPCOperation`
//! of the C++ app, one request at a time (the C++ queue never runs two at once either).

use std::io;
use std::time::Duration;

use crate::frame::{encode_frame, FrameDecoder, FrameError};
use crate::proto::pb::main::Content;
use crate::proto::pb::{CommandStatus, Main};
use crate::transport::{is_timeout, Transport};

/// The end of the CLI banner and prompt the Flipper prints when DTR goes high.
pub const PROMPT: &[u8] = b"\r\n\r\n>: ";
/// Typed at the prompt to switch to RPC mode.
pub const START_RPC: &[u8] = b"start_rpc_session\r";
/// The Flipper's echo of [`START_RPC`]; RPC frames follow it.
pub const START_RPC_ECHO: &[u8] = b"start_rpc_session\r\n";

/// The banner is about 1 KiB of text; a device that sends more before its prompt is refused.
const MAX_BANNER: usize = 16 * 1024;
/// Frames with another `command_id` (broadcasts such as screen frames, or stray replies) that
/// one request skips before giving up.
const MAX_IGNORED_FRAMES: usize = 1024;
/// Responses to one request (a 512 MiB file read in 512-byte chunks needs 2^20).
const MAX_RESPONSES: usize = 1 << 20;

#[derive(Debug)]
pub enum RpcError {
    Io(io::Error),
    /// Nothing arrived within the timeout; `stage` says what we were waiting for.
    Timeout(&'static str),
    /// The connection closed.
    Closed(&'static str),
    Handshake(String),
    Frame(FrameError),
    /// The device answered with a status other than OK.
    Device(CommandStatus),
    /// The device answered with an unknown status number.
    UnknownStatus(i32),
    /// A response of a type the request does not produce.
    Unexpected(String),
    /// A response stream longer than any legitimate one.
    Limit(String),
    /// A caller-supplied argument the device would refuse anyway (checked before sending).
    Invalid(String),
    /// An earlier error left the stream in an unknown place; open a new session.
    Broken,
    /// A local file could not be written or read.
    Local(String),
}

impl RpcError {
    /// A short stable code for JSON output.
    pub fn code(&self) -> &'static str {
        match self {
            RpcError::Io(_) => "io",
            RpcError::Timeout(_) => "timeout",
            RpcError::Closed(_) => "closed",
            RpcError::Handshake(_) => "handshake",
            RpcError::Frame(_) => "frame",
            RpcError::Device(_) | RpcError::UnknownStatus(_) => "device",
            RpcError::Unexpected(_) => "unexpected",
            RpcError::Limit(_) => "limit",
            RpcError::Invalid(_) => "invalid",
            RpcError::Broken => "broken",
            RpcError::Local(_) => "local",
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Io(e) => write!(f, "I/O error: {e}"),
            RpcError::Timeout(stage) => write!(f, "timed out waiting for {stage}"),
            RpcError::Closed(stage) => write!(f, "connection closed while waiting for {stage}"),
            RpcError::Handshake(m) => write!(f, "RPC handshake failed: {m}"),
            RpcError::Frame(e) => write!(f, "{e}"),
            RpcError::Device(s) => write!(f, "Device replied with error: {}", status_text(*s)),
            RpcError::UnknownStatus(n) => write!(f, "Device replied with unknown status {n}"),
            RpcError::Unexpected(m) => write!(f, "unexpected response: {m}"),
            RpcError::Limit(m) => write!(f, "{m}"),
            RpcError::Invalid(m) => write!(f, "{m}"),
            RpcError::Broken => write!(f, "the session is unusable after an earlier error"),
            RpcError::Local(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for RpcError {}

impl From<FrameError> for RpcError {
    fn from(e: FrameError) -> RpcError {
        RpcError::Frame(e)
    }
}

/// The C++ app's wording for each status (`MainResponse::errorString`), so messages match.
pub fn status_text(s: CommandStatus) -> &'static str {
    use CommandStatus::*;
    match s {
        Ok => "No error",
        Error => "Unknown",
        ErrorDecode => "Decode failure",
        ErrorNotImplemented => "Command not implemented",
        ErrorBusy => "Device is busy",
        ErrorContinuousCommandInterrupted => "Continuous command interrupted",
        ErrorInvalidParameters => "Invalid parameters",
        ErrorStorageNotReady => "Storage not ready",
        ErrorStorageExist => "File/directory already exists",
        ErrorStorageNotExist => "File/directory does not exist",
        ErrorStorageInvalidParameter => "Invalid storage API parameter",
        ErrorStorageDenied => "Access denied",
        ErrorStorageInvalidName => "Invalid name/path",
        ErrorStorageInternal => "Internal error",
        ErrorStorageNotImplemented => "Storage command not implemented",
        ErrorStorageAlreadyOpen => "File/directory is already open",
        ErrorStorageDirNotEmpty => "Directory is not empty",
        ErrorAppCantStart => "Cannot start the application",
        ErrorAppSystemLocked => "Another application is already running",
        ErrorAppNotRunning => "Application is not running",
        ErrorAppCmdError => "Application command failed",
        ErrorVirtualDisplayAlreadyStarted => "Virtual display session has already been started",
        ErrorVirtualDisplayNotStarted => "No virtual display session running",
        ErrorGpioModeIncorrect => "GPIO pin mode is incorrect",
        ErrorGpioUnknownPinMode => "Unknown GPIO pin mode",
    }
}

pub struct Session<T: Transport> {
    io: T,
    decoder: FrameDecoder,
    counter: u32,
    broken: bool,
}

impl<T: Transport> Session<T> {
    /// Runs the handshake on a freshly opened transport: wait for the CLI prompt, type
    /// `start_rpc_session`, wait for its echo. `timeout` applies to each read.
    pub fn start(mut io: T, timeout: Duration) -> Result<Session<T>, RpcError> {
        io.set_read_timeout(timeout).map_err(RpcError::Io)?;
        let mut buf = Vec::new();
        let after_prompt = read_until(&mut io, &mut buf, PROMPT, MAX_BANNER, "the CLI prompt")?;
        let mut rest = buf.split_off(after_prompt);
        io.write_all(START_RPC).and_then(|_| io.flush()).map_err(RpcError::Io)?;
        let after_echo = read_until(&mut io, &mut rest, START_RPC_ECHO, MAX_BANNER, "the start_rpc_session echo")?;
        let mut decoder = FrameDecoder::new();
        decoder.push(&rest[after_echo..])?;
        Ok(Session { io, decoder, counter: 0, broken: false })
    }

    /// The next command id. 0 is reserved for broadcasts, as in the C++ app.
    fn next_id(&mut self) -> u32 {
        self.counter = self.counter.wrapping_add(1);
        if self.counter == 0 {
            self.counter = 1;
        }
        self.counter
    }

    /// Sends one request and hands each response to `on_response` until the device sends one
    /// with `has_next` false. A non-OK status ends the exchange with [`RpcError::Device`].
    /// Frames with other ids are skipped (the C++ app logs and drops them too), up to a limit.
    pub fn request(&mut self, content: Content, mut on_response: impl FnMut(Main) -> Result<(), RpcError>) -> Result<(), RpcError> {
        let id = self.send(vec![content])?;
        self.receive(id, |m| {
            let last = !m.has_next;
            on_response(m)?;
            Ok(last)
        })
    }

    /// Writes one frame per content, all with one new command id (the C++ storage read sends
    /// its stat and read requests that way). Returns the id.
    pub fn send(&mut self, contents: Vec<Content>) -> Result<u32, RpcError> {
        if self.broken {
            return Err(RpcError::Broken);
        }
        let id = self.next_id();
        let mut bytes = Vec::new();
        for content in contents {
            bytes.extend(encode_frame(&Main { command_id: id, content: Some(content), ..Default::default() })?);
        }
        let r = self.io.write_all(&bytes).and_then(|_| self.io.flush()).map_err(RpcError::Io);
        self.check(r).map(|_| id)
    }

    /// Hands every OK response with `id` to `on_response` until it returns `true` (done).
    /// A non-OK status ends the exchange with [`RpcError::Device`]; the session stays usable.
    /// Any other error leaves the stream in an unknown place, so the session refuses further
    /// requests ([`RpcError::Broken`]).
    pub fn receive(&mut self, id: u32, mut on_response: impl FnMut(Main) -> Result<bool, RpcError>) -> Result<(), RpcError> {
        if self.broken {
            return Err(RpcError::Broken);
        }
        let r = self.receive_inner(id, &mut on_response);
        self.check(r)
    }

    fn check<V>(&mut self, r: Result<V, RpcError>) -> Result<V, RpcError> {
        if let Err(e) = &r {
            if !matches!(e, RpcError::Device(_) | RpcError::UnknownStatus(_) | RpcError::Invalid(_)) {
                self.broken = true;
            }
        }
        r
    }

    fn receive_inner(&mut self, id: u32, on_response: &mut impl FnMut(Main) -> Result<bool, RpcError>) -> Result<(), RpcError> {
        let mut ignored = 0usize;
        let mut answered = 0usize;
        loop {
            let resp = self.next_message()?;
            if resp.command_id != id {
                ignored += 1;
                if ignored > MAX_IGNORED_FRAMES {
                    return Err(RpcError::Limit(format!("more than {MAX_IGNORED_FRAMES} frames for other requests")));
                }
                continue;
            }
            answered += 1;
            if answered > MAX_RESPONSES {
                return Err(RpcError::Limit(format!("more than {MAX_RESPONSES} responses to one request")));
            }
            let status = CommandStatus::try_from(resp.command_status).map_err(|_| RpcError::UnknownStatus(resp.command_status))?;
            if status != CommandStatus::Ok {
                return Err(RpcError::Device(status));
            }
            if on_response(resp)? {
                return Ok(());
            }
        }
    }

    fn next_message(&mut self) -> Result<Main, RpcError> {
        let mut chunk = [0u8; 4096];
        loop {
            if let Some(m) = self.decoder.next_frame()? {
                return Ok(m);
            }
            match self.io.read(&mut chunk) {
                Ok(0) => return Err(RpcError::Closed("a response")),
                Ok(n) => self.decoder.push(&chunk[..n])?,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if is_timeout(&e) => return Err(RpcError::Timeout("a response")),
                Err(e) => return Err(RpcError::Io(e)),
            }
        }
    }

    /// Gives the transport back (for tests that inspect it).
    pub fn into_inner(self) -> T {
        self.io
    }
}

/// Reads into `buf` until it contains `marker`; returns the index just past the marker.
fn read_until(io: &mut impl Transport, buf: &mut Vec<u8>, marker: &[u8], max: usize, stage: &'static str) -> Result<usize, RpcError> {
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(pos) = buf.windows(marker.len()).position(|w| w == marker) {
            return Ok(pos + marker.len());
        }
        if buf.len() > max {
            return Err(RpcError::Handshake(format!("more than {max} bytes without {stage}")));
        }
        match io.read(&mut chunk) {
            Ok(0) => return Err(RpcError::Closed(stage)),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if is_timeout(&e) => return Err(RpcError::Timeout(stage)),
            Err(e) => return Err(RpcError::Io(e)),
        }
    }
}
