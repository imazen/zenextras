//! TIFF error types.

use alloc::string::String;

/// Errors from TIFF encode/decode operations.
///
/// Each variant maps to exactly one coarse [`zencodec::ErrorCategory`] (see the
/// [`CategorizedError`](zencodec::CategorizedError) impl, `zencodec`-gated) so
/// consumers can route on the category — HTTP status, retry policy, logging —
/// without matching this enum directly.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TiffError {
    /// Corrupt or invalid TIFF bitstream content reported by the underlying
    /// `tiff` crate (bad IFD/tag structure, invalid dimensions, a cycle in the
    /// offset chain, etc.) — see `tiff::TiffFormatError`. Maps to
    /// [`ErrorCategory::Image`]`(`[`ImageError::Malformed`]`)`.
    ///
    /// [`ErrorCategory::Image`]: zencodec::ErrorCategory::Image
    /// [`ImageError::Malformed`]: zencodec::ImageError::Malformed
    #[error("TIFF decode error: {0}")]
    Decode(String),

    /// TIFF encoding error from the underlying `tiff` crate — a foreign-library
    /// failure this codec has not classified further. Maps to
    /// [`ErrorCategory::Internal`]`(`[`InternalKind::Dependency`]`)`.
    ///
    /// [`ErrorCategory::Internal`]: zencodec::ErrorCategory::Internal
    /// [`InternalKind::Dependency`]: zencodec::InternalKind::Dependency
    #[error("TIFF encode error: {0}")]
    Encode(String),

    /// Input ended before a complete TIFF structure could be read — a genuine
    /// `std::io::ErrorKind::UnexpectedEof` from the underlying reader (the `tiff`
    /// crate surfaces this as `tiff::TiffError::IoError`, a *sibling* of
    /// `FormatError`, not a sub-kind of it). Distinguished from the generic
    /// [`Decode`](Self::Decode) so truncated input categorizes precisely. Maps to
    /// [`ErrorCategory::Image`]`(`[`ImageError::UnexpectedEof`]`)`.
    ///
    /// [`ErrorCategory::Image`]: zencodec::ErrorCategory::Image
    /// [`ImageError::UnexpectedEof`]: zencodec::ImageError::UnexpectedEof
    #[error("unexpected end of TIFF data: {0}")]
    Truncated(String),

    /// Invalid caller-supplied input: dimensions, buffer size, pixel format /
    /// compression combination the encoder cannot produce, or a `tiff`-crate
    /// usage error (API called incompatibly with the image's chunk layout).
    /// Maps to [`ErrorCategory::Request`]`(`[`RequestError::Invalid`]`(`[`InvalidKind::Parameters`]`))`.
    ///
    /// [`ErrorCategory::Request`]: zencodec::ErrorCategory::Request
    /// [`RequestError::Invalid`]: zencodec::RequestError::Invalid
    /// [`InvalidKind::Parameters`]: zencodec::InvalidKind::Parameters
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// A structurally-valid TIFF feature this codec does not implement (unknown
    /// compression method, unsupported color-type/bit-depth/predictor
    /// combination, an optional decode feature not compiled in). The bytes are a
    /// well-formed TIFF; a differently-built decoder (or a different codec)
    /// might still handle it. Maps to
    /// [`ErrorCategory::Image`]`(`[`ImageError::Unsupported`]`(`[`UnsupportedImageKind::Feature`]`))`.
    ///
    /// [`ErrorCategory::Image`]: zencodec::ErrorCategory::Image
    /// [`ImageError::Unsupported`]: zencodec::ImageError::Unsupported
    /// [`UnsupportedImageKind::Feature`]: zencodec::UnsupportedImageKind::Feature
    #[error("unsupported TIFF feature: {0}")]
    UnsupportedFeature(String),

    /// An unsupported zencodec API operation (animation, row-level decode, …).
    /// Origin is the caller's request (the zencodec adapter invoked an
    /// operation this codec doesn't implement), never the image bytes.
    /// Delegates its category to the wrapped
    /// [`zencodec::UnsupportedOperation`] (always
    /// [`ErrorCategory::Request`]`(`[`RequestError::Unsupported`]`(_))`).
    ///
    /// [`ErrorCategory::Request`]: zencodec::ErrorCategory::Request
    /// [`RequestError::Unsupported`]: zencodec::RequestError::Unsupported
    #[cfg(feature = "zencodec")]
    #[error("unsupported operation: {0}")]
    Unsupported(zencodec::UnsupportedOperation),

    /// An integer conversion between a TIFF-declared value (a dimension,
    /// offset, or count read from the file) and a platform numeric type failed
    /// (`tiff::TiffError::IntSizeError`). The overflowing value is claimed by
    /// the bytes themselves, not a caller-configured cap, so this is kept
    /// distinguishable from the generic [`Decode`](Self::Decode) bucket rather
    /// than folded into a stringified message. Maps to
    /// [`ErrorCategory::Image`]`(`[`ImageError::Malformed`]`)`.
    ///
    /// [`ErrorCategory::Image`]: zencodec::ErrorCategory::Image
    /// [`ImageError::Malformed`]: zencodec::ImageError::Malformed
    #[error(
        "integer size overflow: a TIFF-declared value does not fit the platform's numeric range"
    )]
    IntSizeOverflow,

    /// Memory acquisition failed: a fallible allocation returned an error, or a
    /// size computation overflowed the platform's address space (so the buffer
    /// can never be allocated). Maps to
    /// [`ErrorCategory::Resource`]`(`[`ResourceError::OutOfMemory`]`)`.
    ///
    /// Also used, more coarsely, for zentiff's own native (zencodec-independent)
    /// `TiffDecodeConfig` cap checks (`max_width`/`max_height`/`max_pixels`/
    /// `max_memory_bytes`) — those are genuinely a configured-limit rejection
    /// rather than an allocation failure, but carry no [`LimitKind`](zencodec::LimitKind)
    /// here since the decode core stays free of a `zencodec` dependency (see
    /// [`Limit`](Self::Limit) for the `zencodec`-gated, precisely-kinded form
    /// used by the `codec` module's `ResourceLimits` enforcement). Known
    /// imprecision, not fixed in this pass.
    ///
    /// [`ErrorCategory::Resource`]: zencodec::ErrorCategory::Resource
    /// [`ResourceError::OutOfMemory`]: zencodec::ResourceError::OutOfMemory
    #[error("limit exceeded: {0}")]
    LimitExceeded(String),

    /// A configured [`zencodec::ResourceLimits`] cap was exceeded. Wraps the
    /// typed [`zencodec::LimitExceeded`] so the [`LimitKind`](zencodec::LimitKind)
    /// is preserved; delegates its category to
    /// [`ErrorCategory::Resource`]`(`[`ResourceError::Limits`]`(kind))`.
    ///
    /// [`ErrorCategory::Resource`]: zencodec::ErrorCategory::Resource
    /// [`ResourceError::Limits`]: zencodec::ResourceError::Limits
    #[cfg(feature = "zencodec")]
    #[error("resource limit exceeded: {0}")]
    Limit(zencodec::LimitExceeded),

    /// Operation stopped by cooperative cancellation. Delegates its category to
    /// the wrapped [`enough::StopReason`] — carried directly (not re-derived),
    /// so both `Cancelled` and `TimedOut` map correctly to
    /// [`ErrorCategory::Lifecycle`]`(reason)` with no special-casing needed here.
    ///
    /// [`ErrorCategory::Lifecycle`]: zencodec::ErrorCategory::Lifecycle
    #[error("stopped: {0}")]
    Stopped(enough::StopReason),

    /// I/O error (not a truncation — see [`Truncated`](Self::Truncated) for
    /// `UnexpectedEof`). Maps to [`ErrorCategory::Io`] carrying the
    /// [`std::io::ErrorKind`] via [`CodecIoKind`](zencodec::CodecIoKind).
    ///
    /// [`ErrorCategory::Io`]: zencodec::ErrorCategory::Io
    #[error("I/O error: {0}")]
    Io(std::io::Error),

    /// Caller-supplied pixel buffer has an invalid layout (wrong size, stride,
    /// or descriptor). Maps to
    /// [`ErrorCategory::Request`]`(`[`RequestError::Invalid`]`(`[`InvalidKind::Buffer`]`))`.
    ///
    /// [`ErrorCategory::Request`]: zencodec::ErrorCategory::Request
    /// [`RequestError::Invalid`]: zencodec::RequestError::Invalid
    /// [`InvalidKind::Buffer`]: zencodec::InvalidKind::Buffer
    #[error("buffer error: {0}")]
    Buffer(zenpixels::BufferError),
}

