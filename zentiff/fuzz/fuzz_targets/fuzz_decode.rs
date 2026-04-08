//! Primary TIFF decode fuzzer — exercises the full decode pipeline.
//!
//! Any crash, panic, or OOM on arbitrary input is a bug.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let config = zentiff::TiffDecodeConfig::default();
    let _ = zentiff::decode(data, &config, &enough::Unstoppable);
});
