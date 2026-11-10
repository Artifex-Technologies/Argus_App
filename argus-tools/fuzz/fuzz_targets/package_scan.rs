#![no_main]
use argus_tools::api::FirmwareApi;
use argus_tools::package::{self, Limits};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let root = tempfile::tempdir().unwrap();
    let pkg = root.path().join("p");
    std::fs::write(&pkg, data).unwrap();
    let limits = Limits { total_bytes: 4 << 20, nested_bytes: 2 << 20, app_bytes: 1 << 20, entries: 2000, depth: 4, apps: 200 };
    let _ = package::scan(&pkg, &root.path().join("out"), limits, Some(FirmwareApi { major: 88, minor: 2, target: 7 }));
});
