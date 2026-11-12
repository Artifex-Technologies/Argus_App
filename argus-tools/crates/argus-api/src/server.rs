//! The transport of the Sarina API: the port of `SarinaServer` (backend/chymaera/sarina). TCP on
//! 127.0.0.1 only, one JSON request per line, a token on every request (made and written to
//! `$XDG_RUNTIME_DIR/argus/token` when none is given). One thread serves every connection in turn,
//! as the C++ server's event loop does, so requests never run at the same time.
//!
//! [`Conn`] is the protocol without sockets (the tests drive it directly); [`Server`] binds,
//! polls and moves bytes.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::bridge::Bridge;
use crate::eventlog::{Severity, Source};
use crate::qjson::{self, Object, Value};

pub const DEFAULT_PORT: u16 = 44700;
/// A client may have this much of an unfinished line buffered (`request too large` past it).
pub const MAX_LINE: usize = 1 << 20;
/// Answers a client has not read yet; past this it is dropped.
const MAX_PENDING_OUT: usize = 16 << 20;
const MAX_CLIENTS: usize = 1000;

/// One connection's protocol state.
#[derive(Default)]
pub struct Conn {
    buf: Vec<u8>,
    /// Set after `request too large`: send what is pending, then close.
    pub closing: bool,
}

fn compact_line(o: Object) -> Vec<u8> {
    let mut v = qjson::to_compact(&Value::Object(o));
    v.push(b'\n');
    v
}

/// Constant-time comparison, as the C++ server's.
fn same_token(given: &[u8], want: &[u8]) -> bool {
    let mut diff = u8::from(given.len() != want.len());
    for (i, w) in want.iter().enumerate() {
        diff |= w ^ given.get(i).copied().unwrap_or(0);
    }
    !want.is_empty() && diff == 0
}

/// `processLine`: the answer to one request line (none for an empty line).
pub fn answer(bridge: &mut Bridge, token: &[u8], line: &[u8]) -> Option<Vec<u8>> {
    let line = qjson::trimmed(line);
    if line.is_empty() {
        return None;
    }
    let request = match qjson::from_json(line) {
        Ok(Value::Object(o)) => o,
        other => {
            let why = match other {
                Err(e) => e.text(),
                Ok(_) => "no error occurred",
            };
            return Some(compact_line(
                Object::new().with("ok", Value::Bool(false)).with("error", Value::from(format!("invalid JSON: {why}"))),
            ));
        }
    };
    let given = qjson::to_utf8(&request.get("token").map(Value::to_qstring).unwrap_or_default());
    if !same_token(&given, token) {
        return Some(compact_line(Object::new().with("ok", Value::Bool(false)).with("error", Value::from("unauthorized"))));
    }
    let cmd = request.get("cmd").map(Value::to_qstring).unwrap_or_default();
    bridge.note_request(&cmd);
    let mut response = bridge.handle(&request);
    if let Some(id) = request.get("id") {
        response.insert("id", id.clone());
    }
    Some(compact_line(response))
}

impl Conn {
    /// Bytes from the client in, answers out.
    pub fn feed(&mut self, bridge: &mut Bridge, token: &[u8], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        if self.closing {
            return out;
        }
        self.buf.extend_from_slice(data);
        let mut start = 0;
        while let Some(n) = self.buf[start..].iter().position(|&b| b == b'\n') {
            if let Some(a) = answer(bridge, token, &self.buf[start..start + n]) {
                out.extend_from_slice(&a);
            }
            start += n + 1;
        }
        self.buf.drain(..start);
        if self.buf.len() > MAX_LINE {
            out.extend_from_slice(&compact_line(
                Object::new().with("ok", Value::Bool(false)).with("error", Value::from("request too large")),
            ));
            self.buf = Vec::new();
            self.closing = true;
        }
        out
    }
}

/// The private runtime folder: `$XDG_RUNTIME_DIR`, else `/run/user/UID`; it must be ours and 0700.
pub fn runtime_dir() -> Option<PathBuf> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    let candidates = [std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from), Some(PathBuf::from(format!("/run/user/{uid}")))];
    candidates
        .into_iter()
        .flatten()
        .filter(|p| p.is_absolute())
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_dir() && m.uid() == uid && m.permissions().mode() & 0o077 == 0))
}

