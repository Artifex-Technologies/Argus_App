//! Rebuild a Flipper app for the device's firmware with ufbt, inside disposable Limbo guests.
//!
//! App source from the internet is untrusted (`application.fam` is Python that ufbt runs), so it
//! is never built on this PC. Argus talks to Limbo's agent socket (`/run/limbo/agent.sock`,
//! group `limbo-agent`), the same validation-run interface Sarina uses (Limbo docs/QUARANTINE.md):
//!
//! - **Fetch** (git URL only), profile `argus-fap-fetch`: network `internet-only`, no sudo, no
//!   bundle. The guest runs `git clone --depth 1` (no submodules, nothing from the repo is
//!   executed), packs the app folder as a .tgz and prints it as base64 between markers. Argus
//!   treats those bytes as opaque: it checks their SHA-256 and size and never unpacks them.
//! - **Build**, profile `argus-fap-build`: network `none`, no sudo, `allow_bundle: true`. Argus
//!   sends the .tgz (from the fetch, or packed here from a local folder) as the run's bundle; the
//!   guest unpacks it, runs `ufbt build` against the SDK baked into its image for the device's
//!   API (`/opt/argus-ufbt/f7-<major>.<minor>`), and prints each built .fap as base64 between
//!   markers with its SHA-256. The build log goes to stderr.
//!
//! The hand-back is the run's own output channel, because a validation run has no shared
//! folder: Limbo's exchange outbox (ClamAV-scanned, `limbo exchange take`) belongs to registered
//! instances, and a run's guest is torn down when it ends. Argus decodes the .fap, checks the
//! SHA-256 the guest printed, parses its manifest and checks it against the device's API before
//! offering it. The bundle is capped at 40 KiB by Limbo, so large sources do not fit (see
//! argus-tools/limbo/README.md).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine;
use serde::Serialize;
use serde_json::{json, Value};

use crate::api::{self, Compat, FirmwareApi};
use crate::fap::{self, Manifest};
use crate::{Error, Result};

pub const DEFAULT_SOCKET: &str = "/run/limbo/agent.sock";
pub const BUILD_PROFILE: &str = "argus-fap-build";
pub const FETCH_PROFILE: &str = "argus-fap-fetch";
/// Limbo's server-side cap on a run's bundle (internal/runs/runs.go maxBundle).
pub const MAX_BUNDLE: usize = 40 << 10;
/// Local source folders: refuse anything this big before compressing.
pub const MAX_SOURCE_BYTES: u64 = 32 << 20;
pub const MAX_SOURCE_FILES: usize = 5000;
const MAX_FAP: usize = 4 << 20;
const MAX_RESPONSE: usize = 32 << 20;

/// Folders and files that never matter for a build and only make the bundle bigger.
const SKIP_NAMES: &[&str] = &[".git", ".github", ".vscode", ".idea", "dist", "build", "screenshots", ".catalog", "docs", "node_modules", "__pycache__", ".ufbt"];
const SKIP_EXT: &[&str] = &["gif", "jpg", "jpeg", "mp4", "webm", "mov", "pdf", "zip", "fap", "elf", "o"];

#[derive(Debug, Clone, Serialize)]
pub struct Packed {
    #[serde(skip)]
    pub tgz: Vec<u8>,
    pub files: usize,
    pub raw_bytes: u64,
    pub tgz_bytes: usize,
    pub skipped: Vec<String>,
}

fn skip_entry(name: &str, is_dir: bool) -> bool {
    if SKIP_NAMES.contains(&name) {
        return true;
    }
    !is_dir && name.rsplit_once('.').is_some_and(|(_, e)| SKIP_EXT.contains(&e.to_ascii_lowercase().as_str()))
}

