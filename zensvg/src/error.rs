use core::fmt;

use whereat::At;

/// Errors that can occur during SVG operations.
///
/// Each variant maps to exactly one coarse [`zencodec::ErrorCategory`] (see the
/// [`CategorizedError`](zencodec::CategorizedError) impl) so consumers can route
/// on the category — HTTP status, retry policy, logging — without matching this
/// enum directly.
#[derive(Debug)]
#[non_exhaustive]
pub enum SvgError {
    /// Corrupt or invalid SVG content: not valid UTF-8, a malformed gzip
    /// (SVGZ) container, an invalid/zero declared size, or an XML structural
    /// parse failure. Maps to [`ErrorCategory::Image(ImageError::Malformed)`].
    ///
    /// [`ErrorCategory::Image(ImageError::Malformed)`]: zencodec::ErrorCategory::Image
    Parse(String),

    /// usvg's own internal element-count safety cap was exceeded (its
    /// hardcoded 1,000,000-element ceiling) — a structural anti-DoS bound on
    /// parse work, not sized in pixels or bytes. Maps to
    /// [`ErrorCategory::Resource(ResourceError::Limits(LimitKind::Scans))`]
    /// (the closest existing analog: a structural per-document complexity
    /// cap, the same shape as a progressive-image scan-count ceiling).
    ///
    /// [`ErrorCategory::Resource(ResourceError::Limits(LimitKind::Scans))`]: zencodec::ErrorCategory::Resource
    TooManyElements,

    /// Data does not look like SVG/SVGZ at all — a pre-parse sniff failure,
    /// as opposed to recognized-but-corrupt content (that's
    /// [`Parse`](Self::Parse)). Maps to
    /// [`ErrorCategory::Image(ImageError::Unsupported(UnsupportedImageKind::Type))`].
    ///
    /// [`ErrorCategory::Image(ImageError::Unsupported(UnsupportedImageKind::Type))`]: zencodec::ErrorCategory::Image
    NotSvg,

    /// Caller-supplied width/height/scale/fit combined with the SVG's
    /// (already-known-positive) intrinsic size to resolve to a zero-pixel
    /// output. Fixable by the caller adjusting `RenderOptions` — unlike a
    /// genuinely zero-sized SVG (that's [`Parse`](Self::Parse)), a different
    /// scale/width/height/fit combination can produce a non-zero result. Maps
    /// to [`ErrorCategory::Request(RequestError::Invalid(InvalidKind::Parameters))`].
    ///
    /// [`ErrorCategory::Request(RequestError::Invalid(InvalidKind::Parameters))`]: zencodec::ErrorCategory::Request
    ZeroOutputDimensions,

    /// The output raster failed to allocate (tiny-skia's `Pixmap::new`
    /// returned `None`) — oversized or address-space-exhausting dimensions.
    /// Maps to [`ErrorCategory::Resource(ResourceError::OutOfMemory)`].
    ///
    /// [`ErrorCategory::Resource(ResourceError::OutOfMemory)`]: zencodec::ErrorCategory::Resource
    AllocationFailed {
        /// Requested output width in pixels.
        width: u32,
        /// Requested output height in pixels.
        height: u32,
    },

    /// A fallible raw byte/element allocation ran out of memory — distinct
    /// from [`AllocationFailed`](Self::AllocationFailed), which is
    /// specifically the tiny-skia output-raster allocation; this is
    /// zensvg's own generic `try_reserve`-style helper allocations
    /// (currently unused templates awaiting a zensvg-owned allocation site,
    /// see `crate::alloc_util`). Maps to
    /// [`ErrorCategory::Resource(ResourceError::OutOfMemory)`].
    ///
    /// [`ErrorCategory::Resource(ResourceError::OutOfMemory)`]: zencodec::ErrorCategory::Resource
    OutOfMemory {
        /// The number of bytes that failed to allocate.
        bytes: usize,
    },

    /// zensvg's own rendered pixel data didn't match the dimensions/format
    /// handed to the pixel-buffer constructor — an internal invariant
    /// violation, not a caller or image fault. Maps to
    /// [`ErrorCategory::Internal(InternalKind::Bug)`].
    ///
    /// [`ErrorCategory::Internal(InternalKind::Bug)`]: zencodec::ErrorCategory::Internal
    PixelBufferMismatch(String),

