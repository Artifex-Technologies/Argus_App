//! Differential test: the Rust directory and region parsers against the C++ app's own (`UpdateRegistry`,
//! `Flipper::Updates`, `RegionInfo`, the plugin's `RegionData`), run as the oracle in tests/qt-oracle (build it
//! with `sh build.sh OUT`, then `ARGUS_UPDATES_ORACLE=OUT cargo test -p flipper-updates --test qt_oracle`).
//! Without the variable the tests say so and pass; the Linux CI job, which has Qt, runs them.
//!
//! Inputs are generated from fixed seeds: directories and region bundles built from parts that are right,
//! missing, of the wrong type, out of range or at the edge (hashes of every length and case, URLs of every
//! scheme, versions that overflow or are not numbers, band values at and past the firmware's limits, band names
//! that do not exist), now and then cut short or not JSON at all. Both must read the same channels, versions,
//! files and latest versions, and make the same region data bytes (or both refuse).

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;

use flipper_updates::{directory, region};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
    fn chance(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }
}

fn oracle() -> Option<PathBuf> {
    let o = std::env::var_os("ARGUS_UPDATES_ORACLE");
    if o.is_none() {
        eprintln!("ARGUS_UPDATES_ORACLE not set: build tests/qt-oracle (sh build.sh OUT) to run this comparison");
    }
    o.map(PathBuf::from)
}

/// UTF-16 code units as the oracle prints them.
fn esc(s: &str) -> String {
    let mut r = String::new();
    for u in s.encode_utf16() {
        if u > 0x20 && u < 0x7f && u != u16::from(b'\\') && u != u16::from(b',') {
            r.push(char::from(u as u8));
        } else {
            write!(r, "\\u{u:04x}").unwrap();
        }
    }
    if r.is_empty() {
        "-".into()
    } else {
        r
    }
}

/// Adds `"key":value` to an object body now and then leaving it out or giving it another type.
fn field(rng: &mut Rng, parts: &mut Vec<String>, key: &str, value: String) {
    match rng.below(120) {
        0 => {}
        1 => parts.push(format!("\"{key}\":null")),
        2 => parts.push(format!("\"{key}\":5")),
        3 => parts.push(format!("\"{key}\":{{}}")),
        _ => parts.push(format!("\"{key}\":{value}")),
    }
}

fn s(x: &str) -> String {
    format!("\"{x}\"")
}

const SHA: &str = "0ffa9770ec5ad5d1fa9531299da40087f201969de3a65e95cbbea989342e2e80";

fn gen_file(rng: &mut Rng) -> String {
    let mut p = Vec::new();
    let v = s(rng.pick(&["f7", "any", "f18", "", "f7"]));
    field(rng, &mut p, "target", v);
    let v = s(rng.pick(&["full_dfu", "update_tgz", "resources_tgz", "core2_firmware_tgz", ""]));
    field(rng, &mut p, "type", v);
    let url = rng.pick(&[
        "https://update.invalid/firmware/a.dfu",
        "https://update.invalid/firmware/a.dfu",
        "https://update.invalid/firmware/a.dfu",
        "https://update.invalid/firmware/a.dfu",
        "https://update.invalid/firmware/a.dfu",
        "https://update.invalid/firmware/c.tgz",
        "https://update.invalid/firmware/c.tgz",
        "https://update.invalid/firmware/c.tgz",
        "https://update.invalid/firmware/c.tgz",
        "https://update.invalid/firmware/c.tgz",
        "https://update.invalid/firmware/a.dfu",
        "https://update.invalid/b.tgz",
        "http://update.invalid/a.dfu",
        "file:///etc/passwd",
        "HTTPS://update.invalid/a",
        "",
        "https://",
        "data:,x",
        "https://update.invalid/caf\u{e9}",
    ]);
    let v = s(url);
    field(rng, &mut p, "url", v);
    let upper = SHA.to_ascii_uppercase();
    let sha = match rng.below(40) {
        0 => String::new(),
        1 => SHA[..63].to_string(),
        2 => format!("{SHA}0"),
        3 => upper,
        4 => format!("{}g", &SHA[..63]),
        5 => format!(" {}", &SHA[..63]),
        _ => SHA.to_string(),
    };
    let v = s(&sha);
    field(rng, &mut p, "sha256", v);
    if rng.chance(6) {
        p.reverse();
    }
    format!("{{{}}}", p.join(","))
}

