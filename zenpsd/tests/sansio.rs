//! Incremental (sans-I/O) decode through `dyn_incremental_decoder`: the
//! testkit's incremental contract on synthetic PSD and PSB fixtures.
#![cfg(feature = "zencodec")]

use zencodec::decode::DecoderConfig as _;
use zenpsd::PsdDecoderConfig;

fn minimal_psd_rgb8() -> Vec<u8> {
    let mut out = Vec::new();
    // Header: 8BPS, v1, 6 reserved, channels=3, height=2, width=2, depth=8, mode=3 (RGB)
    out.extend_from_slice(b"8BPS");
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&[0u8; 6]);
    out.extend_from_slice(&3u16.to_be_bytes());
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&8u16.to_be_bytes());
    out.extend_from_slice(&3u16.to_be_bytes());
    // Color Mode Data length = 0
    out.extend_from_slice(&0u32.to_be_bytes());
    // Image Resources length = 0
    out.extend_from_slice(&0u32.to_be_bytes());
    // Layer and Mask Information length = 0
    out.extend_from_slice(&0u32.to_be_bytes());
    // Image Data: compression = 0 (Raw), 3 channels * 2 * 2 = 12 bytes
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&[
        255, 128, 64, 0, // R
        0, 64, 128, 255, // G
        10, 20, 30, 40, // B
    ]);
    out
}

#[test]
fn incremental_contract_holds() {
    assert!(PsdDecoderConfig::capabilities().incremental());
    let psd = minimal_psd_rgb8();
    zencodec_testkit::check_incremental_capability_honesty(PsdDecoderConfig::new(), &psd)
        .unwrap_or_else(|e| panic!("psd: {e:?}"));
}
