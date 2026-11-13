//! argus-tools CLI. Every command prints one JSON object on stdout: `{"ok":true,...}` or
//! `{"ok":false,"error":{"code","message"}}` (exit status 1). Progress goes to stderr as JSON
//! lines `{"progress":"..."}`. Usage: `argus-tools help`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use argus_tools::api::{self, FirmwareApi};
use argus_tools::capture::{self, Builder, Library};
use argus_tools::limbo;
use argus_tools::package::{self, Limits};
use argus_tools::snapshot::{self, Listing, Snapshots};
use argus_tools::{fap, Error, Result};

/// The Argus app's own version (the VERSION file at the top of the repository, read when this is built), as the app reports it.
const APP_VERSION: &str = include_str!("../../VERSION").trim_ascii();

const HELP: &str = "argus-tools — Argus_App's logic (JSON on stdout)

  fap inspect FILE [--api 88.2[/7]]
  package scan PACKAGE --out DIR [--api 88.2/7] [--max-total BYTES]
  checkup --dir MIRROR [--api 88.2/7] [--package PKG --out DIR]...
  snapshot list|plan|create|diff|restore-plan|delete|gc|verify --store DIR ...
      plan --listing L.json [--base ID]
      create --listing L.json --staging DIR [--base ID] [--label TEXT] [--device JSON]
      diff --from ID (--to ID | --listing L.json)
      restore-plan --id ID [--path /ext/...] [--listing L.json]
      delete --id ID ; gc ; verify [--id ID]
  capture index --library DIR [--snapshots STORE] [--tree DIR]...
  capture search|dupes|show|explain|tag --library DIR [--query Q] [--limit N] [--id ID] [--add T] [--remove T]
  limbo profiles [--socket PATH]
  limbo build (--source DIR | --git URL [--ref R] [--subdir S]) --out DIR [--api 88.2/7]
              [--socket PATH] [--profile NAME] [--fetch-profile NAME] [--timeout SECONDS]
  version | help";

struct Args {
    pos: Vec<String>,
    flags: BTreeMap<String, Vec<String>>,
}

