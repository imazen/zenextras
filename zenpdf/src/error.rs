use whereat::At;
use zenpixels::BufferError;

/// Errors from PDF rendering.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PdfError {
    /// The PDF bitstream is corrupt or otherwise could not be parsed. hayro's
    /// `LoadPdfError::Invalid` does not structurally distinguish a corrupt
    /// document from a truncated one, so both collapse here — this still
    /// categorizes correctly since `ErrorCategory::Image(_)` (not just the
    /// `UnexpectedEof` sub-variant) is the "incomplete input" bucket a
    /// truncation-conformance check tolerates.
    #[error("invalid PDF: could not parse document structure")]
    Malformed,

    /// The PDF is encrypted and this crate cannot decrypt it. zenpdf's public
    /// API has no password parameter, so any encrypted PDF hits this path
    /// regardless of hayro's specific `DecryptionError` reason (missing ID
    /// entry, wrong/missing password, invalid encryption dictionary, or an
    /// unsupported algorithm) — the bytes are a well-formed PDF using a
    /// feature (encryption) this crate does not implement.
    #[error("encrypted PDF not supported: {0:?}")]
    Encrypted(hayro::hayro_syntax::DecryptionError),

    /// Requested page index is out of range.
    #[error("page {index} out of range (document has {count} pages)")]
    PageOutOfRange { index: u32, count: u32 },

    /// Rendered dimensions exceed the u16 limit (65535 pixels) imposed by the
    /// underlying rendering backend's `RenderSettings`. This is a fixed
    /// structural ceiling of the rendering API, not a configurable resource
    /// policy — the caller can fix it by requesting a smaller scale/DPI/target
    /// size, so it categorizes as an invalid request parameter rather than a
    /// `Resource` limit.
    #[error("rendered dimensions {width}x{height} exceed u16 max (65535)")]
    DimensionOverflow { width: u32, height: u32 },

    /// The PDF's own page (MediaBox) has a zero, negative, or non-finite size
    /// — a property of the document itself, not of the caller's request.
    /// Distinct from [`InvalidRenderBounds`](Self::InvalidRenderBounds): here
    /// the page geometry itself is degenerate before any caller bounds are
    /// even applied.
    #[error("page {page} has invalid (zero/non-finite) source dimensions")]
    ZeroDimensions { page: u32 },

    /// A caller-supplied [`RenderBounds`](crate::render::RenderBounds) —
    /// combined with valid page dimensions — computed to a zero-size or
    /// non-finite target (e.g. `FitWidth(0)`, or a scale/DPI so extreme it
    /// over/underflows). Distinct from [`ZeroDimensions`](Self::ZeroDimensions):
    /// here the page itself is fine and the caller's request is the problem.
    #[error("render bounds produced zero-size output for page {page}")]
    InvalidRenderBounds { page: u32 },

    /// Too many pages requested (exceeds `RenderLimits::max_pages`).
    #[error("requested {requested} pages, limit is {limit}")]
    TooManyPages { requested: usize, limit: usize },

    /// Per-page pixel count exceeds `RenderLimits::max_pixels_per_page`.
    #[error("page would produce {pixels} pixels, limit is {limit}")]
    PixelLimitExceeded { pixels: u64, limit: u64 },

    /// Pixel buffer construction failed. The only call site
    /// (`pixmap_to_buffer`) passes already-validated, exactly-computed
    /// dimensions, so any failure here other than allocation exhaustion
    /// reflects a broken invariant in this crate's own size computation
    /// rather than a caller or image fault.
    #[error("pixel buffer error: {0}")]
    Buffer(#[from] At<BufferError>),

    /// Operation not supported by this codec.
    #[cfg(feature = "zencodec")]
    #[error("unsupported: {0}")]
    Unsupported(#[from] zencodec::UnsupportedOperation),

    /// Downstream sink error (from zencodec row sink).
    #[cfg(feature = "zencodec")]
    #[error("sink error: {0}")]
    Sink(#[source] zencodec::decode::SinkError),

    /// A resource limit was exceeded.
    #[cfg(feature = "zencodec")]
    #[error("limit exceeded: {0}")]
    LimitExceeded(#[from] zencodec::LimitExceeded),
}

pub type Result<T> = core::result::Result<T, PdfError>;

// Codec-agnostic error taxonomy (zencodec PR #116, two-level origin-first
// ErrorCategory: Image/Request/Resource/Policy/Stopped/Io/Internal). Maps
// every `PdfError` variant to exactly one category so consumers can route on
// it — HTTP status, retry policy, logging — without matching this enum
// directly. `zencodec` is an optional dependency, so this impl (and the
// bridge below) are gated on the feature.
#[cfg(feature = "zencodec")]
impl zencodec::CategorizedError for PdfError {
    fn codec_name(&self) -> Option<&'static str> {
        Some("zenpdf")
    }

    fn category(&self) -> zencodec::ErrorCategory {
        use zencodec::{
            ErrorCategory as C, ImageError as Img, InternalKind as Int, InvalidKind as Inv,
            LimitKind as L, RequestError as Req, ResourceError as Res,
            UnsupportedImageKind as UImg,
        };
        match self {
            // Corrupt / truncated bitstream — hayro doesn't structurally
            // distinguish the two (see the variant doc). `Image(_)` (not
            // specifically `UnexpectedEof`) is what a truncation-conformance
            // check treats as "incomplete input", so this still categorizes
            // correctly.
            PdfError::Malformed => C::Image(Img::Malformed),

            // A well-formed PDF using a feature (encryption) this crate
            // doesn't implement (no password plumbing on the public API).
            PdfError::Encrypted(_) => C::Image(Img::Unsupported(UImg::Feature)),

            // Bad caller parameter: page index, render bounds, or a computed
            // size overflowing the rendering backend's fixed u16 ceiling.
            // `DimensionOverflow` is a structural buffer-width limitation of
            // `hayro::RenderSettings` (`width`/`height`: `Option<u16>`), not a
            // configurable `ResourceLimits` cap — the caller fixes it by
            // requesting smaller bounds, so `Invalid(Parameters)` fits better
            // than `Resource::Limits` (there is no limit *policy* to report).
            PdfError::PageOutOfRange { .. } => C::Request(Req::Invalid(Inv::Parameters)),
            PdfError::DimensionOverflow { .. } => C::Request(Req::Invalid(Inv::Parameters)),
            PdfError::InvalidRenderBounds { .. } => C::Request(Req::Invalid(Inv::Parameters)),

            // The PDF's own page geometry is degenerate — image-bytes fault,
            // not a request fault (see the variant doc for the distinction
            // from `InvalidRenderBounds`).
            PdfError::ZeroDimensions { .. } => C::Image(Img::Malformed),

            // Configured resource ceilings (`RenderLimits`). PDF pages are
            // modeled as `ImageSequence::Multi` elsewhere in this crate, so
            // `Frames` is the closest available `LimitKind` for a page-count
            // cap; per-page pixel count maps directly to `Pixels`.
            PdfError::TooManyPages { .. } => C::Resource(Res::Limits(L::Frames)),
            PdfError::PixelLimitExceeded { .. } => C::Resource(Res::Limits(L::Pixels)),

            // `zenpixels::BufferError` doesn't implement `CategorizedError`
            // (zenpixels has no zencodec dependency), and the only call site
            // (`pixmap_to_buffer`) passes already-validated exact dimensions,
            // so match its inner variant directly: allocation exhaustion is a
            // real resource fault; everything else here would mean this
            // crate's own size computation is wrong (a bug, not a caller or
            // image fault).
            PdfError::Buffer(at) => match at.error() {
                BufferError::AllocationFailed => C::Resource(Res::OutOfMemory),
                _ => C::Internal(Int::Bug),
            },

            // Delegate to the wrapped zencodec cause types — they carry their
            // own `CategorizedError` impl.
            PdfError::Unsupported(op) => op.category(),
            PdfError::LimitExceeded(limit) => limit.category(),

            // The shared "output-sink" bucket — `ErrorCategory::Io`'s own docs
            // cover I/O *and* output-sink operations.
            PdfError::Sink(_) => C::Io(zencodec::CodecIoKind::opaque()),
        }
    }
}

/// Bridge a bare [`PdfError`] into the shared
/// [`CodecError`](zencodec::CodecError) envelope (Pattern B), so the zencodec
/// trait impls (`zencodec_impl.rs`) can return `At<zencodec::CodecError>` from
/// a `PdfError`-native call via `?`/`.into()` without every internal
/// `Result<_, PdfError>` in this crate needing to change shape.
/// `.start_at()` begins the location trace at the conversion point (the
/// zencodec trait-method boundary, since `render.rs`'s own direct API keeps
/// returning bare `PdfError` with no location tracking of its own).
#[cfg(feature = "zencodec")]
impl From<PdfError> for At<zencodec::CodecError> {
    #[track_caller]
    fn from(e: PdfError) -> Self {
        use whereat::ErrorAtExt;
        zencodec::CodecError::of(e.start_at())
    }
}

#[cfg(all(test, feature = "zencodec"))]
mod tests {
    use super::*;
    use zencodec::{CategorizedError, ErrorCategory as C, ImageError as Img, LimitKind as L};

    #[test]
    fn error_category_mapping() {
        assert_eq!(PdfError::Malformed.codec_name(), Some("zenpdf"));

        assert_eq!(PdfError::Malformed.category(), C::Image(Img::Malformed));
        assert_eq!(
            PdfError::PageOutOfRange { index: 5, count: 1 }.category(),
            C::Request(zencodec::RequestError::Invalid(
                zencodec::InvalidKind::Parameters
            ))
        );
        assert_eq!(
            PdfError::DimensionOverflow {
                width: 70000,
                height: 100
            }
            .category(),
            C::Request(zencodec::RequestError::Invalid(
                zencodec::InvalidKind::Parameters
            ))
        );
        assert_eq!(
            PdfError::ZeroDimensions { page: 0 }.category(),
            C::Image(Img::Malformed)
        );
        assert_eq!(
            PdfError::InvalidRenderBounds { page: 0 }.category(),
            C::Request(zencodec::RequestError::Invalid(
                zencodec::InvalidKind::Parameters
            ))
        );
        assert_eq!(
            PdfError::TooManyPages {
                requested: 10,
                limit: 5
            }
            .category(),
            C::Resource(zencodec::ResourceError::Limits(L::Frames))
        );
        assert_eq!(
            PdfError::PixelLimitExceeded {
                pixels: 9,
                limit: 4
            }
            .category(),
            C::Resource(zencodec::ResourceError::Limits(L::Pixels))
        );
    }

    #[test]
    fn error_from_unsupported_operation() {
        let op = zencodec::UnsupportedOperation::RowLevelDecode;
        let e: PdfError = op.into();
        assert!(matches!(e, PdfError::Unsupported(_)));
        assert_eq!(
            e.category(),
            C::Request(zencodec::RequestError::Unsupported(
                zencodec::UnsupportedOperation::RowLevelDecode
            ))
        );
    }

    #[test]
    fn error_from_limit_exceeded() {
        let limit = zencodec::LimitExceeded::Pixels { actual: 9, max: 4 };
        let e: PdfError = limit.into();
        assert!(matches!(e, PdfError::LimitExceeded(_)));
        assert_eq!(
            e.category(),
            C::Resource(zencodec::ResourceError::Limits(L::Pixels))
        );
    }

    #[test]
    fn sink_error_is_io_category() {
        let e = PdfError::Sink("boom".into());
        assert_eq!(e.category(), C::Io(zencodec::CodecIoKind::opaque()));
    }

    #[test]
    fn buffer_allocation_failure_is_out_of_memory() {
        use whereat::ErrorAtExt;
        let e = PdfError::Buffer(BufferError::AllocationFailed.start_at());
        assert_eq!(
            e.category(),
            C::Resource(zencodec::ResourceError::OutOfMemory)
        );
    }

    #[test]
    fn buffer_other_failure_is_internal_bug() {
        use whereat::ErrorAtExt;
        let e = PdfError::Buffer(BufferError::InvalidDimensions.start_at());
        assert_eq!(e.category(), C::Internal(zencodec::InternalKind::Bug));
    }

    #[test]
    fn category_bridges_into_codec_error_envelope() {
        use zencodec::CodecErrorExt;
        let located: whereat::At<zencodec::CodecError> = PdfError::Malformed.into();
        assert_eq!(located.error_category(), Some(C::Image(Img::Malformed)));
        assert_eq!(
            located.codec_error().and_then(zencodec::CodecError::codec),
            Some("zenpdf")
        );
    }
}
