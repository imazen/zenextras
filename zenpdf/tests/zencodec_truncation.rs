#![cfg(feature = "zencodec")]

//! Truncation / EOF conformance for the PDF decoder via the zencodec traits.
//!
//! zenpdf is decode-only (there is no PDF encoder), so the known-good bytes come
//! from a tiny committed single-page fixture (`tests/fixtures/test.pdf`, ~1 KB)
//! rather than an encode round-trip. `zencodec_testkit::check_decode_truncation_series`
//! feeds a deterministic series of byte-truncated prefixes of that valid stream
//! into the decoder through the dyn-erased boundary. Every prefix must be
//! categorized as an incomplete/short-read error — never a panic, OOM, or an
//! `Internal`-class error. This guards the untrusted-input decode path.
//!
//! The fixture is intentionally small: the series runs a FULL decode per prefix.

use zenpdf::PdfDecoderConfig;

/// Known-good, committed single-page PDF (~1 KB). Small on purpose — the series
/// decodes once per truncation offset.
const VALID_PDF: &[u8] = include_bytes!("fixtures/test.pdf");

#[test]
fn decode_truncation_series_is_incomplete_never_panic() {
    assert!(VALID_PDF.len() > 8, "fixture is a real PDF stream");

    zencodec_testkit::check_decode_truncation_series(PdfDecoderConfig::new(), VALID_PDF)
        .expect("truncated input must categorize as incomplete, never panic/OOM/Internal");
}