/// Pack a local app folder (the one holding application.fam) as a deterministic .tgz: regular
/// files only (symlinks and specials are left out and listed), owner 0, mode 0644, mtime 0.
pub fn pack_source(dir: &Path) -> Result<Packed> {
    if !dir.join("application.fam").is_file() {
        return Err(Error::new("no_manifest", "the folder has no application.fam; choose the app's own folder"));
    }
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let mut skipped = Vec::new();
    let mut raw = 0u64;
    let mut stack = vec![(dir.to_path_buf(), String::new(), 0usize)];
    while let Some((d, rel, depth)) = stack.pop() {
        if depth > 16 {
            skipped.push(format!("{rel}: nested too deep"));
            continue;
        }
        let mut entries: Vec<_> = std::fs::read_dir(&d)?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let path_rel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            let ft = e.file_type()?;
            if skip_entry(&name, ft.is_dir()) {
                continue;
            }
            if ft.is_symlink() {
                skipped.push(format!("{path_rel}: symlink left out"));
            } else if ft.is_dir() {
                stack.push((e.path(), path_rel, depth + 1));
            } else if ft.is_file() {
                raw += e.metadata()?.len();
                files.push((path_rel, e.path()));
            } else {
                skipped.push(format!("{path_rel}: not a regular file"));
            }
            if raw > MAX_SOURCE_BYTES || files.len() > MAX_SOURCE_FILES {
                return Err(Error::new("source_too_big", "the folder is too big to be one Flipper app's source"));
            }
        }
    }
    files.sort();
    let mut tar = tar::Builder::new(Vec::new());
    tar.mode(tar::HeaderMode::Deterministic);
    for (rel, path) in &files {
        let data = std::fs::read(path)?;
        let mut h = tar::Header::new_ustar();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_mtime(0);
        h.set_uid(0);
        h.set_gid(0);
        h.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut h, rel, data.as_slice())?;
    }
    let tar_bytes = tar.into_inner()?;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(&tar_bytes)?;
    let tgz = gz.finish()?;
    Ok(Packed { tgz_bytes: tgz.len(), tgz, files: files.len(), raw_bytes: raw, skipped })
}

/// The run's bundle: an uncompressed tar holding only `src.tgz` (Limbo's guest unpacks the bundle
/// with plain `tar -x`, so the compression has to be one level in).
pub fn bundle(tgz: &[u8]) -> Result<Vec<u8>> {
    let mut tar = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_ustar();
    h.set_size(tgz.len() as u64);
    h.set_mode(0o644);
    h.set_mtime(0);
    h.set_entry_type(tar::EntryType::Regular);
    tar.append_data(&mut h, "src.tgz", tgz)?;
    let b = tar.into_inner()?;
    if b.len() > MAX_BUNDLE {
        return Err(Error::new(
            "bundle_too_big",
            format!(
                "the compressed source is {} KiB; Limbo carries at most {} KiB into a sealed guest. Remove assets the build does not need, or see argus-tools/limbo/README.md",
                tgz.len() / 1024,
                MAX_BUNDLE / 1024
            ),
        ));
    }
    Ok(b)
}

pub fn valid_git_url(u: &str) -> bool {
    let Some(rest) = u.strip_prefix("https://") else { return false };
    let Some((host, path)) = rest.split_once('/') else { return false };
    u.len() <= 300
        && !host.is_empty()
        && host.chars().all(|c| c.is_ascii_alphanumeric() || ".-:".contains(c))
        && !host.starts_with('-')
        && !path.is_empty()
        && path.chars().all(|c| c.is_ascii_alphanumeric() || "._~/-".contains(c))
        && !path.split('/').any(|p| p == "..")
}

pub fn valid_ref(r: &str) -> bool {
    !r.is_empty() && r.len() <= 100 && !r.starts_with('-') && !r.contains("..") && r.chars().all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c))
}

pub fn valid_subdir(s: &str) -> bool {
    s.is_empty()
        || (s.len() <= 200
            && !s.starts_with('/')
            && !s.starts_with('-')
            && s.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
            && s.chars().all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c)))
}

const SRC_BEGIN: &str = "ARGUS-SRC-BEGIN";
const SRC_END: &str = "ARGUS-SRC-END";
const FAP_BEGIN: &str = "ARGUS-FAP-BEGIN";
const FAP_END: &str = "ARGUS-FAP-END";
const ERR_MARK: &str = "ARGUS-ERROR";

