#![no_main]

use libfuzzer_sys::fuzz_target;
use zencodec::decode::{DecodeJob, DecoderConfig};

// The structural inventory never panics and always covers the input
// exactly; only the part cap may turn it into an error.
fuzz_target!(|data: &[u8]| {
    let job = zensvg::SvgDecoderConfig::new().job();
    if let Ok(inv) = job.inventory(data) {
        let inv = inv.expect("zensvg implements inventory");
        assert_eq!(inv.input_len(), data.len() as u64);
        if let Err(e) = inv.validate() {
            panic!("invalid inventory: {e}\n{inv}");
        }
    }
});
