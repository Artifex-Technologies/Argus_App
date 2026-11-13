//! `argus-cli updates ...` and `argus-cli region FILE`: Flipper's update server, through `flipper_updates`
//! (HTTPS with the system's roots; `--update-server URL` names another server, as the tests' local fake).
//! No device is opened. Every download is bounded; a firmware file is written only once its SHA-256 matched
//! the directory, to a new file (0600) that never replaces one.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::ExitCode;

use serde_json::{json, Value};

use flipper_updates::fetch::{self, Client};
use flipper_updates::{directory, region};

use super::{fail, print, Cmd};

pub(crate) const USAGE: &str = "argus-cli [--update-server URL] updates directory | updates fetch CHANNEL TYPE TARGET FILE
argus-cli [--update-server URL] [--country CC] region FILE

  updates directory        the firmware channels, their latest versions and files
  updates fetch CHANNEL TYPE TARGET FILE
                           the latest version's file of that type and target (e.g. release
                           full_dfu f7), checked against the directory's SHA-256, to FILE
                           (which must not exist)
  region FILE              the region bundle made into the bytes the firmware reads from
                           /int/.region_data, written to FILE (which must not exist): the
                           bands for the country the server saw, else for --country, else the
                           default bands

  --update-server URL      default https://update.flipperzero.one (only https)
  --country CC             the country to use when the server names none (the C++ app takes
                           the system locale's territory there; argus-cli never guesses)";

pub(crate) fn server(cmd: &Cmd) -> &str {
    cmd.update_server.as_deref().unwrap_or(flipper_updates::DEFAULT_SERVER)
}

pub(crate) fn client() -> Result<Client, (String, String)> {
    Client::new().map_err(|e| (e.code().to_string(), e.to_string()))
}

fn write_new(path: &Path, data: &[u8]) -> Result<(), (String, String)> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut f| f.write_all(data))
        .map_err(|e| ("local".to_string(), format!("{}: {e}", path.display())))
}

pub(crate) fn get_directory(c: &Client, server: &str) -> Result<directory::Directory, (String, String)> {
    let text = c.get(&flipper_updates::directory_url(server), directory::MAX_DIRECTORY).map_err(|e| (e.code().into(), e.to_string()))?;
    directory::parse(&text).map_err(|e| ("directory".into(), e.to_string()))
}

/// The region data for this server and `--country`.
pub(crate) fn fetch_region_data(cmd: &Cmd) -> Result<region::RegionData, (String, String)> {
    let c = client()?;
    let text = c.get(&flipper_updates::region_url(server(cmd)), region::MAX_BUNDLE).map_err(|e| (e.code().into(), e.to_string()))?;
    let fallback = cmd.country.as_deref().unwrap_or("").as_bytes();
    region::region_data(&text, fallback).map_err(|e| ("region".into(), e.to_string()))
}

pub(crate) fn region_json(d: &region::RegionData) -> Value {
    json!({
        "country": String::from_utf8_lossy(&d.country),
        "bands": d.bands.iter().map(|b| json!({"start": b.start, "end": b.end, "power_limit": b.power_limit, "duty_cycle": b.duty_cycle})).collect::<Vec<_>>(),
        "bytes": d.bytes.len(),
    })
}

fn run_words(cmd: &Cmd, words: &[&str]) -> Result<Value, (String, String)> {
    match words {
        ["updates", "directory"] => {
            let d = get_directory(&client()?, server(cmd))?;
            let channels: Vec<Value> = d
                .channels()
                .iter()
                .map(|ch| {
                    let latest = ch.latest().map(|v| {
                        json!({
                            "version": v.number, "timestamp": v.timestamp,
                            "files": v.files.iter().map(|f| json!({"type": f.kind, "target": f.target, "url": f.url, "sha256": f.sha256})).collect::<Vec<_>>(),
                        })
                    });
                    json!({"id": ch.id, "title": ch.title, "versions": ch.versions.len(), "latest": latest})
                })
                .collect();
            Ok(json!({"ok": true, "command": "updates directory", "channels": channels}))
        }
        ["updates", "fetch", channel, kind, target, file] => {
            let path = Path::new(file);
            if path.exists() {
                return Err(("exists".into(), format!("{file} exists")));
            }
            let c = client()?;
            let d = get_directory(&c, server(cmd))?;
            let ch = d.channel(channel).ok_or_else(|| ("not_found".to_string(), format!("no channel {channel}")))?;
            let v = ch.latest().ok_or_else(|| ("not_found".to_string(), format!("channel {channel} has no versions")))?;
            let f = v
                .file(kind, target)
                .ok_or_else(|| ("not_found".to_string(), format!("{channel} {} has no {kind} file for {target}", v.number)))?;
            let data = c.get_file(f, fetch::MAX_FILE).map_err(|e| (e.code().into(), e.to_string()))?;
            write_new(path, &data)?;
            Ok(json!({
                "ok": true, "command": "updates fetch", "channel": channel, "version": v.number, "type": kind, "target": target,
                "file": file, "bytes": data.len(), "sha256": f.sha256,
            }))
        }
        ["region", file] => {
            let path = Path::new(file);
            if path.exists() {
                return Err(("exists".into(), format!("{file} exists")));
            }
            let d = fetch_region_data(cmd)?;
            write_new(path, &d.bytes)?;
            Ok(json!({"ok": true, "command": "region", "file": file, "region_data": region_json(&d)}))
        }
        _ => Err(("usage".into(), format!("unknown command: {}\n\n{USAGE}", words.join(" ")))),
    }
}

pub fn run(cmd: &Cmd) -> ExitCode {
    let words: Vec<&str> = cmd.words.iter().map(String::as_str).collect();
    if matches!(words.as_slice(), ["updates"] | ["updates", "help"] | ["region"]) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match run_words(cmd, &words) {
        Ok(v) => {
            print(v);
            ExitCode::SUCCESS
        }
        Err((code, m)) => {
            let status = if code == "usage" { 2 } else { 1 };
            fail(&code, m, status)
        }
    }
}
