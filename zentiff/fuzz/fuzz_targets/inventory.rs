//! Structural inventory fuzzer: `DecodeJob::inventory` must never panic, and
//! every inventory it returns must cover the input exactly and validate.
#![no_main]

use libfuzzer_sys::fuzz_target;
use zencodec::decode::{DecodeJob, DecoderConfig};
use zentiff::codec::TiffDecoderCodecConfig;

fuzz_target!(|data: &[u8]| {
    let job = TiffDecoderCodecConfig::new().job();
    match job.inventory(data) {
        Ok(Some(inv)) => {
            assert_eq!(inv.input_len(), data.len() as u64);
            if let Err(e) = inv.validate() {
                panic!("invalid inventory: {e}\n{inv}");
            }
        }
        Ok(None) => panic!("zentiff declares the inventory capability"),
        // Only the part cap may fail, and it needs far more than a fuzz input.
        Err(e) => panic!("inventory failed: {e}"),
    }
});
