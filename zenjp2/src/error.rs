//! Error types for JPEG 2000 decoding.

use alloc::string::String;

/// Result type alias for JPEG 2000 operations.
pub type Result<T> = core::result::Result<T, whereat::At<Jp2Error>>;

/// Errors that can occur during JPEG 2000 decoding.
///
/// Each variant maps to exactly one coarse [`zencodec::ErrorCategory`] (see the
/// [`CategorizedError`](zencodec::CategorizedError) impl) so consumers can route
/// on the category — HTTP status, retry policy, logging — without matching this
/// enum directly.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Jp2Error {
    /// `hayro_jpeg2000` failed to parse the JP2 container or J2K codestream.
    /// Maps to [`ImageError::Malformed`](zencodec::ImageError::Malformed),
    /// [`ImageError::UnexpectedEof`](zencodec::ImageError::UnexpectedEof), or
    /// [`ImageError::Unsupported`](zencodec::ImageError::Unsupported) depending
    /// on the specific hayro error — see the `CategorizedError` impl below.
    #[error("JPEG 2000 decode error: {0}")]
    Decode(#[from] hayro_jpeg2000::error::DecodeError),

    /// hayro decoded successfully but the returned pixel bytes don't fit its
    /// own declared width/height/format — an invariant violation in this
    /// crate's glue code, not a fault of the input bytes or the caller. Maps
    /// to [`InternalKind::Bug`](zencodec::InternalKind::Bug).
    #[error("pixel buffer error: {0}")]
    PixelBuffer(String),

    /// A fallible allocation failed (out-of-memory on an untrusted-sized
    /// buffer). Always available (no `zencodec` feature required) since
    /// [`crate::alloc_util`]'s helpers are feature-agnostic. Maps to
    /// [`ResourceError::OutOfMemory`](zencodec::ResourceError::OutOfMemory).
    #[error("out of memory: {0}")]
    OutOfMemory(String),

    /// An unsupported operation was requested — zenjp2 is single-image,
    /// one-shot decode only (no streaming or animation decode). Delegates its
    /// category to the wrapped [`zencodec::UnsupportedOperation`].
    #[cfg(feature = "zencodec")]
    #[error(transparent)]
    UnsupportedOperation(#[from] zencodec::UnsupportedOperation),

    /// A [`zencodec::ResourceLimits`] cap was exceeded. Delegates its category
    /// to the wrapped [`zencodec::LimitExceeded`] (preserves the
    /// [`LimitKind`](zencodec::LimitKind)).
    #[cfg(feature = "zencodec")]
    #[error(transparent)]
    Limit(#[from] zencodec::LimitExceeded),

    /// A decode-row-sink error while pushing decoded output through
    /// [`zencodec::helpers::copy_decode_to_sink`]. Maps to
    /// [`ErrorCategory::Io`](zencodec::ErrorCategory::Io).
    #[cfg(feature = "zencodec")]
    #[error("sink error: {0}")]
    Sink(zencodec::decode::SinkError),
}

// Codec-agnostic error taxonomy (zencodec's origin-first `ErrorCategory`
// reshape). Maps every `Jp2Error` variant to exactly one category so
// consumers can route on it without naming this enum.
#[cfg(feature = "zencodec")]
impl zencodec::CategorizedError for Jp2Error {
    fn codec_name(&self) -> Option<&'static str> {
        Some("zenjp2")
    }

    fn category(&self) -> zencodec::ErrorCategory {
        use hayro_jpeg2000::error::{DecodeError as H, DecodingError, FormatError, MarkerError};
        use zencodec::{
            ErrorCategory as C, ImageError as I, InternalKind, ResourceError,
            UnsupportedImageKind as U,
        };

        match self {
            Jp2Error::Decode(e) => match e {
                // The one explicit, unambiguous truncation signal hayro
                // reports — the ideal category for a cut-short bitstream.
                H::Decoding(DecodingError::UnexpectedEof) => C::Image(I::UnexpectedEof),
                // Explicit "unsupported" naming at the JP2 container level: a
                // recognized-but-foreign image format/profile.
                H::Format(FormatError::Unsupported) => C::Image(I::Unsupported(U::Type)),
                // Explicit "unsupported" naming at the codestream level: the
                // container is recognized, but this marker segment isn't
                // implemented — a feature gap, not a foreign format.
                H::Marker(MarkerError::Unsupported) => C::Image(I::Unsupported(U::Feature)),
                // Every other hayro variant (bad boxes, invalid markers/tiles,
                // failed dimension/quantization/progression validation, a
                // code-block that doesn't decode, a failed color transform)
                // is a structural defect in the container/codestream bytes.
                H::Format(_)
                | H::Marker(_)
                | H::Tile(_)
                | H::Validation(_)
                | H::Decoding(_)
                | H::Color(_) => C::Image(I::Malformed),
            },
            Jp2Error::PixelBuffer(_) => C::Internal(InternalKind::Bug),
            Jp2Error::OutOfMemory(_) => C::Resource(ResourceError::OutOfMemory),
            Jp2Error::UnsupportedOperation(op) => op.category(),
            Jp2Error::Limit(limit) => limit.category(),
            Jp2Error::Sink(_) => C::Io(zencodec::CodecIoKind::opaque()),
        }
    }
}

/// Bridge a bare [`Jp2Error`] into the shared
/// [`CodecError`](zencodec::CodecError) envelope (Pattern B).
///
/// `.start_at()` begins the location trace; [`CodecError::of`] then reads the
/// [`category`](zencodec::CategorizedError::category) *and* the
/// [`codec_name`](zencodec::CategorizedError::codec_name) from the value, keeping
/// the trace on the outside. With this, `?`/`.into()` on a bare `Jp2Error`
/// auto-wraps into the envelope the zencodec trait impls return.
///
/// Already-located `At<Jp2Error>` values convert via `.map_err(CodecError::of)`
/// instead — the orphan rule forbids a `From<At<Jp2Error>>` impl here (`At` is
/// not a fundamental type, so `At<Jp2Error>` is not a local type).
#[cfg(feature = "zencodec")]
impl From<Jp2Error> for whereat::At<zencodec::CodecError> {
    #[track_caller]
    fn from(e: Jp2Error) -> Self {
        use whereat::ErrorAtExt;
        zencodec::CodecError::of(e.start_at())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_decode() {
        let e = Jp2Error::Decode(hayro_jpeg2000::error::DecodeError::Decoding(
            hayro_jpeg2000::error::DecodingError::UnexpectedEof,
        ));
        assert!(e.to_string().contains("unexpected end of data"));
    }

    #[test]
    fn error_display_pixel_buffer() {
        let e = Jp2Error::PixelBuffer("bad size".into());
        assert!(e.to_string().contains("bad size"));
    }

    #[test]
    fn error_display_out_of_memory() {
        let e = Jp2Error::OutOfMemory("too big".into());
        assert!(e.to_string().contains("too big"));
    }

    #[cfg(feature = "zencodec")]
    mod zencodec_tests {
        use super::*;
        use hayro_jpeg2000::error::{
            ColorError, DecodeError as H, DecodingError, FormatError, MarkerError, TileError,
            ValidationError,
        };
        use zencodec::{
            CategorizedError, ErrorCategory as C, ImageError as I, InternalKind, LimitKind as L,
            ResourceError, UnsupportedImageKind as U, UnsupportedOperation,
        };

        #[test]
        fn codec_name_is_zenjp2() {
            assert_eq!(
                Jp2Error::OutOfMemory("x".into()).codec_name(),
                Some("zenjp2")
            );
        }

        #[test]
        fn decode_unexpected_eof_is_ideal_truncation_category() {
            let e = Jp2Error::Decode(H::Decoding(DecodingError::UnexpectedEof));
            assert_eq!(e.category(), C::Image(I::UnexpectedEof));
        }

        #[test]
        fn decode_format_unsupported_is_unsupported_type() {
            let e = Jp2Error::Decode(H::Format(FormatError::Unsupported));
            assert_eq!(e.category(), C::Image(I::Unsupported(U::Type)));
        }

        #[test]
        fn decode_marker_unsupported_is_unsupported_feature() {
            let e = Jp2Error::Decode(H::Marker(MarkerError::Unsupported));
            assert_eq!(e.category(), C::Image(I::Unsupported(U::Feature)));
        }

        #[test]
        fn decode_everything_else_is_malformed() {
            let cases = [
                H::Format(FormatError::InvalidSignature),
                H::Format(FormatError::InvalidFileType),
                H::Format(FormatError::InvalidBox),
                H::Format(FormatError::MissingCodestream),
                H::Marker(MarkerError::Invalid),
                H::Marker(MarkerError::Expected("SIZ")),
                H::Marker(MarkerError::Missing("SOT")),
                H::Marker(MarkerError::ParseFailure("COD")),
                H::Tile(TileError::Invalid),
                H::Tile(TileError::InvalidIndex),
                H::Tile(TileError::InvalidOffsets),
                H::Tile(TileError::PpmPptConflict),
                H::Validation(ValidationError::InvalidDimensions),
                H::Validation(ValidationError::ImageTooLarge),
                H::Validation(ValidationError::TooManyChannels),
                H::Validation(ValidationError::InvalidComponentMetadata),
                H::Decoding(DecodingError::CodeBlockDecodeFailure),
                H::Decoding(DecodingError::TooManyBitplanes),
                H::Color(ColorError::Mct),
                H::Color(ColorError::SyccConversionFailed),
            ];
            for case in cases {
                assert_eq!(
                    Jp2Error::Decode(case).category(),
                    C::Image(I::Malformed),
                    "expected Malformed for {case:?}"
                );
            }
        }

        #[test]
        fn pixel_buffer_is_internal_bug() {
            let e = Jp2Error::PixelBuffer("size mismatch".into());
            assert_eq!(e.category(), C::Internal(InternalKind::Bug));
        }

        #[test]
        fn out_of_memory_is_resource_out_of_memory() {
            let e = Jp2Error::OutOfMemory("oom".into());
            assert_eq!(e.category(), C::Resource(ResourceError::OutOfMemory));
        }

        #[test]
        fn unsupported_operation_delegates_category() {
            let e = Jp2Error::UnsupportedOperation(UnsupportedOperation::RowLevelDecode);
            assert_eq!(
                e.category(),
                C::Request(zencodec::RequestError::Unsupported(
                    UnsupportedOperation::RowLevelDecode
                ))
            );
        }

        #[test]
        fn limit_delegates_category_with_kind() {
            let e = Jp2Error::Limit(zencodec::LimitExceeded::Width {
                actual: 5000,
                max: 4096,
            });
            assert_eq!(e.category(), C::Resource(ResourceError::Limits(L::Width)));
        }

        #[test]
        fn sink_is_io() {
            let boxed: zencodec::decode::SinkError = "sink failed".into();
            let e = Jp2Error::Sink(boxed);
            assert_eq!(e.category(), C::Io(zencodec::CodecIoKind::opaque()));
        }

        // `At<Jp2Error>` forwards both the category and the codec name.
        #[test]
        fn category_through_at() {
            let err: whereat::At<Jp2Error> =
                whereat::At::wrap(Jp2Error::Decode(H::Decoding(DecodingError::UnexpectedEof)));
            assert_eq!(err.category(), C::Image(I::UnexpectedEof));
            assert_eq!(err.codec_name(), Some("zenjp2"));
        }

        // Bridge into the shared envelope preserves category + codec name.
        #[test]
        fn bridges_into_codec_error_envelope() {
            use zencodec::{CodecError, CodecErrorExt};

            let located: whereat::At<CodecError> =
                Jp2Error::Decode(H::Decoding(DecodingError::UnexpectedEof)).into();
            assert_eq!(located.category(), C::Image(I::UnexpectedEof));
            assert_eq!(located.error().codec(), Some("zenjp2"));

            // Survives erasure to a boxed trait object too.
            let boxed: alloc::boxed::Box<dyn core::error::Error + Send + Sync> =
                alloc::boxed::Box::new(located);
            assert_eq!(boxed.error_category(), Some(C::Image(I::UnexpectedEof)));
            assert_eq!(
                boxed.codec_error().and_then(CodecError::codec),
                Some("zenjp2")
            );
        }
    }
}
