#![no_main]
// The DfuSe file parser and the DFU memory layout (crates/flipper-dfu): a recovery file or a USB
// interface string as anything pretending to be one might hand them over.
use flipper_dfu::{dfuse, layout::MemoryLayout};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(f) = dfuse::parse(data) {
        let total: usize = f.targets.iter().flat_map(|t| &t.elements).map(|e| e.data.len()).sum();
        assert!(total < data.len());
    }
    if let Ok(l) = MemoryLayout::parse(data) {
        let start = l.address;
        let _ = l.page_addresses(start, (u64::from(start) + 0x10000).min(l.end()));
    }
});
