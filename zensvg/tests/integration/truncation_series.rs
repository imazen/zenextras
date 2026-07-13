//! EOF/truncation conformance: cutting a known-good SVG short must categorize
//! as *incomplete client input* — never panic, OOM, or surface as an internal
//! (5xx) error for what is a 4xx-class truncated request.
//!
//! Delegates to the zencodec-testkit [`check_decode_truncation_series`] check,
//! which builds a deterministic prefix series (header sizes + fractions) and
//! runs each through the dyn-erased full decode path, verifying the erased
//! [`ErrorCategory`] lands in the incomplete-input set.

use zensvg::SvgDecoderConfig;

/// A small, known-good SVG document (all-ASCII, so every byte offset is a
/// valid UTF-8 truncation point — the truncation series exercises XML
/// well-formedness failures, not incidental UTF-8 boundary splits).
const VALID_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="30">
    <rect width="40" height="30" fill="red"/>
    <circle cx="20" cy="15" r="10" fill="blue"/>
</svg>"#;

#[test]
fn truncation_series_categorizes_as_incomplete_input() {
    zencodec_testkit::check_decode_truncation_series(SvgDecoderConfig::new(), VALID_SVG).expect(
        "truncated SVG must categorize as incomplete input (UnexpectedEof/MalformedImage), \
         never panic, OOM, or Internal",
    );
}