impl Args {
    fn parse(raw: Vec<String>) -> Result<Args> {
        let mut pos = Vec::new();
        let mut flags: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut it = raw.into_iter();
        while let Some(a) = it.next() {
            if let Some(name) = a.strip_prefix("--") {
                let v = it.next().ok_or_else(|| Error::new("usage", format!("--{name} needs a value")))?;
                flags.entry(name.to_string()).or_default().push(v);
            } else {
                pos.push(a);
            }
        }
        Ok(Args { pos, flags })
    }
    fn get(&self, k: &str) -> Option<&str> {
        self.flags.get(k).and_then(|v| v.last()).map(String::as_str)
    }
    fn all(&self, k: &str) -> Vec<String> {
        self.flags.get(k).cloned().unwrap_or_default()
    }
    fn need(&self, k: &str) -> Result<&str> {
        self.get(k).ok_or_else(|| Error::new("usage", format!("--{k} is required")))
    }
    fn path(&self, k: &str) -> Result<PathBuf> {
        Ok(PathBuf::from(self.need(k)?))
    }
    fn fw(&self) -> Result<FirmwareApi> {
        let s = self.get("api").unwrap_or("88.2/7");
        FirmwareApi::parse(s).ok_or_else(|| Error::new("usage", "--api is MAJOR.MINOR or MAJOR.MINOR/TARGET, e.g. 88.2/7"))
    }
    fn usize(&self, k: &str, d: usize) -> Result<usize> {
        self.get(k).map(|v| v.parse().map_err(|_| Error::new("usage", format!("--{k} is a number")))).unwrap_or(Ok(d))
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn progress(msg: &str) {
    eprintln!("{}", json!({ "progress": msg }));
}

fn read_listing(p: &Path) -> Result<Listing> {
    let raw = std::fs::read(p)?;
    serde_json::from_slice(&raw).map_err(|e| Error::new("bad_listing", e.to_string()))
}

fn ok(mut v: Value) -> Value {
    v["ok"] = json!(true);
    v
}

fn run(raw: Vec<String>) -> Result<Value> {
    let a = Args::parse(raw)?;
    let p: Vec<&str> = a.pos.iter().map(String::as_str).collect();
    match p.as_slice() {
        ["version"] => Ok(ok(json!({"version": APP_VERSION}))),
        [] | ["help"] => Ok(ok(json!({"help": HELP}))),
        ["fap", "inspect", file] => {
            let data = std::fs::read(file)?;
            let info = fap::inspect(&data)?;
            let fw = a.fw()?;
            let table = api::builtin_table(&fw);
            let compat = api::check(&info, &fw, table.as_ref());
            Ok(ok(json!({"sha256": argus_tools::sha256_hex(&data), "size": data.len(), "manifest": info.manifest,
                         "imports": info.imports, "compat": compat})))
        }
        ["package", "scan", pkg] => {
            let mut limits = Limits::default();
            if let Some(m) = a.get("max-total") {
                limits.total_bytes = m.parse().map_err(|_| Error::new("usage", "--max-total is a number"))?;
            }
            let fw = if a.get("api") == Some("none") { None } else { Some(a.fw()?) };
            let r = package::scan(Path::new(pkg), &a.path("out")?, limits, fw)?;
            Ok(ok(serde_json::to_value(r)?))
        }
        ["checkup"] => checkup(&a),
        ["snapshot", sub] => snapshot_cmd(sub, &a),
        ["capture", sub] => capture_cmd(sub, &a),
        ["limbo", "profiles"] => {
            let c = limbo::Client::new(Path::new(a.get("socket").unwrap_or(limbo::DEFAULT_SOCKET)));
            Ok(ok(json!({"limbo": c.profiles()?})))
        }
        ["limbo", "build"] => {
            let out = a.path("out")?;
            let socket = PathBuf::from(a.get("socket").unwrap_or(limbo::DEFAULT_SOCKET));
            let o = limbo::BuildOptions {
                socket: &socket,
                build_profile: a.get("profile").unwrap_or(limbo::BUILD_PROFILE),
                fetch_profile: a.get("fetch-profile").unwrap_or(limbo::FETCH_PROFILE),
                fw: a.fw()?,
                out_dir: &out,
                timeout: a.usize("timeout", 900)? as u32,
                poll: Duration::from_millis(a.usize("poll-ms", 2000)? as u64),
            };
            let src = match (a.get("source"), a.get("git")) {
                (Some(dir), None) => limbo::Source::Folder(Path::new(dir)),
                (None, Some(url)) => limbo::Source::Git { url, git_ref: a.get("ref"), subdir: a.get("subdir").unwrap_or("") },
                _ => return Err(Error::new("usage", "give exactly one of --source DIR or --git URL")),
            };
            let r = limbo::build(src, &o, &mut |m| progress(m))?;
            Ok(ok(serde_json::to_value(r)?))
        }
        _ => Err(Error::new("usage", format!("unknown command; {HELP}"))),
    }
}

fn checkup(a: &Args) -> Result<Value> {
    let packages = a.all("package");
    let out = if packages.is_empty() { None } else { Some(a.path("out")?) };
    Ok(ok(argus_tools::checkup::run(&a.path("dir")?, a.fw()?, &packages, out.as_deref(), &mut |m| progress(m))?))
}

fn snapshot_cmd(sub: &str, a: &Args) -> Result<Value> {
    let snaps = Snapshots::open(&a.path("store")?)?;
    match sub {
        "list" => {
            let list: Vec<_> = snaps.list()?.iter().map(snapshot::summary).collect();
            Ok(ok(json!({"snapshots": list})))
        }
        "plan" => Ok(ok(serde_json::to_value(snapshot::plan(&snaps, &read_listing(&a.path("listing")?)?, a.get("base"))?)?)),
        "create" => {
            let listing = read_listing(&a.path("listing")?)?;
            let device: Value = a.get("device").map(serde_json::from_str).transpose()?.unwrap_or(Value::Null);
            let staging = a.get("staging").map(PathBuf::from);
            let s = snapshot::create(&snaps, &listing, staging.as_deref(), a.get("base"), a.get("label").unwrap_or(""), device, now())?;
            Ok(ok(json!({"snapshot": snapshot::summary(&s), "skipped": s.skipped})))
        }
        "show" => Ok(ok(json!({"snapshot": snaps.load(a.need("id")?)?}))),
        "diff" => {
            let from = snaps.load(a.need("from")?)?;
            let d = match (a.get("to"), a.get("listing")) {
                (Some(to), None) => snapshot::diff_snapshots(&from, &snaps.load(to)?),
                (None, Some(l)) => snapshot::diff_listing(&from, &read_listing(Path::new(l))?),
                _ => return Err(Error::new("usage", "give --to ID or --listing FILE")),
            };
            Ok(ok(serde_json::to_value(d)?))
        }
        "restore-plan" => {
            let s = snaps.load(a.need("id")?)?;
            let dev = a.get("listing").map(|l| read_listing(Path::new(l))).transpose()?;
            Ok(ok(serde_json::to_value(snapshot::restore_plan(&snaps, &s, a.get("path"), dev.as_ref())?)?))
        }
        "delete" => {
            snaps.delete(a.need("id")?)?;
            let (n, bytes) = snaps.gc()?;
            Ok(ok(json!({"removed_objects": n, "freed_bytes": bytes})))
        }
        "gc" => {
            let (n, bytes) = snaps.gc()?;
            Ok(ok(json!({"removed_objects": n, "freed_bytes": bytes})))
        }
        "verify" => {
            let list = match a.get("id") {
                Some(id) => vec![snaps.load(id)?],
                None => snaps.list()?,
            };
            let mut bad = Vec::new();
            let mut checked = std::collections::BTreeSet::new();
            for s in &list {
                for f in &s.files {
                    if checked.insert(f.sha256.clone()) && !snaps.store.verify(&f.sha256) {
                        bad.push(json!({"snapshot": s.id, "path": f.path, "sha256": f.sha256}));
                    }
                }
            }
            Ok(ok(json!({"objects_checked": checked.len(), "bad": bad})))
        }
        _ => Err(Error::new("usage", "snapshot list|plan|create|show|diff|restore-plan|delete|gc|verify")),
    }
}

fn capture_cmd(sub: &str, a: &Args) -> Result<Value> {
    let lib = Library::open(&a.path("library")?)?;
    match sub {
        "index" => {
            let mut b = Builder::default();
            if let Some(store) = a.get("snapshots") {
                if Path::new(store).join("snapshots").is_dir() {
                    b.add_snapshots(&Snapshots::open(Path::new(store))?)?;
                }
            }
            for t in a.all("tree") {
                if Path::new(&t).is_dir() {
                    b.add_tree(Path::new(&t), "pull")?;
                }
            }
            let idx = b.finish(now());
            lib.save_index(&idx)?;
            let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
            for c in &idx.captures {
                *kinds.entry(c.kind.clone()).or_default() += 1;
            }
            Ok(ok(json!({"captures": idx.captures.len(), "kinds": kinds, "skipped": idx.skipped.len()})))
        }
        "search" => {
            let idx = lib.load()?;
            let hits = capture::search(&idx, a.get("query").unwrap_or(""), a.usize("limit", 200)?);
            Ok(ok(json!({"total": idx.captures.len(), "captures": hits})))
        }
        "show" => {
            let idx = lib.load()?;
            Ok(ok(json!({"capture": lib.find(&idx, a.need("id")?)?})))
        }
        "explain" => {
            let idx = lib.load()?;
            let c = lib.find(&idx, a.need("id")?)?;
            Ok(ok(json!({"capture": c, "explanation": capture::explain(c)})))
        }
        "dupes" => {
            let idx = lib.load()?;
            Ok(ok(json!({"groups": capture::duplicates(&idx)})))
        }
        "tag" => {
            let idx = lib.load()?;
            let sha = lib.find(&idx, a.need("id")?)?.sha256.clone();
            let tags = lib.tag(&sha, &a.all("add"), &a.all("remove"))?;
            Ok(ok(json!({"sha256": sha, "tags": tags})))
        }
        _ => Err(Error::new("usage", "capture index|search|show|explain|dupes|tag")),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match std::panic::catch_unwind(|| run(args)) {
        Ok(Ok(v)) => {
            println!("{v}");
            0
        }
        Ok(Err(e)) => {
            println!("{}", json!({"ok": false, "error": {"code": e.code, "message": e.message}}));
            1
        }
        Err(_) => {
            println!("{}", json!({"ok": false, "error": {"code": "internal", "message": "argus-tools hit an internal error"}}));
            2
        }
    };
    std::process::exit(code);
}