    /// A configured resource limit was exceeded. Wraps the typed
    /// [`zencodec::LimitExceeded`] so the [`LimitKind`](zencodec::LimitKind) is
    /// preserved; delegates its category to
    /// [`ErrorCategory::Resource(ResourceError::Limits(kind))`].
    ///
    /// [`ErrorCategory::Resource(ResourceError::Limits(kind))`]: zencodec::ErrorCategory::Resource
    Limit(zencodec::LimitExceeded),

    /// SVGZ decompressed size exceeded its declared input-size ceiling — a
    /// decompression-bomb guard. No [`zencodec::LimitExceeded`] variant
    /// carries this (per its docs, `Scans`/`DecompressionRatio` route directly
    /// via `ErrorCategory::Resource`, with no matching actual/max-carrying
    /// struct variant). Maps to
    /// [`ErrorCategory::Resource(ResourceError::Limits(LimitKind::DecompressionRatio))`].
    ///
    /// [`ErrorCategory::Resource(ResourceError::Limits(LimitKind::DecompressionRatio))`]: zencodec::ErrorCategory::Resource
    DecompressionBomb {
        /// Decompressed size in bytes.
        actual: u64,
        /// The declared input-size ceiling, in bytes.
        max: u64,
    },

    /// An unsupported operation, including pixel-format negotiation failures.
    /// Delegates its category to the wrapped [`zencodec::UnsupportedOperation`]
    /// (`PixelFormat` → [`ErrorCategory::UnsupportedPixelFormat`], otherwise
    /// [`ErrorCategory::Request(RequestError::Unsupported)`]).
    ///
    /// [`ErrorCategory::UnsupportedPixelFormat`]: zencodec::ErrorCategory::Request
    /// [`ErrorCategory::Request(RequestError::Unsupported)`]: zencodec::ErrorCategory::Request
    Unsupported(zencodec::UnsupportedOperation),

    /// Operation stopped by cooperative cancellation. Delegates its category
    /// to the wrapped [`enough::StopReason`]
    /// ([`ErrorCategory::Lifecycle`](zencodec::ErrorCategory::Lifecycle)).
    Stopped(enough::StopReason),

    /// The caller-provided decode sink rejected a row or output — an opaque,
    /// foreign (caller-supplied) failure this codec cannot classify any
    /// further. Maps to
    /// [`ErrorCategory::Internal(InternalKind::Dependency)`].
    ///
    /// [`ErrorCategory::Internal(InternalKind::Dependency)`]: zencodec::ErrorCategory::Internal
    Sink(String),

    /// `quick_xml` failed while writing optimized output. The writer here is
    /// always an in-memory `Vec` (never a real I/O sink), so a failure is a
    /// `quick_xml`-internal (encoding/escaping) issue rather than genuine I/O.
    /// Maps to [`ErrorCategory::Internal(InternalKind::Dependency)`].
    ///
    /// [`ErrorCategory::Internal(InternalKind::Dependency)`]: zencodec::ErrorCategory::Internal
    #[cfg(feature = "optimize")]
    XmlWrite(String),

    /// I/O error (SVGZ gzip compression).
    /// Maps to [`ErrorCategory::Io`](zencodec::ErrorCategory::Io).
    #[cfg(feature = "optimize")]
    Io(std::io::Error),
}

