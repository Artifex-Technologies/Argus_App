//! The Limbo rebuild against a fake Limbo agent socket that speaks Limbo's HTTP API and plays the
//! guest's part (what the fetch and build commands print).
#![cfg(unix)]

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use argus_tools::api::{FirmwareApi, Status};
use argus_tools::limbo::{self, BuildOptions, Source};
use base64::Engine;
use common::*;
use serde_json::{json, Value};

const FW: FirmwareApi = FirmwareApi { major: 88, minor: 2, target: 7 };
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

#[derive(Default)]
struct Seen {
    runs: Vec<Value>,
    polls: usize,
}

/// What the fake guest prints for each run.
type Guest = Arc<dyn Fn(&Value) -> (String, String, i64) + Send + Sync>;

fn serve(sock: &Path, guest: Guest, seen: Arc<Mutex<Seen>>) {
    let l = UnixListener::bind(sock).unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { break };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            let (method, path) = {
                let mut w = line.split_whitespace();
                (w.next().unwrap_or("").to_string(), w.next().unwrap_or("").to_string())
            };
            let mut len = 0;
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                if h == "\r\n" || h.is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            let (status, out, chunked): (u16, Value, bool) = match (method.as_str(), path.as_str()) {
                ("GET", "/v1/profiles") => (200, json!({"profiles": ["argus-fap-build", "argus-fap-fetch"], "details": [], "skipped": []}), false),
                ("POST", "/v1/runs") => {
                    let v: Value = serde_json::from_slice(&body).unwrap();
                    let mut st = seen.lock().unwrap();
                    st.runs.push(v);
                    (202, json!({"id": format!("r{:08x}", st.runs.len())}), false)
                }
                ("GET", p) if p.starts_with("/v1/runs/r") => {
                    let n: usize = usize::from_str_radix(&p["/v1/runs/r".len()..], 16).unwrap();
                    let mut st = seen.lock().unwrap();
                    st.polls += 1;
                    if st.polls % 2 == 1 {
                        (200, json!({"id": &p[9..], "status": "running"}), false)
                    } else {
                        let req = st.runs[n - 1].clone();
                        let (stdout, stderr, exit) = guest(&req);
                        (200, json!({"id": &p[9..], "profile": req["profile"], "status": "done", "exit_code": exit, "stdout": stdout, "stderr": stderr}), true)
                    }
                }
                _ => (404, json!({"error": "not_found: no", "code": "not_found"}), false),
            };
            let text = out.to_string();
            let resp = if chunked {
                // Go's net/http sends large bodies chunked
                let mid = text.len() / 2;
                format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n{:x}\r\n{}\r\n0\r\n\r\n",
                    mid,
                    &text[..mid],
                    text.len() - mid,
                    &text[mid..]
                )
            } else {
                format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{text}", text.len())
            };
            s.write_all(resp.as_bytes()).unwrap();
        }
    });
}

fn fap_block(name: &str, data: &[u8], sha: &str) -> String {
    format!("ARGUS-FAP-BEGIN {name} {sha}\n{}\nARGUS-FAP-END\n", B64.encode(data))
}

fn source_dir(root: &Path) -> PathBuf {
    let d = root.join("app");
    std::fs::create_dir_all(d.join("images")).unwrap();
    std::fs::create_dir_all(d.join(".git")).unwrap();
    std::fs::write(d.join("application.fam"), "App(appid='snake_game', apptype=FlipperAppType.EXTERNAL)\n").unwrap();
    std::fs::write(d.join("snake.c"), "int main(void) { return 0; }\n").unwrap();
    std::fs::write(d.join("images/icon.png"), [0x89, b'P', b'N', b'G']).unwrap();
    std::fs::write(d.join(".git/config"), "[core]\n").unwrap();
    std::fs::write(d.join("demo.gif"), vec![0; 100_000]).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", d.join("passwd")).unwrap();
    d
}