impl From<enough::StopReason> for TiffError {
    fn from(reason: enough::StopReason) -> Self {
        TiffError::Stopped(reason)
    }
}

impl From<tiff::TiffError> for TiffError {
    fn from(e: tiff::TiffError) -> Self {
        match e {
            tiff::TiffError::FormatError(ref fe) => {
                TiffError::Decode(alloc::format!("format error: {fe}"))
            }
            // The tiff crate's own decoder-feature gap (unsupported color type,
            // compression method, sample depth, …): a well-formed TIFF using a
            // feature this codec doesn't implement — image-bytes-origin.
            tiff::TiffError::UnsupportedError(ref ue) => {
                TiffError::UnsupportedFeature(alloc::format!("{ue}"))
            }
            // Distinguish a genuine truncation (`UnexpectedEof`) from other I/O
            // failures: the tiff crate has no dedicated EOF variant on
            // `TiffFormatError` — reads past the end of a truncated in-memory
            // reader surface as `TiffError::IoError` with that io::ErrorKind, a
            // *sibling* of `FormatError`, not a sub-kind of it.
            tiff::TiffError::IoError(io) if io.kind() == std::io::ErrorKind::UnexpectedEof => {
                TiffError::Truncated(alloc::format!("{io}"))
            }
            tiff::TiffError::IoError(io) => TiffError::Io(io),
            tiff::TiffError::LimitsExceeded => {
                TiffError::LimitExceeded("tiff decoder limits exceeded".into())
            }
            // The file's own declared dimension/offset/count doesn't fit a
            // platform numeric type — the bytes claim something unrepresentable,
            // not a caller-configured cap. Kept as its own distinguishable
            // variant (see `IntSizeOverflow`'s doc) rather than folded into the
            // generic limit-exceeded bucket.
            tiff::TiffError::IntSizeError => TiffError::IntSizeOverflow,
            tiff::TiffError::UsageError(ref ue) => {
                TiffError::InvalidInput(alloc::format!("usage error: {ue}"))
            }
        }
    }
}

