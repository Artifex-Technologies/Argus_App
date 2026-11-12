//! HTTPS GET for the update server, the port of `RemoteFileFetcher`: rustls (ring) with the system's root
//! certificates (`rustls-native-certs`, which also honours `SSL_CERT_FILE`/`SSL_CERT_DIR`), https only (also
//! after a redirect, at most 3), no compression asked for, bounded time and a bounded body: a reply larger than
//! its limit is an error, never a truncated file. A file from the directory is only handed out once its
//! SHA-256 matched.

use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::directory::{checked_sha256, FileInfo};

/// The largest firmware file fetched (Flipper's update package and resources are about 10 MiB each).
pub const MAX_FILE: u64 = 64 << 20;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// No usable root certificates, or a URL that is not https.
    Setup(String),
    /// The server could not be reached, refused, or the transfer failed or took too long.
    Network(String),
    /// The reply is larger than its limit.
    TooLarge { url: String, limit: u64 },
    /// The bytes are not what the directory says (`RemoteFileFetcher`'s "File integrity check failed").
    Integrity { url: String, expected: String, got: String },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Setup(m) | Error::Network(m) => f.write_str(m),
            Error::TooLarge { url, limit } => write!(f, "{url}: the reply is larger than {limit} bytes"),
            Error::Integrity { url, expected, got } => {
                write!(f, "{url}: file integrity check failed (SHA-256 {got}, the directory says {expected})")
            }
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::Setup(_) => "setup",
            Error::Network(_) => "network",
            Error::TooLarge { .. } => "too_large",
            Error::Integrity { .. } => "integrity",
        }
    }
}

/// How long one request may take in all (a 64 MiB file over a slow line still fits).
pub const TIMEOUT: Duration = Duration::from_secs(600);
/// How long connecting may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

pub struct Client {
    agent: ureq::Agent,
}

impl Client {
    /// A client that trusts the system's root certificates (or `SSL_CERT_FILE`/`SSL_CERT_DIR`).
    pub fn new() -> Result<Client, Error> {
        let found = rustls_native_certs::load_native_certs();
        let roots: Vec<ureq::tls::Certificate<'static>> =
            found.certs.iter().map(|c| ureq::tls::Certificate::from_der(c.as_ref()).to_owned()).collect();
        if roots.is_empty() {
            let why = found.errors.first().map(|e| format!(": {e}")).unwrap_or_default();
            return Err(Error::Setup(format!("no root certificates found{why}")));
        }
        Ok(Client::with_roots(roots, TIMEOUT))
    }

    /// A client that trusts only these roots.
    pub fn with_roots(roots: Vec<ureq::tls::Certificate<'static>>, timeout: Duration) -> Client {
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::Rustls)
            .root_certs(ureq::tls::RootCerts::from(roots))
            .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .build();
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(3)
            .timeout_global(Some(timeout))
            .timeout_connect(Some(CONNECT_TIMEOUT.min(timeout)))
            .user_agent("argus-cli")
            .tls_config(tls)
            .build();
        Client { agent: config.into() }
    }

    /// The body at `url`, at most `limit` bytes.
    pub fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, Error> {
        if !url.starts_with("https://") {
            return Err(Error::Setup(format!("{url}: only https URLs are fetched")));
        }
        let net = |e: ureq::Error| match e {
            ureq::Error::BodyExceedsLimit(_) => Error::TooLarge { url: url.to_string(), limit },
            e => Error::Network(format!("{url}: {e}")),
        };
        let mut reply = self.agent.get(url).call().map_err(net)?;
        if reply.body().content_length().is_some_and(|n| n > limit) {
            return Err(Error::TooLarge { url: url.to_string(), limit });
        }
        reply.body_mut().with_config().limit(limit).read_to_vec().map_err(net)
    }

    /// A file from the directory: at most `limit` bytes, and only if its SHA-256 is the directory's.
    pub fn get_file(&self, file: &FileInfo, limit: u64) -> Result<Vec<u8>, Error> {
        let expected = checked_sha256(&file.sha256).ok_or_else(|| Error::Setup(format!("{}: no SHA-256 to check it by", file.url)))?;
        let data = self.get(&file.url, limit)?;
        let got = hex::encode(Sha256::digest(&data));
        if got != expected {
            return Err(Error::Integrity { url: file.url.clone(), expected, got });
        }
        Ok(data)
    }
}