/// The files inside a run's bundle (tar holding src.tgz).
fn bundle_files(req: &Value) -> Vec<String> {
    let raw = B64.decode(req["bundle"].as_str().unwrap()).unwrap();
    let mut outer = tar::Archive::new(raw.as_slice());
    let mut e = outer.entries().unwrap().next().unwrap().unwrap();
    assert_eq!(e.path().unwrap().to_string_lossy(), "src.tgz");
    let mut tgz = Vec::new();
    e.read_to_end(&mut tgz).unwrap();
    let mut inner = tar::Archive::new(flate2::read::GzDecoder::new(tgz.as_slice()));
    let mut names: Vec<String> = inner.entries().unwrap().map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

fn opts<'a>(sock: &'a Path, out: &'a Path) -> BuildOptions<'a> {
    BuildOptions { socket: sock, build_profile: "argus-fap-build", fetch_profile: "argus-fap-fetch", fw: FW, out_dir: out, timeout: 60, poll: Duration::from_millis(1) }
}

#[test]
fn builds_a_local_folder_in_a_sealed_guest() {
    let d = tempfile::tempdir().unwrap();
    let sock = d.path().join("agent.sock");
    let seen = Arc::new(Mutex::new(Seen::default()));
    let snake = read_fixture("snake_game.fap");
    let sha = argus_tools::sha256_hex(&snake);
    let s2 = snake.clone();
    serve(&sock, Arc::new(move |_req| (fap_block("snake_game.fap", &s2, &sha), "ufbt: built\n".into(), 0)), seen.clone());

    let src = source_dir(d.path());
    let out = d.path().join("out");
    let mut log = Vec::new();
    let r = limbo::build(Source::Folder(&src), &opts(&sock, &out), &mut |m| log.push(m.to_string())).unwrap();
    assert_eq!(r.faps.len(), 1);
    let f = &r.faps[0];
    assert_eq!(f.file_name, "snake_game.fap");
    assert_eq!(f.compat.status, Status::Compatible);
    assert_eq!(std::fs::read(&f.stored).unwrap(), snake);
    assert_eq!(r.log_tail, "ufbt: built\n");

    let st = seen.lock().unwrap();
    assert_eq!(st.runs.len(), 1, "a local folder needs no fetch run");
    let req = &st.runs[0];
    assert_eq!(req["profile"], "argus-fap-build");
    assert!(req["command"].as_str().unwrap().contains("UFBT_HOME=/opt/argus-ufbt/f7-88.2"));
    assert_eq!(bundle_files(req), vec!["application.fam", "images/icon.png", "snake.c"], "no .git, no gif, no symlink");
}

#[test]
fn builds_from_git_with_a_fetch_run_first() {
    let d = tempfile::tempdir().unwrap();
    let sock = d.path().join("agent.sock");
    let seen = Arc::new(Mutex::new(Seen::default()));
    let snake = read_fixture("snake_game.fap");
    let tgz = limbo::pack_source(&source_dir(d.path())).unwrap().tgz;
    serve(
        &sock,
        Arc::new(move |req| {
            if req["profile"] == "argus-fap-fetch" {
                assert!(req.get("bundle").is_none());
                let out = format!("ARGUS-SRC-BEGIN {}\n{}\nARGUS-SRC-END\n", argus_tools::sha256_hex(&tgz), B64.encode(&tgz));
                (out, String::new(), 0)
            } else {
                (fap_block("snake_game.fap", &snake, &argus_tools::sha256_hex(&snake)), String::new(), 0)
            }
        }),
        seen.clone(),
    );
    let out = d.path().join("out");
    let src = Source::Git { url: "https://github.com/example/flipper-snake", git_ref: Some("v1.2"), subdir: "apps/snake" };
    let r = limbo::build(src, &opts(&sock, &out), &mut |_| {}).unwrap();
    assert_eq!(r.faps.len(), 1);
    assert!(r.fetch_run.is_some());
    let st = seen.lock().unwrap();
    assert_eq!(st.runs.len(), 2);
    let fetch = st.runs[0]["command"].as_str().unwrap();
    assert!(fetch.contains("--branch 'v1.2'") && fetch.contains("'https://github.com/example/flipper-snake'") && fetch.contains("cd 'apps/snake'"));
    assert!(fetch.contains("--no-recurse-submodules"));
    assert_eq!(bundle_files(&st.runs[1]), vec!["application.fam", "images/icon.png", "snake.c"]);
}

#[test]
fn bad_guest_output_is_refused() {
    let snake = read_fixture("snake_game.fap");
    let out = tempfile::tempdir().unwrap();
    // wrong hash
    let s = fap_block("snake_game.fap", &snake, &"0".repeat(64));
    assert_eq!(limbo::parse_build_output(&s, &FW, out.path()).unwrap_err().code, "hash_mismatch");
    // not base64
    let s = "ARGUS-FAP-BEGIN x.fap abc\n$$$\nARGUS-FAP-END\n";
    assert_eq!(limbo::parse_build_output(s, &FW, out.path()).unwrap_err().code, "bad_output");
    // the guest says the SDK is missing
    let e = limbo::parse_build_output("ARGUS-ERROR sdk_missing 88.2\n", &FW, out.path()).unwrap_err();
    assert!(e.message.contains("no ufbt SDK for API 88.2"));
    // nothing at all
    assert_eq!(limbo::parse_build_output("hello", &FW, out.path()).unwrap_err().code, "no_fap");
    // a hostile name is replaced, the file is still checked
    let junk = b"\x7fELF not really".to_vec();
    let s = fap_block("../../evil.fap", &junk, &argus_tools::sha256_hex(&junk));
    let r = limbo::parse_build_output(&s, &FW, out.path()).unwrap();
    assert!(!r[0].file_name.contains('/'));
    assert_eq!(r[0].compat.code, "not_a_fap");
    assert!(r[0].stored.starts_with(out.path()));
    // an incompatible build is reported as such
    let old = with_api(&snake, 86, 0, 7);
    let r = limbo::parse_build_output(&fap_block("a.fap", &old, &argus_tools::sha256_hex(&old)), &FW, out.path()).unwrap();
    assert_eq!(r[0].compat.code, "api_too_old");
}

#[test]
fn inputs_are_validated_before_anything_runs() {
    for u in [
        "http://github.com/a/b",
        "https://github.com/a/b'; rm -rf ~; '",
        "https://github.com/a/$(id)",
        "https://-oProxyCommand=x/a",
        "https://github.com/a/../../b",
        "file:///etc",
        "https://github.com/a b",
    ] {
        assert!(!limbo::valid_git_url(u), "{u}");
        assert!(limbo::fetch_command(u, None, "").is_err());
    }
    assert!(limbo::valid_git_url("https://github.com/flipperdevices/flipperzero-good-faps.git"));
    for r in ["-x", "a..b", "a b", "$(x)", ""] {
        assert!(!limbo::valid_ref(r), "{r}");
    }
    for s in ["/abs", "../x", "a/../b", "a//b", "-x", "a b"] {
        assert!(!limbo::valid_subdir(s), "{s}");
    }
    assert!(limbo::pack_source(Path::new("/nonexistent")).is_err());
}

#[test]
fn a_source_too_big_for_limbos_bundle_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("big");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("application.fam"), "App()\n").unwrap();
    // incompressible bytes
    let mut x: u64 = 0x9E3779B97F4A7C15;
    let noise: Vec<u8> = (0..100_000).map(|_| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x as u8
    }).collect();
    std::fs::write(src.join("blob.bin"), noise).unwrap();
    let p = limbo::pack_source(&src).unwrap();
    assert_eq!(limbo::bundle(&p.tgz).unwrap_err().code, "bundle_too_big");
}

#[test]
fn chunked_and_errors_from_limbo() {
    let (st, body) = limbo::parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n").unwrap();
    assert_eq!((st, body.as_slice()), (200, b"abcde".as_slice()));
    assert!(limbo::parse_response(b"garbage").is_err());
    assert!(limbo::parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n").is_err());
    let c = limbo::Client::new(Path::new("/nonexistent/agent.sock"));
    assert_eq!(c.profiles().unwrap_err().code, "limbo_unreachable");
}