impl From<zenpixels::BufferError> for TiffError {
    fn from(e: zenpixels::BufferError) -> Self {
        TiffError::Buffer(e)
    }
}

#[cfg(feature = "zencodec")]
impl From<zencodec::UnsupportedOperation> for TiffError {
    fn from(op: zencodec::UnsupportedOperation) -> Self {
        TiffError::Unsupported(op)
    }
}

#[cfg(feature = "zencodec")]
impl From<zencodec::LimitExceeded> for TiffError {
    fn from(limit: zencodec::LimitExceeded) -> Self {
        TiffError::Limit(limit)
    }
}

/// Result type alias for zentiff operations with location tracking.
pub type Result<T> = core::result::Result<T, whereat::At<TiffError>>;

// Codec-agnostic error taxonomy (zencodec PR #116, origin-first two-level
// reshape of #99/#103 — never published, so this is not a break of any
// crates.io API). Maps every `TiffError` variant to exactly one coarse
// `ErrorCategory` so consumers can route on the category without naming this
// enum. `zencodec` is optional for zentiff (the decode/encode core stays
// dependency-free), so this impl is gated on the feature.
#[cfg(feature = "zencodec")]
impl zencodec::CategorizedError for TiffError {
    fn codec_name(&self) -> Option<&'static str> {
        Some("zentiff")
    }

    fn category(&self) -> zencodec::ErrorCategory {
        use zencodec::{
            ErrorCategory as C, ImageError, InternalKind, InvalidKind, RequestError, ResourceError,
        };

        match self {
            // Corrupt / invalid bitstream content.
            TiffError::Decode(_) => C::Image(ImageError::Malformed),

            // Foreign-library (tiff crate) encode failure, unclassified.
            TiffError::Encode(_) => C::Internal(InternalKind::Dependency),

            // Genuine truncation / unexpected end of data.
            TiffError::Truncated(_) => C::Image(ImageError::UnexpectedEof),

            // Bad caller parameters / configuration / pixel-format-on-encode /
            // tiff-crate usage error.
            TiffError::InvalidInput(_) => {
                C::Request(RequestError::Invalid(InvalidKind::Parameters))
            }

            // A valid TIFF feature we don't implement.
            TiffError::UnsupportedFeature(_) => C::Image(ImageError::Unsupported(
                zencodec::UnsupportedImageKind::Feature,
            )),

            // Delegate to the wrapped zencodec cause types — they carry their
            // own `CategorizedError` impl (op / limit-kind / stop-reason).
            TiffError::Unsupported(op) => op.category(),

            // The file's own declared size doesn't fit a platform numeric type.
            TiffError::IntSizeOverflow => C::Image(ImageError::Malformed),

            // Memory acquisition failure (alloc failed or address-space
            // overflow) — also covers the native `TiffDecodeConfig` cap checks
            // (see the variant doc for the known coarseness there).
            TiffError::LimitExceeded(_) => C::Resource(ResourceError::OutOfMemory),

            TiffError::Limit(limit) => limit.category(),

            TiffError::Stopped(reason) => reason.category(),

            // I/O failure (not a truncation — see `Truncated`).
            TiffError::Io(io) => C::Io(zencodec::CodecIoKind::from(io)),

            // Caller-supplied pixel buffer has the wrong geometry.
            TiffError::Buffer(_) => C::Request(RequestError::Invalid(InvalidKind::Buffer)),
        }
    }
}