impl fmt::Display for SvgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(msg) => write!(f, "SVG parse error: {msg}"),
            Self::TooManyElements => f.write_str("SVG exceeds the maximum supported element count"),
            Self::NotSvg => f.write_str("input is not valid SVG"),
            Self::ZeroOutputDimensions => f.write_str("computed output dimensions are zero"),
            Self::AllocationFailed { width, height } => {
                write!(f, "failed to allocate a {width}x{height} output raster")
            }
            Self::OutOfMemory { bytes } => write!(f, "out of memory allocating {bytes} bytes"),
            Self::PixelBufferMismatch(msg) => {
                write!(f, "internal error: rendered pixel buffer mismatch: {msg}")
            }
            Self::Limit(e) => write!(f, "resource limit exceeded: {e}"),
            Self::DecompressionBomb { actual, max } => {
                write!(f, "decompressed SVGZ size {actual} exceeds limit {max}")
            }
            Self::Unsupported(op) => write!(f, "unsupported operation: {op}"),
            Self::Stopped(reason) => write!(f, "stopped: {reason}"),
            Self::Sink(msg) => write!(f, "decode sink error: {msg}"),
            #[cfg(feature = "optimize")]
            Self::XmlWrite(msg) => write!(f, "XML write error: {msg}"),
            #[cfg(feature = "optimize")]
            Self::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for SvgError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            #[cfg(feature = "optimize")]
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<zencodec::UnsupportedOperation> for SvgError {
    fn from(op: zencodec::UnsupportedOperation) -> Self {
        Self::Unsupported(op)
    }
}

impl From<zencodec::LimitExceeded> for SvgError {
    fn from(e: zencodec::LimitExceeded) -> Self {
        Self::Limit(e)
    }
}

impl From<enough::StopReason> for SvgError {
    fn from(reason: enough::StopReason) -> Self {
        Self::Stopped(reason)
    }
}

#[cfg(feature = "optimize")]
impl From<std::io::Error> for SvgError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Map each [`usvg::Error`] variant to the closest-fitting [`SvgError`].
///
/// `ElementsLimitReached` is usvg's own internal anti-DoS structural cap — a
/// completely different origin (resource exhaustion, not content) — so it gets
/// its own variant. Every other variant (`NotAnUtf8Str`, `MalformedGZip`,
/// `InvalidSize`, `ParsingFailed`) is bad-bitstream-content and lands in
/// [`Parse`](SvgError::Parse) — same category either way, kept as one variant
/// rather than four near-duplicates (`usvg::Error`'s own `Display` preserves
/// which one occurred, in the message).
impl From<usvg::Error> for SvgError {
    fn from(e: usvg::Error) -> Self {
        match e {
            usvg::Error::ElementsLimitReached => Self::TooManyElements,
            other => Self::Parse(other.to_string()),
        }
    }
}

// Codec-agnostic error taxonomy (zencodec PR #116, the origin-first two-level
// reshape of PR #103's flat set). Maps every `SvgError` variant to exactly one
// coarse `ErrorCategory` so consumers can route on the category without naming
// this enum. `zencodec` is a non-optional dependency, so this impl is
// unconditional.
impl zencodec::CategorizedError for SvgError {
    fn codec_name(&self) -> Option<&'static str> {
        Some("zensvg")
    }

    fn category(&self) -> zencodec::ErrorCategory {
        use zencodec::{
            ErrorCategory as C, ImageError, InternalKind, InvalidKind, LimitKind, RequestError,
            ResourceError, UnsupportedImageKind,
        };
        match self {
            // Bad bitstream content: not valid UTF-8, malformed gzip, an
            // invalid declared size, or an XML structural parse failure.
            Self::Parse(_) => C::Image(ImageError::Malformed),

            // usvg's internal element-count safety cap: a structural,
            // non-pixel/byte anti-DoS bound. `Scans` is the closest existing
            // analog (a structural per-document complexity cap).
            Self::TooManyElements => C::Resource(ResourceError::Limits(LimitKind::Scans)),

            // Not SVG/SVGZ at all — a different format, not corrupt content.
            Self::NotSvg => C::Image(ImageError::Unsupported(UnsupportedImageKind::Type)),

            // Caller-controlled render options resolved to a zero-pixel
            // output; a different scale/width/height/fit fixes it, so this is
            // the caller's request to fix, not the image's fault.
            Self::ZeroOutputDimensions => {
                C::Request(RequestError::Invalid(InvalidKind::Parameters))
            }

            // Output raster allocation failed.
            Self::AllocationFailed { .. } => C::Resource(ResourceError::OutOfMemory),

            // A generic fallible byte/element allocation ran out of memory.
            Self::OutOfMemory { .. } => C::Resource(ResourceError::OutOfMemory),

            // zensvg's own render-to-buffer handoff invariant broke.
            Self::PixelBufferMismatch(_) => C::Internal(InternalKind::Bug),

            // Delegate to the wrapped zencodec cause types — they carry their
            // own `CategorizedError` impl (kind / pixel-format / stop-reason).
            Self::Limit(limit) => limit.category(),
            Self::Unsupported(op) => op.category(),
            Self::Stopped(reason) => reason.category(),

            // SVGZ decompression-bomb guard — no `LimitExceeded` variant
            // carries this; route directly per `LimitKind::DecompressionRatio`.
            Self::DecompressionBomb { .. } => {
                C::Resource(ResourceError::Limits(LimitKind::DecompressionRatio))
            }

            // Caller-supplied sink is opaque/foreign — this codec cannot
            // classify its failure any further than "a dependency failed".
            Self::Sink(_) => C::Internal(InternalKind::Dependency),

            // quick_xml's writer here is always an in-memory Vec (never real
            // I/O), so a write failure is a dependency-internal issue, not Io.
            #[cfg(feature = "optimize")]
            Self::XmlWrite(_) => C::Internal(InternalKind::Dependency),

            // Genuine I/O failure (gzip compression path). `CodecIoKind::from`
            // is gated behind zencodec's `std` feature (which zensvg does not
            // enable); `opaque()` matches the established convention (zenpng's
            // `Io` variant maps the same way).
            #[cfg(feature = "optimize")]
            Self::Io(_) => C::Io(zencodec::CodecIoKind::opaque()),
        }
    }
}

