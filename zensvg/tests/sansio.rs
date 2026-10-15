//! Incremental (sans-I/O) decode through `dyn_incremental_decoder`: the
//! testkit's incremental contract on small SVG documents.

use zencodec::decode::DecoderConfig as _;
use zensvg::SvgDecoderConfig;

#[test]
fn incremental_contract_holds() {
    assert!(SvgDecoderConfig::capabilities().incremental());
    for svg in [
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="3"><rect width="4" height="3" fill="red"/></svg>"#,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="30" viewBox="0 0 8 6"><defs><linearGradient id="g"><stop offset="0" stop-color="#08f"/><stop offset="1" stop-color="#f80" stop-opacity="0.5"/></linearGradient></defs><circle cx="4" cy="3" r="2.5" fill="url(#g)"/></svg>"##,
    ] {
        zencodec_testkit::check_incremental_capability_honesty(
            SvgDecoderConfig::new(),
            svg.as_bytes(),
        )
        .unwrap_or_else(|e| panic!("{svg}: {e:?}"));
    }
}
