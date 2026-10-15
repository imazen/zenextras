//! Incremental (sans-I/O) decode through `dyn_incremental_decoder`: the
//! testkit's incremental contract on the committed JP2 and J2K fixtures.
#![cfg(feature = "zencodec")]

use zencodec::decode::DecoderConfig as _;
use zenjp2::Jp2DecoderConfig;

#[test]
fn incremental_contract_holds() {
    assert!(Jp2DecoderConfig::capabilities().incremental());
    for (name, bytes) in [
        ("test.jp2", include_bytes!("fixtures/test.jp2").as_slice()),
        ("test.j2k", include_bytes!("fixtures/test.j2k").as_slice()),
    ] {
        zencodec_testkit::check_incremental_capability_honesty(Jp2DecoderConfig::new(), bytes)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }
}
