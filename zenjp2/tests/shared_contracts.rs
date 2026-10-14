#![cfg(feature = "zencodec")]
use std::borrow::Cow;
use zencodec::decode::{Decode, DecodeJob, DecodePolicy, DecoderConfig};
use zenjp2::Jp2DecoderConfig;
#[test]
fn native_fixture_policy_and_inventory_contracts() {
    for bytes in [
        include_bytes!("fixtures/test.jp2").as_slice(),
        include_bytes!("fixtures/test.j2k").as_slice(),
    ] {
        let dec = Jp2DecoderConfig::new();
        let expected = dec
            .clone()
            .job()
            .decoder(Cow::Borrowed(bytes), &[])
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
                bytes,
                DecodePolicy::none(),
                policy,
                accepted,
                &expected,
            )
            .unwrap();
        }
        zencodec_testkit::check_inventory_dispatch(dec.clone(), bytes).unwrap();
        #[cfg(feature = "inventory")]
        zencodec_testkit::check_inventory_input_limit(dec, bytes).unwrap();
    }
}
