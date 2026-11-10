#![no_main]
use argus_tools::api::{self, FirmwareApi};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let fw = FirmwareApi { major: 88, minor: 2, target: 7 };
    if let Ok(info) = argus_tools::fap::inspect(data) {
        let _ = api::check(&info, &fw, api::builtin_table(&fw).as_ref());
    }
});