/// 256 random bits as 64 hex digits.
pub fn new_token() -> std::io::Result<String> {
    let mut b = [0u8; 32];
    let mut f = std::fs::File::open("/dev/urandom")?;
    f.read_exact(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Writes the token to `DIR/argus/token` (folder 0700, file 0600, made new).
pub fn write_token(runtime: &Path, token: &str) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let dir = runtime.join("argus");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let path = dir.join("token");
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
    f.write_all(token.as_bytes())?;
    Ok(path)
}

/// What a started server records: the port in the bridge and the event the C++ app logs.
pub fn announce(bridge: &mut Bridge, port: u16, token_file: Option<&Path>) {
    bridge.server_port = port;
    let detail = match token_file {
        Some(f) => format!("token required (written to {})", f.display()),
        None => "token required".into(),
    };
    bridge.append(Severity::Notice, Source::Sarina, &format!("Control API listening on 127.0.0.1:{port}"), &detail);
}

struct Client {
    stream: TcpStream,
    conn: Conn,
    out: Vec<u8>,
}

pub struct Server {
    listener: TcpListener,
    port: u16,
    token: String,
    token_file: Option<PathBuf>,
}

impl Server {
    /// `Bridge::startServer`: listens on 127.0.0.1:`port` (0 for any free port) with `token`, or,
    /// when it is empty, with a new token written to the runtime folder. The port is bound first:
    /// a server that cannot listen leaves the token file of one already running alone.
    pub fn start(bridge: &mut Bridge, port: u16, token: &str) -> Result<Server, String> {
        let result = Server::listen(port, token);
        match &result {
            Ok(s) => announce(bridge, s.port, s.token_file.as_deref()),
            Err(_) => {
                bridge.append(Severity::Error, Source::Sarina, &format!("Control API failed to start on port {port}"), "");
            }
        }
        result
    }

    fn listen(port: u16, token: &str) -> Result<Server, String> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(|e| format!("Cannot listen on 127.0.0.1:{port}: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let (token, token_file) = if token.is_empty() {
            let dir = runtime_dir().ok_or("No private runtime folder for the control token; not starting")?;
            let t = new_token().map_err(|e| format!("Cannot make a control token: {e}"))?;
            let f = write_token(&dir, &t).map_err(|e| format!("Cannot write the control token to {}: {e}; not starting", dir.display()))?;
            (t, Some(f))
        } else {
            (token.to_string(), None)
        };
        Ok(Server { listener, port, token, token_file })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn token_file(&self) -> Option<&Path> {
        self.token_file.as_deref()
    }

    /// Serves until `stop` is set.
    pub fn run(&self, bridge: &mut Bridge, stop: &AtomicBool) {
        let mut clients: Vec<Client> = Vec::new();
        while !stop.load(Ordering::Relaxed) {
            bridge.poll_device();
            let mut fds = vec![libc::pollfd { fd: self.listener.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
            for c in &clients {
                let mut events = libc::POLLIN;
                if !c.out.is_empty() {
                    events |= libc::POLLOUT;
                }
                fds.push(libc::pollfd { fd: c.stream.as_raw_fd(), events, revents: 0 });
            }
            // SAFETY: fds is a valid array of pollfd for the duration of the call.
            let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 200) };
            if n <= 0 {
                continue;
            }
            let mut drop_ids = Vec::new();
            for (i, c) in clients.iter_mut().enumerate() {
                let rev = fds[i + 1].revents;
                if rev & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                    let mut buf = [0u8; 65536];
                    match c.stream.read(&mut buf) {
                        Ok(0) => drop_ids.push(i),
                        Ok(k) => {
                            let out = c.conn.feed(bridge, self.token.as_bytes(), &buf[..k]);
                            c.out.extend_from_slice(&out);
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {}
                        Err(_) => drop_ids.push(i),
                    }
                }
                if !c.out.is_empty() {
                    match c.stream.write(&c.out) {
                        Ok(k) => {
                            c.out.drain(..k);
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {}
                        Err(_) => drop_ids.push(i),
                    }
                }
                if c.out.len() > MAX_PENDING_OUT || (c.conn.closing && c.out.is_empty()) {
                    drop_ids.push(i);
                }
            }
            drop_ids.sort_unstable();
            drop_ids.dedup();
            for i in drop_ids.into_iter().rev() {
                clients.remove(i);
            }
            if fds[0].revents & libc::POLLIN != 0 {
                while let Ok((stream, peer)) = self.listener.accept() {
                    // Defence in depth: the socket is bound to loopback anyway.
                    if !peer.ip().is_loopback() || clients.len() >= MAX_CLIENTS || stream.set_nonblocking(true).is_err() {
                        continue;
                    }
                    clients.push(Client { stream, conn: Conn::default(), out: Vec::new() });
                }
            }
        }
    }

    /// `stopServer`: removes a token file this server wrote.
    pub fn stop(self, bridge: &mut Bridge) {
        if let Some(f) = &self.token_file {
            let _ = std::fs::remove_file(f);
        }
        bridge.server_port = 0;
        bridge.append(Severity::Notice, Source::Sarina, "Control API stopped", "");
    }
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

/// A flag set by SIGINT or SIGTERM, so `serve` stops cleanly (and removes its token file).
pub fn stop_on_signals() -> &'static AtomicBool {
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
    &STOP
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        assert!(same_token(b"abc", b"abc"));
        assert!(!same_token(b"abd", b"abc"));
        assert!(!same_token(b"abcd", b"abc"));
        assert!(!same_token(b"", b""));
        let t = new_token().unwrap();
        assert_eq!(t.len(), 64);
        assert!(t.bytes().all(|b| b.is_ascii_hexdigit()));
    }
}