fn gen_version(rng: &mut Rng) -> String {
    let mut p = Vec::new();
    let number = rng.pick(&[
        "0.98.3",
        "0.99.1-rc",
        "1.0.0",
        "1.0.0",
        "c6d5ae6e",
        "",
        "100.0.0.0",
        "1.2",
        "9.9.9.9.9.9.9.9",
        "+1.2",
        "1..2",
        "0.98.3-rc2",
        "2147483.647.1",
        "999999999.999999999.999999999",
        "01.02",
        "1.2.3-",
        "\u{663}.1",
    ]);
    let v = s(number);
    field(rng, &mut p, "version", v);
    let v = s(rng.pick(&["", "fixes", "line\\nbreak", "caf\u{e9}"]));
    field(rng, &mut p, "changelog", v);
    let v = rng.pick(&["1700000000", "0", "-5", "1.5", "\"x\""]).to_string();
    field(rng, &mut p, "timestamp", v);
    let files: Vec<String> = (0..rng.below(4)).map(|_| gen_file(rng)).collect();
    let v = format!("[{}]", files.join(","));
    field(rng, &mut p, "files", v);
    format!("{{{}}}", p.join(","))
}

fn gen_directory(rng: &mut Rng) -> Vec<u8> {
    let channels: Vec<String> = (0..rng.below(4))
        .map(|_| {
            let mut p = Vec::new();
            let v = s(rng.pick(&["release", "rc", "development", "dev-x", "release", "", "Zeta"]));
            field(rng, &mut p, "id", v);
            let v = s(rng.pick(&["Release", "", "Dev"]));
            field(rng, &mut p, "title", v);
            let v = s(rng.pick(&["d", "", "t\u{e9}st"]));
            field(rng, &mut p, "description", v);
            let versions: Vec<String> = (0..rng.below(5)).map(|_| gen_version(rng)).collect();
            let v = format!("[{}]", versions.join(","));
            field(rng, &mut p, "versions", v);
            format!("{{{}}}", p.join(","))
        })
        .collect();
    let text = match rng.below(30) {
        0 => format!("[{}]", channels.join(",")),
        1 => format!("{{\"channel\":[{}]}}", channels.join(",")),
        2 => "{\"channels\":{}}".to_string(),
        _ => format!("{{\"channels\":[{}]}}", channels.join(",")),
    };
    let mut text = text.into_bytes();
    if rng.chance(25) {
        let cut = rng.below(text.len() + 1);
        text.truncate(cut);
    }
    text
}

fn rust_directory(text: &[u8]) -> String {
    let Ok(d) = directory::parse(text) else { return "invalid\n".into() };
    let mut o = String::new();
    let names: Vec<String> = d.channel_names().iter().map(|n| esc(n)).collect();
    writeln!(o, "names {}", names.join(",")).unwrap();
    for ch in d.channels() {
        let latest = ch.latest().map(|v| v.number.as_str()).unwrap_or("");
        writeln!(
            o,
            "channel {} {} {} versions {} latest {}",
            esc(&ch.id),
            esc(&ch.title),
            esc(&ch.description),
            ch.versions.len(),
            esc(latest)
        )
        .unwrap();
        for v in &ch.versions {
            writeln!(o, "version {} {} files {}", esc(&v.number), esc(&v.changelog), v.files.len()).unwrap();
            for f in &v.files {
                writeln!(o, "file {} {} {} {}", esc(&f.kind), esc(&f.target), esc(&f.url), f.sha256).unwrap();
            }
        }
    }
    o.push_str("missing -\n");
    o
}

fn band_value(rng: &mut Rng, ok: &str) -> String {
    if rng.below(100) < 94 {
        return ok.to_string();
    }
    rng.pick(&[
        "0",
        "-1",
        "4294967295",
        "4294967296",
        "127",
        "128",
        "-128",
        "-129",
        "100",
        "101",
        "1.5",
        "\"5\"",
        "null",
        "1000.0",
        "2e9",
        "1e10",
        "-0",
        "true",
    ])
    .to_string()
}

