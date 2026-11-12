//! A local stand-in for Flipper's update server over HTTPS, for testing `flipper_updates` and argus-cli
//! without the network: it listens on 127.0.0.1 only, with a test CA made when it starts (ECDSA P-256 with
//! ring, the certificates written out in DER by hand: [`TestCa`]) whose certificate the client is told to
//! trust (`SSL_CERT_FILE`); nothing else trusts it. It serves a folder like the one `--write-firmware` makes:
//!
//! - `/firmware/directory.json`: `DIR/directory.json`, with every `https://update.invalid` (the made-up
//!   directory's host, which the C++ app's test hook ignores) replaced by the server's own address;
//! - `/regions/api/v0/bundle`: `DIR/bundle`;
//! - `/firmware/NAME`: `DIR/NAME` (a plain name; nothing outside DIR);
//! - and misbehaviour under `/test/`: `endless` (a body without end and without a length), `huge-length` (a
//!   length of 1 TB), `stall` (headers, then nothing), `redirect-http` (to plain http), `redirect-loop`, and
//!   `wrong-NAME` (`DIR/NAME` with its first byte changed).
//!
//! Every request is recorded (`GET PATH`) for the tests.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

// --- DER, as much as two certificates need

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let n = content.len();
    if n < 0x80 {
        out.push(n as u8);
    } else {
        let len = n.to_be_bytes();
        let skip = len.iter().take_while(|b| **b == 0).count();
        out.push(0x80 | (len.len() - skip) as u8);
        out.extend(&len[skip..]);
    }
    out.extend(content);
    out
}

fn seq(items: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x30, &items.concat())
}

fn oid(bytes: &[u8]) -> Vec<u8> {
    tlv(0x06, bytes)
}

fn bits(bytes: &[u8], unused: u8) -> Vec<u8> {
    let mut c = vec![unused];
    c.extend(bytes);
    tlv(0x03, &c)
}

const ECDSA_SHA256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
const EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
const BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
const KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
const SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];
const EXT_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x25];
const SERVER_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];

fn name(cn: &str) -> Vec<u8> {
    seq(&[tlv(0x31, &seq(&[oid(COMMON_NAME), tlv(0x0c, cn.as_bytes())]))])
}

fn extension(id: &[u8], critical: bool, value: Vec<u8>) -> Vec<u8> {
    let mut parts = vec![oid(id)];
    if critical {
        parts.push(tlv(0x01, &[0xff]));
    }
    parts.push(tlv(0x04, &value));
    seq(&parts)
}

fn certificate(serial: u8, issuer: &str, subject: &str, public_key: &[u8], extensions: Vec<Vec<u8>>, signer: &EcdsaKeyPair) -> Vec<u8> {
    let alg = seq(&[oid(ECDSA_SHA256)]);
    let tbs = seq(&[
        tlv(0xa0, &tlv(0x02, &[2])),
        tlv(0x02, &[serial]),
        alg.clone(),
        name(issuer),
        seq(&[tlv(0x17, b"200101000000Z"), tlv(0x17, b"491231235959Z")]),
        name(subject),
        seq(&[seq(&[oid(EC_PUBLIC_KEY), oid(P256)]), bits(public_key, 0)]),
        tlv(0xa3, &seq(&extensions)),
    ]);
    let sig = signer.sign(&SystemRandom::new(), &tbs).expect("signing");
    seq(&[tbs, alg, bits(sig.as_ref(), 0)])
}

fn key() -> (Vec<u8>, EcdsaKeyPair) {
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).expect("key");
    let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).expect("key");
    (pkcs8.as_ref().to_vec(), pair)
}

/// A CA made for one test run and the server certificate it signed (for `127.0.0.1` and `localhost`).
pub struct TestCa {
    pub ca_der: Vec<u8>,
    pub server_der: Vec<u8>,
    pub server_key_pkcs8: Vec<u8>,
}

impl TestCa {
    pub fn new() -> TestCa {
        let (_, ca_key) = key();
        let ca_der = certificate(
            1,
            "Argus test CA (local only)",
            "Argus test CA (local only)",
            ca_key.public_key().as_ref(),
            vec![extension(BASIC_CONSTRAINTS, true, seq(&[tlv(0x01, &[0xff])])), extension(KEY_USAGE, true, bits(&[0x06], 1))],
            &ca_key,
        );
        let (server_key_pkcs8, server_key) = key();
        let san = seq(&[tlv(0x82, b"localhost"), tlv(0x87, &[127, 0, 0, 1])]);
        let server_der = certificate(
            2,
            "Argus test CA (local only)",
            "127.0.0.1",
            server_key.public_key().as_ref(),
            vec![
                extension(KEY_USAGE, true, bits(&[0x80], 7)),
                extension(EXT_KEY_USAGE, false, seq(&[oid(SERVER_AUTH)])),
                extension(SUBJECT_ALT_NAME, false, san),
            ],
            &ca_key,
        );
        TestCa { ca_der, server_der, server_key_pkcs8 }
    }