/// Bridge a bare [`SvgError`] into the shared
/// [`CodecError`](zencodec::CodecError) envelope (Pattern B).
///
/// `.start_at()` begins the location trace at this conversion point;
/// [`CodecError::of`] then reads the
/// [`category`](zencodec::CategorizedError::category) *and* the
/// [`codec_name`](zencodec::CategorizedError::codec_name) from the value. With
/// this, `?`/`.into()` on a bare `SvgError` auto-wraps into the envelope the
/// zencodec trait impls (`crate::codec`) return.
///
/// [`CodecError::of`]: zencodec::CodecError::of
impl From<SvgError> for At<zencodec::CodecError> {
    #[track_caller]
    fn from(e: SvgError) -> Self {
        use whereat::ErrorAtExt;
        zencodec::CodecError::of(e.start_at())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zencodec::{
        CategorizedError, ErrorCategory as C, ImageError, InternalKind, InvalidKind,
        LimitKind as L, RequestError, ResourceError, UnsupportedImageKind,
    };

    #[test]
    fn error_display_parse() {
        let e = SvgError::Parse("bad xml".into());
        assert!(e.to_string().contains("bad xml"));
    }

    #[test]
    fn error_display_zero_output_dimensions() {
        let e = SvgError::ZeroOutputDimensions;
        assert!(e.to_string().contains("zero"));
    }

    #[test]
    fn error_from_stop_reason() {
        let reason = enough::StopReason::Cancelled;
        let e: SvgError = reason.into();
        assert!(matches!(e, SvgError::Stopped(_)));
    }

    #[test]
    fn error_from_unsupported_operation() {
        let op = zencodec::UnsupportedOperation::RowLevelDecode;
        let e: SvgError = op.into();
        assert!(matches!(e, SvgError::Unsupported(_)));
    }

    #[test]
    fn error_from_limit_exceeded() {
        let limit = zencodec::LimitExceeded::Pixels { actual: 9, max: 4 };
        let e: SvgError = limit.into();
        assert!(matches!(e, SvgError::Limit(_)));
    }

    #[test]
    fn error_from_usvg_elements_limit() {
        let e: SvgError = usvg::Error::ElementsLimitReached.into();
        assert!(matches!(e, SvgError::TooManyElements));
    }

    #[test]
    fn error_from_usvg_other_variants_are_parse() {
        let e: SvgError = usvg::Error::NotAnUtf8Str.into();
        assert!(matches!(e, SvgError::Parse(_)));
        let e: SvgError = usvg::Error::MalformedGZip.into();
        assert!(matches!(e, SvgError::Parse(_)));
        let e: SvgError = usvg::Error::InvalidSize.into();
        assert!(matches!(e, SvgError::Parse(_)));
    }

    // Every `SvgError` variant maps to its documented `ErrorCategory`.
    #[test]
    fn error_category_mapping() {
        assert_eq!(SvgError::Parse("x".into()).codec_name(), Some("zensvg"));

        assert_eq!(
            SvgError::Parse("x".into()).category(),
            C::Image(ImageError::Malformed)
        );
        assert_eq!(
            SvgError::TooManyElements.category(),
            C::Resource(ResourceError::Limits(L::Scans))
        );
        assert_eq!(
            SvgError::NotSvg.category(),
            C::Image(ImageError::Unsupported(UnsupportedImageKind::Type))
        );
        assert_eq!(
            SvgError::ZeroOutputDimensions.category(),
            C::Request(RequestError::Invalid(InvalidKind::Parameters))
        );
        assert_eq!(
            SvgError::AllocationFailed {
                width: 10,
                height: 10
            }
            .category(),
            C::Resource(ResourceError::OutOfMemory)
        );
        assert_eq!(
            SvgError::PixelBufferMismatch("x".into()).category(),
            C::Internal(InternalKind::Bug)
        );
        assert_eq!(
            SvgError::DecompressionBomb {
                actual: 100,
                max: 10
            }
            .category(),
            C::Resource(ResourceError::Limits(L::DecompressionRatio))
        );
        assert_eq!(
            SvgError::Sink("x".into()).category(),
            C::Internal(InternalKind::Dependency)
        );

        // Delegated arms.
        assert_eq!(
            SvgError::Unsupported(zencodec::UnsupportedOperation::PixelFormat).category(),
            C::Request(RequestError::Unsupported(
                zencodec::UnsupportedOperation::PixelFormat
            ))
        );
        assert_eq!(
            SvgError::Unsupported(zencodec::UnsupportedOperation::RowLevelDecode).category(),
            C::Request(RequestError::Unsupported(
                zencodec::UnsupportedOperation::RowLevelDecode
            ))
        );
        assert_eq!(
            SvgError::Limit(zencodec::LimitExceeded::Memory { actual: 9, max: 4 }).category(),
            C::Resource(ResourceError::Limits(L::Memory))
        );
        assert_eq!(
            SvgError::Limit(zencodec::LimitExceeded::Width { actual: 9, max: 4 }).category(),
            C::Resource(ResourceError::Limits(L::Width))
        );
        assert_eq!(
            SvgError::Stopped(enough::StopReason::Cancelled).category(),
            C::Lifecycle(enough::StopReason::Cancelled)
        );
        assert_eq!(
            SvgError::Stopped(enough::StopReason::TimedOut).category(),
            C::Lifecycle(enough::StopReason::TimedOut)
        );
    }

    #[test]
    #[cfg(feature = "optimize")]
    fn error_category_xml_write_and_io() {
        assert_eq!(
            SvgError::XmlWrite("x".into()).category(),
            C::Internal(InternalKind::Dependency)
        );
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "nope");
        assert!(matches!(SvgError::Io(io_err).category(), C::Io(_)));
    }

    /// `At<CodecError>` (built via the bridge) carries both the category and
    /// the codec name through erasure to `Box<dyn Error>` — the same recovery
    /// path a generic dyn-erased consumer uses.
    #[test]
    fn error_category_through_bridge_into_codec_error() {
        let err: At<zencodec::CodecError> = SvgError::NotSvg.into();
        assert_eq!(
            err.category(),
            C::Image(ImageError::Unsupported(UnsupportedImageKind::Type))
        );
        assert_eq!(err.error().codec(), Some("zensvg"));

        let boxed: Box<dyn core::error::Error + Send + Sync> = Box::new(err);
        let recovered = boxed.downcast_ref::<At<zencodec::CodecError>>().unwrap();
        assert_eq!(
            recovered.category(),
            C::Image(ImageError::Unsupported(UnsupportedImageKind::Type))
        );
        assert_eq!(recovered.error().codec(), Some("zensvg"));
    }
}
