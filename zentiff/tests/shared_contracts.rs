#![cfg(feature = "zencodec")]
use std::borrow::Cow;
use zencodec::Metadata;
use zencodec::decode::{Decode, DecodeJob, DecodePolicy, DecoderConfig};
use zencodec::encode::{EncodeJob, EncodePolicy, EncoderConfig};
use zenpixels::{PixelDescriptor, PixelSlice};
use zentiff::codec::{TiffDecoderCodecConfig as D, TiffEncoderCodecConfig as E};
#[test]
fn strided_fixture_policy_and_inventory_contracts() {
    let bytes = [1, 2, 3, 4, 5, 6, 255, 255, 255, 7, 8, 9, 10, 11, 12];
    let pixels = PixelSlice::new(&bytes, 2, 2, 9, PixelDescriptor::RGB8_SRGB).unwrap();
    let encoded = E::new().job().encode(pixels.clone()).unwrap();
    let expected = D::new()
        .job()
        .decoder(Cow::Borrowed(encoded.data()), &[])
        .unwrap()
        .decode()
        .unwrap()
        .metadata();
    for (policy, accepted) in [
        (EncodePolicy::none(), true),
        (EncodePolicy::strip_all(), false),
    ] {
        zencodec_testkit::check_encode_policy_admission(
            E::new(),
            D::new(),
            pixels.clone(),
            Metadata::none(),
            EncodePolicy::none(),
            policy,
            accepted,
            &expected,
        )
        .unwrap();
    }
    for (policy, accepted) in [
        (DecodePolicy::none(), true),
        (DecodePolicy::strict(), false),
    ] {
        zencodec_testkit::check_decode_policy_admission(
            D::new(),
            encoded.data(),
            DecodePolicy::none(),
            policy,
            accepted,
            &expected,
        )
        .unwrap();
    }
    zencodec_testkit::check_inventory_dispatch(D::new(), encoded.data()).unwrap();
    #[cfg(feature = "inventory")]
    zencodec_testkit::check_inventory_input_limit(D::new(), encoded.data()).unwrap();
}
