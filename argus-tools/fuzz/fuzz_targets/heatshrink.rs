#![no_main]
use std::io::Read;

use argus_tools::heatshrink::{Decoder, Params};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let p = Params { window_bits: 4 + data[0] % 12, lookahead_bits: 3 + data[1] % 12 };
    if !p.valid() {
        return;
    }
    let mut out = Vec::new();
    let _ = Decoder::new(&data[2..], p).take(1 << 22).read_to_end(&mut out);
});