    /// The CA certificate as PEM (for `SSL_CERT_FILE`).
    pub fn ca_pem(&self) -> String {
        let b64 = base64(&self.ca_der);
        let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
        for line in b64.as_bytes().chunks(64) {
            pem.push_str(std::str::from_utf8(line).expect("ascii"));
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
        pem
    }
}

impl Default for TestCa {
    fn default() -> TestCa {
        TestCa::new()
    }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// --- the server

/// The server: started with [`UpdateServer::start`], it serves until the process ends.
pub struct UpdateServer {
    pub addr: SocketAddr,
    pub requests: Arc<Mutex<Vec<String>>>,
}

impl UpdateServer {
    /// `https://127.0.0.1:PORT`
    pub fn base(&self) -> String {
        format!("https://{}", self.addr)
    }

    /// Listens on 127.0.0.1 (a free port) and serves `dir` from a thread.
    pub fn start(dir: &Path, ca: &TestCa) -> io::Result<UpdateServer> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(io::Error::other)?
            .with_no_client_auth()
            .with_single_cert(
                vec![ca.server_der.clone().into()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(ca.server_key_pkcs8.clone().into()),
            )
            .map_err(io::Error::other)?;
        let config = Arc::new(config);
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let dir = dir.to_path_buf();
        let log = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (config, dir, log) = (config.clone(), dir.clone(), log.clone());
                std::thread::spawn(move || {
                    let _ = serve(stream, config, &dir, addr, &log);
                });
            }
        });
        Ok(UpdateServer { addr, requests })
    }
}

fn serve(tcp: TcpStream, config: Arc<rustls::ServerConfig>, dir: &Path, addr: SocketAddr, log: &Mutex<Vec<String>>) -> io::Result<()> {
    tcp.set_read_timeout(Some(Duration::from_secs(10)))?;
    let conn = rustls::ServerConnection::new(config).map_err(io::Error::other)?;
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    // the request line and headers, at most 16 KiB
    let mut head = Vec::new();
    {
        let mut r = BufReader::new((&mut tls).take(16 << 10));
        loop {
            let mut line = Vec::new();
            if r.read_until(b'\n', &mut line)? == 0 {
                return Ok(());
            }
            let end = line == b"\r\n" || line == b"\n";
            head.extend(line);
            if end {
                break;
            }
        }
    }
    let first = String::from_utf8_lossy(head.split(|b| *b == b'\n').next().unwrap_or(b"")).trim_end().to_string();
    let path = first.strip_prefix("GET ").and_then(|r| r.split(' ').next()).unwrap_or("").to_string();
    log.lock().expect("log").push(format!("GET {path}"));
    let base = format!("https://{addr}");
    let mut reply = |status: &str, headers: &str, body: &[u8]| -> io::Result<()> {
        write!(tls, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n", body.len())?;
        tls.write_all(body)?;
        tls.conn.send_close_notify();
        tls.flush()
    };
    let file = |name: &str| -> Option<Vec<u8>> {
        (!name.is_empty() && !name.starts_with('.') && !name.contains(['/', '\\'])).then(|| std::fs::read(dir.join(name)).ok()).flatten()
    };
    match path.as_str() {
        "/firmware/directory.json" => match file("directory.json") {
            Some(d) => reply("200 OK", "", String::from_utf8_lossy(&d).replace("https://update.invalid", &base).as_bytes()),
            None => reply("404 Not Found", "", b""),
        },
        "/regions/api/v0/bundle" => match file("bundle") {
            Some(d) => reply("200 OK", "Content-Type: application/json\r\n", &d),
            None => reply("404 Not Found", "", b""),
        },
        "/test/endless" => {
            write!(tls, "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")?;
            let chunk = [0x5a_u8; 16 << 10];
            loop {
                tls.write_all(&chunk)?;
            }
        }
        "/test/huge-length" => {
            write!(tls, "HTTP/1.1 200 OK\r\nContent-Length: 1000000000000\r\nConnection: close\r\n\r\n")?;
            let chunk = [0x5a_u8; 16 << 10];
            loop {
                tls.write_all(&chunk)?;
            }
        }
        "/test/stall" => {
            write!(tls, "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n")?;
            tls.flush()?;
            std::thread::sleep(Duration::from_secs(30));
            Ok(())
        }
        "/test/redirect-http" => reply("302 Found", &format!("Location: http://{addr}/firmware/directory.json\r\n"), b""),
        "/test/redirect-loop" => reply("302 Found", &format!("Location: {base}/test/redirect-loop\r\n"), b""),
        p => {
            let name = p.strip_prefix("/firmware/").or_else(|| p.strip_prefix("/test/wrong-"));
            match name.and_then(file) {
                Some(mut d) => {
                    if p.starts_with("/test/wrong-") && !d.is_empty() {
                        d[0] ^= 1;
                    }
                    reply("200 OK", "", &d)
                }
                None => reply("404 Not Found", "", b""),
            }
        }
    }
}

/// Where a test keeps the CA it trusts.
pub fn write_ca(ca: &TestCa, path: &Path) -> io::Result<PathBuf> {
    std::fs::write(path, ca.ca_pem())?;
    Ok(path.to_path_buf())
}
