#![cfg(feature = "zencodec")]
use std::borrow::Cow;
use zencodec::decode::{Decode, DecodeJob, DecodePolicy, DecoderConfig};
use zenpsd::PsdDecoderConfig;

fn sample_psd_with_icc_and_exif() -> Vec<u8> {
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
    // Image Resources: ICC (0x040F) + EXIF (0x0422) + XMP (0x0424)
    let mut resources = Vec::new();
    for (id, payload) in [
        (0x040Fu16, b"fake-icc-profile".as_slice()),
        (0x0422u16, b"II*\x00\x08\x00\x00\x00\x00\x00".as_slice()),
        (0x0424u16, b"<x:xmpmeta/>".as_slice()),
    ] {
        resources.extend_from_slice(b"8BIM");
        resources.extend_from_slice(&id.to_be_bytes());
        resources.extend_from_slice(&[0, 0]); // empty Pascal name (1 zero length + 1 pad)
        resources.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        resources.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            resources.push(0);
        }
    }
    out.extend_from_slice(&(resources.len() as u32).to_be_bytes());
    out.extend_from_slice(&resources);
    // Layer and Mask Information length = 0
    out.extend_from_slice(&0u32.to_be_bytes());
    // Image Data: compression = 0 (Raw), 3 * 2 * 2 = 12 bytes
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&[100u8; 12]);
    out
}

#[test]
fn native_fixture_policy_and_inventory_contracts() {
    let bytes = sample_psd_with_icc_and_exif();
    let dec = PsdDecoderConfig::new();
    let expected = dec
        .clone()
        .job()
        .decoder(Cow::Borrowed(bytes.as_slice()), &[])
        .unwrap()
        .decode()
        .unwrap()
        .metadata();
    for (policy, accepted) in [
        (DecodePolicy::none(), true),
        (DecodePolicy::strict(), false),
    ] {
        zencodec_testkit::check_decode_policy_admission(
            dec.clone(),
            &bytes,
            DecodePolicy::none(),
            policy,
            accepted,
            &expected,
        )
        .unwrap();
    }
    zencodec_testkit::check_inventory_dispatch(dec.clone(), &bytes).unwrap();
    #[cfg(feature = "inventory")]
    zencodec_testkit::check_inventory_input_limit(dec, &bytes).unwrap();
}
