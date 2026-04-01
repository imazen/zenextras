use zenpixels::BufferError;

/// Errors from PDF rendering.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PdfError {
    /// Failed to parse the PDF file.
    #[error("invalid PDF: {0}")]
    InvalidPdf(String),

    /// Requested page index is out of range.
    #[error("page {index} out of range (document has {count} pages)")]
    PageOutOfRange { index: u32, count: u32 },

    /// Rendered dimensions exceed the u16 limit (65535 pixels).
    #[error("rendered dimensions {width}x{height} exceed u16 max (65535)")]
    DimensionOverflow { width: u32, height: u32 },

    /// Zero or negative dimensions after applying render bounds.
    #[error("render bounds produced zero-size output for page {page}")]
    ZeroDimensions { page: u32 },

    /// Too many pages requested (exceeds `RenderLimits::max_pages`).
    #[error("requested {requested} pages, limit is {limit}")]
    TooManyPages { requested: usize, limit: usize },

    /// Per-page pixel count exceeds `RenderLimits::max_pixels_per_page`.
    #[error("page would produce {pixels} pixels, limit is {limit}")]
    PixelLimitExceeded { pixels: u64, limit: u64 },

    /// Pixel buffer construction failed.
    #[error("pixel buffer error: {0}")]
    Buffer(#[from] whereat::At<BufferError>),

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