fn gen_bundle(rng: &mut Rng) -> Vec<u8> {
    const NAMES: &[&str] = &["A", "B", "A", "B", "C", "", "\u{e9}"];
    if rng.chance(12) {
        let mut p = Vec::new();
        let v = rng.pick(&["3", "-1", "1.5", "99999999999"]).to_string();
        field(rng, &mut p, "code", v);
        let v = s(rng.pick(&["Unknown region", "", "caf\u{e9}"]));
        field(rng, &mut p, "text", v);
        return format!("{{\"error\":{{{}}}{}}}", p.join(","), if rng.chance(2) { ",\"success\":1" } else { "" }).into_bytes();
    }
    let mut bands = Vec::new();
    let count = if rng.chance(20) { 0 } else { 2 + rng.below(3) };
    for i in 0..count {
        let name = if i < 2 { ["A", "B"][i] } else { rng.pick(NAMES) };
        let mut p = Vec::new();
        let v = band_value(rng, "433050000");
        field(rng, &mut p, "start", v);
        let v = band_value(rng, "434790000");
        field(rng, &mut p, "end", v);
        let v = band_value(rng, "10");
        field(rng, &mut p, "max_power", v);
        let v = band_value(rng, "50");
        field(rng, &mut p, "duty_cycle", v);
        bands.push(format!("\"{name}\":{{{}}}", p.join(",")));
    }
    let list = |rng: &mut Rng| -> String {
        let n = if rng.chance(15) { 0 } else { 1 + rng.below(3) };
        let items: Vec<String> = (0..n)
            .map(|_| {
                if rng.chance(40) {
                    "7".to_string()
                } else {
                    if rng.chance(12) {
                        s(rng.pick(&["C", "Z", "", "\u{e9}"]))
                    } else {
                        s(rng.pick(&["A", "B"]))
                    }
                }
            })
            .collect();
        format!("[{}]", items.join(","))
    };
    let mut countries = Vec::new();
    for _ in 0..1 + rng.below(3) {
        let c = rng.pick(&["DE", "US", "JP", "", "de", "XX"]);
        let l = if rng.chance(20) { "\"A\"".to_string() } else { list(rng) };
        countries.push(format!("\"{c}\":{l}"));
    }
    let mut p = Vec::new();
    let v = format!("{{{}}}", bands.join(","));
    field(rng, &mut p, "bands", v);
    let v = format!("{{{}}}", countries.join(","));
    field(rng, &mut p, "countries", v);
    let v = rng.pick(&["null", "\"DE\"", "\"US\"", "\"\"", "\"XX\"", "\"JP\""]).to_string();
    field(rng, &mut p, "country", v);
    let v = list(rng);
    field(rng, &mut p, "default", v);
    let mut text = format!("{{\"success\":{{{}}}}}", p.join(",")).into_bytes();
    if rng.chance(30) {
        let cut = rng.below(text.len() + 1);
        text.truncate(cut);
    }
    text
}

fn rust_region(fallback: &[u8], text: &[u8]) -> String {
    match region::parse(text) {
        Err(_) => "invalid\n".into(),
        Ok(region::Bundle::ServerError { code, text }) => format!("error {code} {}\n", esc(&text)),
        Ok(region::Bundle::Success(info)) => {
            let country = info.country_or(fallback);
            let keys: Vec<String> = info.band_names(&country).iter().map(hex::encode).collect();
            let data = region::encode(&country, &info.bands_for(&country));
            format!(
                "ok {} {} {}\n",
                if country.is_empty() { "-".to_string() } else { hex::encode(&country) },
                if keys.is_empty() { "-".to_string() } else { keys.join(",") },
                hex::encode(data)
            )
        }
    }
}

const CASES: usize = 3000;
const BATCH: usize = 250;

