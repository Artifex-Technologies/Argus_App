//! The check-up of the apps on the card: every `.fap` in a local mirror of `/ext/apps`, judged against the firmware
//! API, and for each one that will not load a compatible copy from the given packages. `argus-tools checkup` and the
//! Rust app's tools window both run this.

use std::path::Path;

use serde_json::{json, Value};

use crate::api::{self, FirmwareApi};
use crate::package::{self, Limits};
use crate::{fap, paths, Error, Result};

/// Read every .fap in a local mirror of the card's /ext/apps (`<dir>/ext/apps/...`), judge it
/// against the firmware API, and for each one that will not load, look for a compatible copy in
/// the given packages.
pub fn run(dir: &Path, fw: FirmwareApi, packages: &[String], out: Option<&Path>, progress: &mut dyn FnMut(&str)) -> Result<Value> {
    let table = api::builtin_table(&fw);
    let apps_root = dir.join("ext").join("apps");
    let mut found = Vec::new();
    let mut stack = vec![(apps_root.clone(), 0)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() && depth < 8 {
                stack.push((e.path(), depth + 1));
            } else if ft.is_file() && e.file_name().to_string_lossy().to_ascii_lowercase().ends_with(".fap") {
                found.push(e.path());
            }
        }
    }
    found.sort();
    // candidates from packages: (file_name lowercase, manifest name) -> entries
    let mut candidates: Vec<package::AppEntry> = Vec::new();
    let mut packages_read = Vec::new();
    if !packages.is_empty() {
        let out = out.ok_or_else(|| Error::new("usage", "--out is required with --package"))?;
        for pkg in packages {
            progress(&format!("reading package {}", crate::display_text(pkg.as_bytes(), 200)));
            let r = package::scan(Path::new(pkg), out, Limits::default(), Some(fw))?;
            packages_read.push(json!({"package": r.package, "apps": r.apps.len(), "incomplete": r.incomplete, "warnings": r.warnings}));
            candidates.extend(r.apps);
        }
    }
    let mut apps = Vec::new();
    let (mut bad, mut unknown) = (0, 0);
    for f in found {
        let rel = f.strip_prefix(dir).unwrap_or(&f).to_string_lossy().replace('\\', "/");
        let card = format!("/{rel}");
        if !paths::valid_device_path(&card) {
            continue;
        }
        let data = match std::fs::read(&f) {
            Ok(d) if d.len() <= 8 << 20 => d,
            _ => continue,
        };
        let file_name = card.rsplit('/').next().unwrap_or("").to_string();
        let category = card.strip_prefix("/ext/apps/").and_then(|r| r.rsplit_once('/')).map(|(c, _)| c.to_string()).unwrap_or_default();
        let (manifest, compat) = match fap::inspect(&data) {
            Ok(info) => (Some(info.manifest.clone()), api::check(&info, &fw, table.as_ref())),
            Err(e) => (None, api::not_a_fap(&fw, &e.message)),
        };
        let mut replacement = Value::Null;
        if compat.status != api::Status::Compatible {
            if compat.status == api::Status::Incompatible {
                bad += 1;
            } else {
                unknown += 1;
            }
            let mname = manifest.as_ref().map(|m| m.name.clone()).unwrap_or_default();
            let best = candidates.iter().find(|c| {
                c.compat.as_ref().is_some_and(|x| x.status == api::Status::Compatible)
                    && (c.file_name.eq_ignore_ascii_case(&file_name)
                        || (!mname.is_empty() && c.manifest.as_ref().is_some_and(|m| m.name == mname)))
            });
            if let Some(c) = best {
                replacement = serde_json::to_value(c)?;
            }
        }
        apps.push(json!({
            "path": card, "category": category, "file_name": file_name, "size": data.len(),
            "sha256": crate::sha256_hex(&data), "manifest": manifest, "compat": compat, "replacement": replacement,
        }));
    }
    Ok(json!({"firmware_api": fw.text(), "symbols_table": table.is_some(), "apps": apps,
              "incompatible": bad, "unknown": unknown, "packages": packages_read}))
}