/// Guest command for the fetch run. Every input was validated to a strict character set, and
/// is single-quoted on top of that.
pub fn fetch_command(url: &str, git_ref: Option<&str>, subdir: &str) -> Result<String> {
    if !valid_git_url(url) {
        return Err(Error::new("bad_url", "give an https:// git URL (letters, digits and ._~/- only)"));
    }
    if git_ref.is_some_and(|r| !valid_ref(r)) {
        return Err(Error::new("bad_ref", "a branch or tag name is letters, digits and ._/- only"));
    }
    if !valid_subdir(subdir) {
        return Err(Error::new("bad_subdir", "the app folder is a relative path inside the repository"));
    }
    let branch = git_ref.map(|r| format!("--branch '{r}' ")).unwrap_or_default();
    let dir = if subdir.is_empty() { ".".to_string() } else { format!("'{subdir}'") };
    let excludes: String = SKIP_NAMES
        .iter()
        .map(|n| format!("--exclude='{n}' "))
        .chain(SKIP_EXT.iter().map(|e| format!("--exclude='*.{e}' ")))
        .collect();
    Ok(format!(
        "set -eu; export GIT_TERMINAL_PROMPT=0; \
         cd \"$HOME\" && rm -rf repo && git clone --quiet --depth 1 --no-recurse-submodules --single-branch {branch}-- '{url}' repo >&2; \
         cd repo && cd {dir}; \
         test -f application.fam || {{ echo '{ERR_MARK} no_manifest'; exit 3; }}; \
         out=\"$HOME/argus-src.tgz\"; tar --owner=0 --group=0 --numeric-owner --mtime=@0 --sort=name {excludes}-czf \"$out\" .; \
         echo '{SRC_BEGIN}' \"$(sha256sum < \"$out\" | cut -c1-64)\"; base64 -w0 \"$out\"; echo; echo '{SRC_END}'"
    ))
}

/// Guest command for the build run (the bundle is unpacked in ~/agent, the command runs there).
pub fn build_command(fw: &FirmwareApi) -> String {
    let api = fw.text();
    format!(
        "set -u; mkdir src && tar -xzf src.tgz -C src --no-same-owner --no-same-permissions || {{ echo '{ERR_MARK} bad_source'; exit 3; }}; \
         export UFBT_HOME=/opt/argus-ufbt/f7-{api}; \
         test -d \"$UFBT_HOME\" || {{ echo '{ERR_MARK} sdk_missing {api}'; exit 3; }}; \
         cd src; (ufbt build > ../build.log 2>&1); rc=$?; tail -c 60000 ../build.log >&2; \
         test $rc -eq 0 || {{ echo '{ERR_MARK} build_failed'; exit 4; }}; \
         found=0; for f in dist/*.fap; do test -f \"$f\" || continue; found=1; \
         echo '{FAP_BEGIN}' \"$(basename \"$f\" | tr -cd 'A-Za-z0-9._-')\" \"$(sha256sum < \"$f\" | cut -c1-64)\"; base64 -w0 \"$f\"; echo; echo '{FAP_END}'; done; \
         test $found -eq 1 || {{ echo '{ERR_MARK} no_fap'; exit 4; }}"
    )
}

/// An error marker the guest printed, if any (code and the rest of its line).
pub fn guest_error(stdout: &str) -> Option<(String, String)> {
    stdout.lines().find_map(|l| {
        let rest = l.trim().strip_prefix(ERR_MARK)?.trim();
        let (code, more) = rest.split_once(' ').unwrap_or((rest, ""));
        Some((crate::display_text(code.as_bytes(), 40), crate::display_text(more.as_bytes(), 80)))
    })
}

/// Blocks between `begin` and `end` markers: (words after begin, decoded payload).
fn blocks(stdout: &str, begin: &str, end: &str, max: usize) -> Result<Vec<(Vec<String>, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut lines = stdout.lines();
    while let Some(l) = lines.next() {
        let Some(rest) = l.trim().strip_prefix(begin) else { continue };
        let words: Vec<String> = rest.split_whitespace().map(|w| crate::display_text(w.as_bytes(), 80)).collect();
        let mut payload = String::new();
        for l in lines.by_ref() {
            if l.trim() == end {
                break;
            }
            if payload.len() > max * 4 / 3 + 8 {
                return Err(Error::new("output_too_big", "the guest sent more than Argus accepts"));
            }
            payload.push_str(l.trim());
        }
        let data = base64::engine::general_purpose::STANDARD
            .decode(payload.as_bytes())
            .map_err(|_| Error::new("bad_output", "the guest's output is not valid base64"))?;
        if data.len() > max {
            return Err(Error::new("output_too_big", "the guest sent more than Argus accepts"));
        }
        out.push((words, data));
    }
    Ok(out)
}

