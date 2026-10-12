//! Structural inventory fuzzer: `ExrDecoderConfig::inventory` must never
//! panic, and every inventory it returns must cover the input exactly and
//! validate.
#![no_main]

use enough::Unstoppable;
use libfuzzer_sys::fuzz_target;
use zenexr::ExrDecoderConfig;

fuzz_target!(|data: &[u8]| {
    match ExrDecoderConfig::new().inventory(data, &Unstoppable) {
        Ok(inv) => {
            assert_eq!(inv.input_len(), data.len() as u64);
            if let Err(e) = inv.validate() {
                panic!("invalid inventory: {e}\n{inv}");
            }
        }
        // Only the part cap may fail, and it needs far more than a fuzz input.
        Err(e) => panic!("inventory failed: {e}"),
    }
});
