//! The Rust Sarina API against answers recorded from the C++ server: tests/fixtures/api_requests.txt
//! is sent, line by line, to the Rust bridge in this process (no port is opened) set up as
//! compare-api.sh sets up both servers (the same files, an old-schema datastore, the same capture
//! library), and the answers, normalised the same way, must equal tests/fixtures/api_responses.txt,
//! which compare-api.sh --record wrote from `qFlipper-cli chymaera serve` (and checks again in CI).

use std::path::{Path, PathBuf};

use argus_api::bridge::Bridge;
use argus_api::server::{announce, Conn};

const TOKEN: &str = "compare-api-test-token";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

/// compare-api.sh's `setup`: returns the bridge and the folder standing in for the server's.
fn setup(d: &Path) -> Bridge {
    let files = d.join("files");
    copy_dir(&fixtures().join("api-files"), &files);
    std::fs::create_dir(files.join("folder.nfc")).unwrap();
    let fifo = std::ffi::CString::new(files.join("pipe.sub").to_str().unwrap()).unwrap();
    // SAFETY: a valid C string.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
    std::os::unix::fs::symlink("garage.sub", files.join("link.sub")).unwrap();
    let db = d.join("data/Argus/chymaera/chymaera.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let seed = std::fs::read_to_string(fixtures().join("api-seed.sql")).unwrap();
    rusqlite::Connection::open(&db).unwrap().execute_batch(&seed).unwrap();
    // `argus-tools capture index --library LIB --tree TREE`
    let lib_dir = d.join("data/argus/captures");
    let lib = argus_tools::capture::Library::open(&lib_dir).unwrap();
    let mut b = argus_tools::capture::Builder::default();
    b.add_tree(&fixtures().join("api-files/tree"), "pull").unwrap();
    lib.save_index(&b.finish(0)).unwrap();
    Bridge::new(&db, lib_dir)
}

fn replace(hay: &[u8], needle: &[u8], with: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < hay.len() {
        if hay[i..].starts_with(needle) {
            out.extend_from_slice(with);
            i += needle.len();
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    out
}

/// compare-api.sh's `normalise`.
fn normalise(line: &str, d: &Path) -> String {
    let mut s = line.to_string();
    for (key, quoted) in
        [("\"timestamp\":\"", true), ("\"epochMs\":", false), ("\"ts\":", false), ("\"serverPort\":", false), ("127.0.0.1:", false)]
    {
        let mut out = String::new();
        let mut rest = s.as_str();
        while let Some(i) = rest.find(key) {
            out.push_str(&rest[..i + key.len()]);
            rest = &rest[i + key.len()..];
            let end =
                if quoted { rest.find('"').unwrap_or(rest.len()) } else { rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len()) };
            if !quoted && end == 0 {
                continue;
            }
            out.push_str(match key {
                "\"timestamp\":\"" => "T",
                "127.0.0.1:" => "PORT",
                _ => "0",
            });
            rest = &rest[end..];
        }
        out.push_str(rest);
        s = out;
    }
    let d = d.display();
    s.replace(&format!("{d}/files"), "@FILES@").replace(&format!("{d}/data"), "@DATA@").replace(&format!("{d}/run"), "@RUN@")
}

#[test]
fn answers_as_recorded_from_the_cpp_server() {
    let tmp = tempdir();
    let d = tmp.as_path();
    let mut bridge = setup(d);
    announce(&mut bridge, 1, None);
    let garage = argus_tools::sha256_hex(&std::fs::read(fixtures().join("api-files/garage.sub")).unwrap())[..16].to_string();
    let requests = std::fs::read(fixtures().join("api_requests.txt")).unwrap();
    let mut conn = Conn::default();
    let mut answers = Vec::new();
    let files = format!("{}/files", d.display());
    for line in requests.split(|&b| b == b'\n') {
        // the script's substitutions, on bytes (one line carries a raw 0xff)
        let mut req =
            replace(&replace(&replace(line, b"@TOKEN@", TOKEN.as_bytes()), b"@FILES@", files.as_bytes()), b"@GARAGE@", garage.as_bytes());
        req.push(b'\n');
        answers.extend(conn.feed(&mut bridge, TOKEN.as_bytes(), &req));
    }
    // two requests in one write, then one in two writes
    let two = format!(
        "{{\"cmd\":\"ping\",\"token\":\"{TOKEN}\",\"id\":\"a\"}}\n{{\"cmd\":\"ping\",\"token\":\"{TOKEN}\",\"id\":\"b\"}}\n{{\"cmd\":\"pi"
    );
    answers.extend(conn.feed(&mut bridge, TOKEN.as_bytes(), two.as_bytes()));
    answers.extend(conn.feed(&mut bridge, TOKEN.as_bytes(), format!("ng\",\"token\":\"{TOKEN}\",\"id\":\"c\"}}\n").as_bytes()));
    let got: Vec<String> = String::from_utf8(answers).unwrap().lines().map(|l| normalise(l, d)).collect();
    let want_text = std::fs::read_to_string(fixtures().join("api_responses.txt")).unwrap();
    let want: Vec<&str> = want_text.lines().collect();
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        assert_eq!(g, w, "answer {}", i + 1);
    }
    assert_eq!(got.len(), want.len());
}

/// A private temporary folder, removed afterwards.
struct TempDir(PathBuf);

impl TempDir {
    fn as_path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempdir() -> TempDir {
    use std::os::unix::fs::DirBuilderExt;
    let p = std::env::temp_dir().join(format!("argus-api-test-{}-{}", std::process::id(), argus_api::eventlog::now_ms()));
    std::fs::DirBuilder::new().mode(0o700).create(&p).unwrap();
    TempDir(p)
}