/// The source .tgz from a fetch run, checked against the SHA-256 the guest printed.
pub fn parse_fetch_output(stdout: &str) -> Result<Vec<u8>> {
    if let Some((code, _)) = guest_error(stdout) {
        return Err(Error::new("fetch_failed", format!("the guest reported {code}")));
    }
    let b = blocks(stdout, SRC_BEGIN, SRC_END, 4 << 20)?;
    let [(words, data)] = b.as_slice() else {
        return Err(Error::new("bad_output", "the fetch run did not return exactly one source archive"));
    };
    if words.first().map(String::as_str) != Some(crate::sha256_hex(data).as_str()) {
        return Err(Error::new("hash_mismatch", "the source archive does not match the hash the guest printed"));
    }
    if !data.starts_with(&[0x1f, 0x8b]) {
        return Err(Error::new("bad_output", "the source archive is not gzip"));
    }
    Ok(data.clone())
}

#[derive(Debug, Clone, Serialize)]
pub struct BuiltFap {
    pub file_name: String,
    pub sha256: String,
    pub size: usize,
    pub stored: PathBuf,
    pub manifest: Option<Manifest>,
    pub compat: Compat,
}

/// The .fap files from a build run: hash-checked, parsed, judged against `fw`, and stored in
/// `out_dir` as `<sha256>.fap`.
pub fn parse_build_output(stdout: &str, fw: &FirmwareApi, out_dir: &Path) -> Result<Vec<BuiltFap>> {
    if let Some((code, more)) = guest_error(stdout) {
        let msg = match code.as_str() {
            "sdk_missing" => format!("the build guest's image has no ufbt SDK for API {more}; see argus-tools/limbo/README.md"),
            "build_failed" => "ufbt build failed; the build log is in the run's stderr".to_string(),
            "no_fap" => "the build produced no .fap".to_string(),
            "bad_source" => "the guest could not unpack the source".to_string(),
            "no_manifest" => "the source has no application.fam".to_string(),
            other => format!("the guest reported {other}"),
        };
        return Err(Error::new("build_failed", msg));
    }
    std::fs::create_dir_all(out_dir)?;
    let table = api::builtin_table(fw);
    let mut out = Vec::new();
    for (words, data) in blocks(stdout, FAP_BEGIN, FAP_END, MAX_FAP)? {
        let name = words.first().cloned().unwrap_or_default();
        let sha = crate::sha256_hex(&data);
        if words.get(1) != Some(&sha) {
            return Err(Error::new("hash_mismatch", format!("{name}: the file does not match the hash the guest printed")));
        }
        let stored = out_dir.join(format!("{sha}.fap"));
        crate::store::write_atomic(&stored, &data)?;
        let (manifest, compat) = match fap::inspect(&data) {
            Ok(info) => (Some(info.manifest.clone()), api::check(&info, fw, table.as_ref())),
            Err(e) => (None, api::not_a_fap(fw, &e.message)),
        };
        let file_name = if name.ends_with(".fap") && name.len() <= 64 && !name.starts_with('.') { name } else { format!("{}.fap", &sha[..8]) };
        out.push(BuiltFap { file_name, sha256: sha, size: data.len(), stored, manifest, compat });
    }
    if out.is_empty() {
        return Err(Error::new("no_fap", "the build run returned no .fap"));
    }
    Ok(out)
}

// ---- Limbo's HTTP API over a unix socket ------------------------------------------------------