fn compare(kind: &str, extra: &[&str], inputs: &[Vec<u8>], rust: impl Fn(&[u8]) -> String) -> (usize, usize) {
    let oracle = oracle().expect("checked");
    let dir = tempfile::tempdir().unwrap();
    let mut valid = 0;
    let mut differ = 0;
    for (b, batch) in inputs.chunks(BATCH).enumerate() {
        let paths: Vec<PathBuf> = batch
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let p = dir.path().join(format!("{kind}-{b}-{i}"));
                std::fs::write(&p, t).unwrap();
                p
            })
            .collect();
        let out = Command::new(&oracle).arg(kind).args(extra).args(&paths).output().unwrap();
        let counting = std::env::var_os("ARGUS_UPDATES_COUNT").is_some();
        assert!(counting || out.status.success(), "oracle failed: {}", String::from_utf8_lossy(&out.stderr));
        let cpp = String::from_utf8_lossy(&out.stdout).into_owned();
        let rs: String = batch.iter().map(|t| rust(t)).collect();
        if cpp != rs {
            // find the inputs that differ: the first fails the test, or (ARGUS_UPDATES_COUNT set, for an oracle
            // built from older C++ code) all are counted
            for (t, p) in batch.iter().zip(&paths) {
                let one = Command::new(&oracle).arg(kind).args(extra).arg(p).output().unwrap();
                let c = String::from_utf8_lossy(&one.stdout).into_owned();
                let r = rust(t);
                if std::env::var_os("ARGUS_UPDATES_COUNT").is_some() {
                    differ += usize::from(c != r || !one.status.success());
                } else {
                    assert_eq!(c, r, "differs on {}:\n{}", p.display(), String::from_utf8_lossy(t));
                }
            }
        }
        valid += rs.lines().filter(|l| !l.starts_with("invalid")).count();
    }
    if differ > 0 {
        eprintln!("{kind} {extra:?}: {differ} of {} inputs differ", inputs.len());
    }
    (inputs.len(), valid)
}

fn seed() -> u64 {
    std::env::var("ARGUS_UPDATES_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x5eed_0007)
}

#[test]
fn directory_matches_the_cpp_app() {
    if oracle().is_none() {
        return;
    }
    let mut rng = Rng(seed());
    let inputs: Vec<Vec<u8>> = (0..CASES).map(|_| gen_directory(&mut rng)).collect();
    let (n, lines) = compare("dir", &[], &inputs, rust_directory);
    let valid = inputs.iter().filter(|t| directory::parse(t).is_ok()).count();
    eprintln!("{n} directories ({valid} valid, {lines} lines) read the same in both");
    assert!(valid > n / 10 && valid < n * 9 / 10, "the generator should make both kinds: {valid} of {n} valid");
}

#[test]
fn region_matches_the_cpp_app() {
    if oracle().is_none() {
        return;
    }
    let mut rng = Rng(seed() ^ 0xb0b);
    let inputs: Vec<Vec<u8>> = (0..CASES).map(|_| gen_bundle(&mut rng)).collect();
    for fallback in ["-", "DE", "XX"] {
        let fb: &[u8] = if fallback == "-" { b"" } else { fallback.as_bytes() };
        compare("region", &[fallback], &inputs, |t| rust_region(fb, t));
    }
    let ok = inputs.iter().filter(|t| matches!(region::parse(t), Ok(region::Bundle::Success(_)))).count();
    eprintln!("{} bundles ({ok} valid) give the same region data in both", inputs.len());
    assert!(ok > CASES / 20, "the generator should make valid bundles: {ok}");
}

/// RemoteFileFetcher's check (through its test hook, a local folder) against `fetch::Client::get_file`'s rules:
/// the same files are taken and refused for their SHA-256 and size.
#[test]
fn fetch_checks_match_the_cpp_app() {
    let Some(oracle) = oracle() else { return };
    let dir = tempfile::tempdir().unwrap();
    let data = b"made-up firmware file\n".repeat(100);
    std::fs::write(dir.path().join("f.bin"), &data).unwrap();
    let good = {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(&data))
    };
    let cases = [
        (good.clone(), data.len(), "ok"),
        (good.to_ascii_uppercase(), data.len(), "ok"),
        (good.clone(), data.len() - 1, "failed"),
        (format!("{}0", &good[..63]), data.len(), "failed"),
        (String::new(), data.len(), "invalid"),
        (good[..63].to_string(), data.len(), "invalid"),
    ];
    for (sha, max, want) in cases {
        let out = Command::new(&oracle)
            .env("ARGUS_TEST_FIRMWARE_DIR", dir.path())
            .args(["fetch", "f.bin", &sha, &max.to_string()])
            .output()
            .unwrap();
        let got = String::from_utf8(out.stdout).unwrap();
        let rust = match directory::checked_sha256(&sha) {
            None => "invalid".to_string(),
            Some(expected) => {
                use sha2::Digest;
                let actual = hex::encode(sha2::Sha256::digest(&data));
                if data.len() > max || actual != expected {
                    "failed".into()
                } else {
                    format!("ok {}", data.len())
                }
            }
        };
        assert_eq!(got.trim(), rust, "{sha} {max}");
        assert!(rust.starts_with(want), "{sha} {max}: {rust}");
    }
}
