//! Incremental (sans-I/O) decode through `dyn_incremental_decoder`: the
//! testkit's incremental contract on the committed PDF fixture.
#![cfg(feature = "zencodec")]

use zencodec::decode::DecoderConfig as _;
use zenpdf::PdfDecoderConfig;

#[test]
fn incremental_contract_holds() {
    assert!(PdfDecoderConfig::capabilities().incremental());
    zencodec_testkit::check_incremental_capability_honesty(
        PdfDecoderConfig::new(),
        include_bytes!("fixtures/test.pdf"),
    )
    .unwrap_or_else(|e| panic!("test.pdf: {e:?}"));
}
