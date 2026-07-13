//! Pattern-B conformance for zenjp2's zencodec decode integration.
//!
//! zenjp2 is decode-only (no encoder), so only the decode-side zencodec-testkit
//! checks apply:
//!
//! - [`check_decode_error_envelope`] — a codec's [`ErrorCategory`] and
//!   originating codec name must survive dyn-dispatch type erasure (the
//!   `At<CodecError>` envelope contract, "Pattern B").
//! - [`check_decode_truncation_series`] — cutting a known-good JP2/J2K image
//!   short must categorize as *incomplete client input* (never panic, OOM, or
//!   surface as an `Internal` 5xx for what is a 4xx-class truncated request).
//!
//! `tests/fixtures/test.jp2` / `test.j2k` are a tiny (16x16) known-good gradient
//! image encoded via Pillow/OpenJPEG in both the JP2 container and the raw J2K
//! codestream forms zenjp2's `is_jpeg2000()` detects — exercising both parse
//! paths (JP2 box parsing vs. bare codestream) against the truncation series.
//!
//! [`ErrorCategory`]: zencodec::ErrorCategory
//! [`check_decode_error_envelope`]: zencodec_testkit::check_decode_error_envelope
//! [`check_decode_truncation_series`]: zencodec_testkit::check_decode_truncation_series

use zenjp2::Jp2DecoderConfig;

const VALID_JP2: &[u8] = include_bytes!("fixtures/test.jp2");
const VALID_J2K: &[u8] = include_bytes!("fixtures/test.j2k");

#[test]
fn envelope_survives_dyn_erasure() {
    zencodec_testkit::check_decode_error_envelope(Jp2DecoderConfig::new(), &[0xABu8; 16]).expect(
        "zenjp2's ErrorCategory and codec name must survive dyn-dispatch erasure \
         (type Error = At<CodecError>)",
    );
}

#[test]
fn truncation_series_jp2_container_categorizes_as_incomplete_input() {
    zencodec_testkit::check_decode_truncation_series(Jp2DecoderConfig::new(), VALID_JP2).expect(
        "a truncated JP2 container must categorize as incomplete input \
         (UnexpectedEof/MalformedImage/UnsupportedImage*), never panic, OOM, or Internal",
    );
}

#[test]
fn truncation_series_j2k_codestream_categorizes_as_incomplete_input() {
    zencodec_testkit::check_decode_truncation_series(Jp2DecoderConfig::new(), VALID_J2K).expect(
        "a truncated raw J2K codestream must categorize as incomplete input \
         (UnexpectedEof/MalformedImage/UnsupportedImage*), never panic, OOM, or Internal",
    );
}
