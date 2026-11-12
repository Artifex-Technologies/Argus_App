#![no_main]
// The RPC frame decoder (crates/flipper-rpc): bytes as a Flipper (or anything pretending to be
// one) might send them, pushed in two pieces split at the first byte's value.
use flipper_rpc::frame::FrameDecoder;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let cut = data.first().map_or(0, |b| *b as usize).min(data.len());
    let mut d = FrameDecoder::new();
    for part in [&data[..cut], &data[cut..]] {
        if d.push(part).is_err() {
            return;
        }
        loop {
            match d.next_frame() {
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => return,
            }
        }
    }
});