#[cfg(all(test, feature = "zencodec"))]
mod category_tests {
    use super::*;
    use whereat::{At, at};
    use zencodec::{
        CategorizedError, ErrorCategory as C, ImageError, InternalKind, InvalidKind, RequestError,
        ResourceError,
    };

    #[test]
    fn codec_name_is_zentiff() {
        assert_eq!(TiffError::Decode("x".into()).codec_name(), Some("zentiff"));
    }

    #[test]
    fn error_category_mapping() {
        assert_eq!(
            TiffError::Decode("x".into()).category(),
            C::Image(ImageError::Malformed)
        );
        assert_eq!(
            TiffError::Encode("x".into()).category(),
            C::Internal(InternalKind::Dependency)
        );
        assert_eq!(
            TiffError::Truncated("x".into()).category(),
            C::Image(ImageError::UnexpectedEof)
        );
        assert_eq!(
            TiffError::InvalidInput("x".into()).category(),
            C::Request(RequestError::Invalid(InvalidKind::Parameters))
        );
        assert_eq!(
            TiffError::UnsupportedFeature("x".into()).category(),
            C::Image(ImageError::Unsupported(
                zencodec::UnsupportedImageKind::Feature
            ))
        );
        assert_eq!(
            TiffError::IntSizeOverflow.category(),
            C::Image(ImageError::Malformed)
        );
        assert_eq!(
            TiffError::LimitExceeded("x".into()).category(),
            C::Resource(ResourceError::OutOfMemory)
        );
        assert_eq!(
            TiffError::Buffer(zenpixels::BufferError::InsufficientData).category(),
            C::Request(RequestError::Invalid(InvalidKind::Buffer))
        );
    }

    #[test]
    fn unsupported_operation_delegates() {
        let op = zencodec::UnsupportedOperation::AnimationEncode;
        let e = TiffError::Unsupported(op);
        assert_eq!(e.category(), op.category());
        assert_eq!(
            e.category(),
            C::Request(RequestError::Unsupported(
                zencodec::UnsupportedOperation::AnimationEncode
            ))
        );
    }

    #[test]
    fn limit_delegates_and_preserves_kind() {
        let limit = zencodec::LimitExceeded::Memory { actual: 9, max: 4 };
        let e = TiffError::Limit(limit.clone());
        assert_eq!(e.category(), limit.category());
        assert_eq!(
            e.category(),
            C::Resource(ResourceError::Limits(zencodec::LimitKind::Memory))
        );
    }

    #[test]
    fn stopped_maps_both_cancelled_and_timed_out() {
        assert_eq!(
            TiffError::Stopped(enough::StopReason::Cancelled).category(),
            C::Lifecycle(enough::StopReason::Cancelled)
        );
        assert_eq!(
            TiffError::Stopped(enough::StopReason::TimedOut).category(),
            C::Lifecycle(enough::StopReason::TimedOut)
        );
    }

    #[test]
    fn io_category_carries_error_kind() {
        let io = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let e = TiffError::Io(io);
        assert_eq!(
            e.category(),
            C::Io(std::io::ErrorKind::PermissionDenied.into())
        );
    }

    #[test]
    fn tiff_int_size_error_becomes_overflow_variant() {
        // `tiff::TiffError::IntSizeError` must stay distinguishable, not folded
        // into a generic stringified bucket.
        let converted: TiffError = tiff::TiffError::IntSizeError.into();
        assert!(matches!(converted, TiffError::IntSizeOverflow));
        assert_eq!(converted.category(), C::Image(ImageError::Malformed));
    }

    #[test]
    fn tiff_io_error_unexpected_eof_becomes_truncated() {
        let io = std::io::Error::from(std::io::ErrorKind::UnexpectedEof);
        let converted: TiffError = tiff::TiffError::IoError(io).into();
        assert!(matches!(converted, TiffError::Truncated(_)));
        assert_eq!(converted.category(), C::Image(ImageError::UnexpectedEof));
    }

    #[test]
    fn tiff_io_error_other_kind_stays_io() {
        let io = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let converted: TiffError = tiff::TiffError::IoError(io).into();
        assert!(matches!(converted, TiffError::Io(_)));
        assert!(matches!(converted.category(), C::Io(_)));
    }

    #[test]
    fn category_is_preserved_through_at() {
        let err: At<TiffError> = at!(TiffError::Truncated("eof".into()));
        assert_eq!(err.category(), C::Image(ImageError::UnexpectedEof));
        assert_eq!(err.codec_name(), Some("zentiff"));
    }
}
