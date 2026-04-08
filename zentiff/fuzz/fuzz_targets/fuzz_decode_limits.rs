//! Limits fuzzer: verify resource limits are enforced under adversarial input.
//!
//! Decodes with strict resource limits — should never exceed them, OOM, or panic.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let config = zentiff::TiffDecodeConfig::default()
        .with_max_pixels(4_000_000)
        .with_max_memory(64 * 1024 * 1024) // 64 MB
        .with_max_width(4096)
        .with_max_height(4096);
    let _ = zentiff::decode(data, &config, &enough::Unstoppable);
});
