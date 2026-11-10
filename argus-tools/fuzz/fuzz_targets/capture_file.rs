#![no_main]
use argus_tools::capture::{self, Builder, Location};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut b = Builder::default();
    b.add(data, Location { origin: "fuzz".into(), source: "fuzz".into(), path: "/ext/nfc/x.nfc".into() });
    for c in b.finish(0).captures {
        let _ = capture::explain(&c);
    }
});
