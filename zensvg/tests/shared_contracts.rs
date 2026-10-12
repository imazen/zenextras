use std::borrow::Cow;
use zencodec::decode::{Decode, DecodeJob, DecodePolicy, DecoderConfig};
use zensvg::SvgDecoderConfig;
#[test]
fn native_fixture_policy_and_inventory_contracts() {
    for bytes in [br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="3"><rect width="4" height="3" fill="red"/></svg>"#.as_slice()] {
        let dec = SvgDecoderConfig::new();
        let expected = dec.clone().job().decoder(Cow::Borrowed(bytes), &[])
            .unwrap().decode().unwrap().metadata();
        for (policy, accepted) in [(DecodePolicy::none(), true), (DecodePolicy::strict(), false)] {
            zencodec_testkit::check_decode_policy_admission(dec.clone(), bytes,
                DecodePolicy::none(), policy, accepted, &expected).unwrap();
        }
        zencodec_testkit::check_inventory_dispatch(dec.clone(), bytes).unwrap();
        #[cfg(feature = "inventory")]
        zencodec_testkit::check_inventory_input_limit(dec, bytes).unwrap();
    }
}
