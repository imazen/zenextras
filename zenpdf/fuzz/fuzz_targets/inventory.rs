#![no_main]

use libfuzzer_sys::fuzz_target;
use zencodec::decode::{DecodeJob, DecoderConfig};

// The structural inventory never panics and always covers the input
// exactly; only the part cap may turn it into an error. Odd-length inputs
// decode the second page, so the first is one the job does not decode.
fuzz_target!(|data: &[u8]| {
    let job = zenpdf::PdfDecoderConfig::new()
        .job()
        .with_start_frame_index((data.len() % 2) as u32);
    if let Ok(inv) = job.inventory(data) {
        let inv = inv.expect("zenpdf implements inventory");
        assert_eq!(inv.input_len(), data.len() as u64);
        if let Err(e) = inv.validate() {
            panic!("invalid inventory: {e}\n{inv}");
        }
    }
});
