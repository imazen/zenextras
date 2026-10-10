#![no_main]

use libfuzzer_sys::fuzz_target;
use zencodec::decode::{DecodeJob, DecoderConfig};
use zenjp2::Jp2DecoderConfig;

// The structural walk must never panic and must always return an inventory
// that tiles the input (`Inventory::validate`), whatever the bytes.
fuzz_target!(|data: &[u8]| {
    let inv = Jp2DecoderConfig::new()
        .job()
        .inventory(data)
        .expect("inventory fails only at the part cap")
        .expect("zenjp2 implements inventory");
    inv.validate().expect("inventory tiles the input");
    assert_eq!(inv.input_len(), data.len() as u64);
});