pub struct Client {
    pub socket: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunResult {
    pub id: String,
    pub status: String,
    pub exit_code: Option<i64>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub error: String,
    pub code: String,
}

fn dechunk(body: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let nl = rest.windows(2).position(|w| w == b"\r\n").ok_or_else(|| Error::new("bad_http", "bad chunked body"))?;
        let size_text = std::str::from_utf8(&rest[..nl]).map_err(|_| Error::new("bad_http", "bad chunk size"))?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| Error::new("bad_http", "bad chunk size"))?;
        rest = &rest[nl + 2..];
        if size == 0 {
            return Ok(out);
        }
        if rest.len() < size + 2 {
            return Err(Error::new("bad_http", "chunk cut short"));
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

/// Split a raw HTTP/1.1 response into (status, body).
pub fn parse_response(raw: &[u8]) -> Result<(u16, Vec<u8>)> {
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| Error::new("bad_http", "no header end"))?;
    let head = String::from_utf8_lossy(&raw[..end]);
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Error::new("bad_http", "no status line"))?;
    let chunked = lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    let body = &raw[end + 4..];
    Ok((status, if chunked { dechunk(body)? } else { body.to_vec() }))
}

impl Client {
    pub fn new(socket: &Path) -> Client {
        Client { socket: socket.to_path_buf() }
    }

    #[cfg(unix)]
    fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Value)> {
        use std::os::unix::net::UnixStream;
        let mut s = UnixStream::connect(&self.socket).map_err(|e| {
            Error::new(
                "limbo_unreachable",
                format!("cannot reach Limbo at {} ({e}); is limbo serve running and are you in the limbo-agent group?", self.socket.display()),
            )
        })?;
        s.set_read_timeout(Some(Duration::from_secs(60)))?;
        let payload = body.map(|b| b.to_string()).unwrap_or_default();
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: limbo\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        );
        s.write_all(req.as_bytes())?;
        let mut raw = Vec::new();
        s.take(MAX_RESPONSE as u64 + 1).read_to_end(&mut raw)?;
        if raw.len() > MAX_RESPONSE {
            return Err(Error::new("bad_http", "Limbo's answer is too large"));
        }
        let (status, body) = parse_response(&raw)?;
        let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        Ok((status, v))
    }

    #[cfg(not(unix))]
    fn request(&self, _: &str, _: &str, _: Option<&Value>) -> Result<(u16, Value)> {
        Err(Error::new("unsupported", "Limbo runs on Linux only"))
    }

    fn fail(v: &Value, status: u16) -> Error {
        let code = v.get("code").and_then(Value::as_str).unwrap_or("limbo_error").to_string();
        let msg = v.get("error").and_then(Value::as_str).map(String::from).unwrap_or(format!("Limbo answered HTTP {status}"));
        Error::new("limbo_refused", format!("{code}: {}", crate::display_text(msg.as_bytes(), 400)))
    }

    pub fn profiles(&self) -> Result<Value> {
        let (st, v) = self.request("GET", "/v1/profiles", None)?;
        if st != 200 {
            return Err(Self::fail(&v, st));
        }
        Ok(v)
    }

    pub fn start(&self, profile: &str, command: &str, timeout: u32, bundle: Option<&[u8]>) -> Result<String> {
        let mut body = json!({"profile": profile, "command": command, "timeout_seconds": timeout});
        if let Some(b) = bundle {
            body["bundle"] = json!(base64::engine::general_purpose::STANDARD.encode(b));
        }
        let (st, v) = self.request("POST", "/v1/runs", Some(&body))?;
        if st != 202 && st != 200 {
            return Err(Self::fail(&v, st));
        }
        v.get("id")
            .and_then(Value::as_str)
            .filter(|id| id.len() <= 16 && id.chars().all(|c| c.is_ascii_alphanumeric()))
            .map(String::from)
            .ok_or_else(|| Error::new("bad_http", "Limbo did not return a run id"))
    }

    pub fn get(&self, id: &str) -> Result<RunResult> {
        let (st, v) = self.request("GET", &format!("/v1/runs/{id}"), None)?;
        if st != 200 {
            return Err(Self::fail(&v, st));
        }
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        Ok(RunResult {
            id: s("id"),
            status: s("status"),
            exit_code: v.get("exit_code").and_then(Value::as_i64),
            stdout: s("stdout"),
            stderr: s("stderr"),
            truncated: v.get("truncated").and_then(Value::as_bool).unwrap_or(false),
            error: s("error"),
            code: s("code"),
        })
    }

    /// Poll until the run is no longer "running" (or `limit` passes).
    pub fn wait(&self, id: &str, poll: Duration, limit: Duration, progress: &mut dyn FnMut(&str)) -> Result<RunResult> {
        let t0 = Instant::now();
        loop {
            let r = self.get(id)?;
            if r.status != "running" {
                return Ok(r);
            }
            if t0.elapsed() > limit {
                return Err(Error::new("timeout", format!("run {id} is still running after {} s", limit.as_secs())));
            }
            progress(&format!("run {id} running ({} s)", t0.elapsed().as_secs()));
            std::thread::sleep(poll);
        }
    }
}

pub enum Source<'a> {
    Folder(&'a Path),
    Git { url: &'a str, git_ref: Option<&'a str>, subdir: &'a str },
}

pub struct BuildOptions<'a> {
    pub socket: &'a Path,
    pub build_profile: &'a str,
    pub fetch_profile: &'a str,
    pub fw: FirmwareApi,
    pub out_dir: &'a Path,
    pub timeout: u32,
    pub poll: Duration,
}

#[derive(Debug, Serialize)]
pub struct BuildReport {
    pub source: String,
    pub source_tgz_bytes: usize,
    pub source_tgz_sha256: String,
    pub fetch_run: Option<String>,
    pub build_run: String,
    pub faps: Vec<BuiltFap>,
    /// The last part of the ufbt log (untrusted text from the guest).
    pub log_tail: String,
}

fn ran_ok(r: &RunResult, what: &str) -> Result<()> {
    if r.status == "done" {
        return Ok(());
    }
    Err(Error::new(
        "run_failed",
        format!("the {what} run ended {} {}", r.status, crate::display_text(format!("{} {}", r.code, r.error).trim().as_bytes(), 400)),
    ))
}

pub fn build(src: Source, o: &BuildOptions, progress: &mut dyn FnMut(&str)) -> Result<BuildReport> {
    let client = Client::new(o.socket);
    let (tgz, label, fetch_run) = match src {
        Source::Folder(dir) => {
            progress("packing the source folder");
            let p = pack_source(dir)?;
            (p.tgz, crate::display_text(dir.to_string_lossy().as_bytes(), 300), None)
        }
        Source::Git { url, git_ref, subdir } => {
            let cmd = fetch_command(url, git_ref, subdir)?;
            progress("starting the fetch guest (git clone)");
            let id = client.start(o.fetch_profile, &cmd, o.timeout.min(600), None)?;
            let r = client.wait(&id, o.poll, Duration::from_secs(o.timeout as u64 + 900), progress)?;
            if let Some((code, _)) = guest_error(&r.stdout) {
                return Err(Error::new("fetch_failed", format!("the fetch guest reported {code}")));
            }
            ran_ok(&r, "fetch")?;
            if r.exit_code != Some(0) {
                return Err(Error::new("fetch_failed", format!("git clone failed: {}", crate::display_text(r.stderr.as_bytes(), 400))));
            }
            (parse_fetch_output(&r.stdout)?, url.to_string(), Some(id))
        }
    };
    let b = bundle(&tgz)?;
    progress("starting the build guest (ufbt build, no network)");
    let id = client.start(o.build_profile, &build_command(&o.fw), o.timeout, Some(&b))?;
    let r = client.wait(&id, o.poll, Duration::from_secs(o.timeout as u64 + 900), progress)?;
    let log_tail = crate::display_log(r.stderr.as_bytes(), 20_000);
    if guest_error(&r.stdout).is_none() {
        ran_ok(&r, "build")?;
    }
    let faps = parse_build_output(&r.stdout, &o.fw, o.out_dir).map_err(|e| {
        if r.truncated {
            Error::new(e.code, format!("{} (the run's output was cut at the profile's max_output_bytes)", e.message))
        } else {
            e
        }
    })?;
    Ok(BuildReport {
        source: label,
        source_tgz_bytes: tgz.len(),
        source_tgz_sha256: crate::sha256_hex(&tgz),
        fetch_run,
        build_run: id,
        faps,
        log_tail,
    })
}
