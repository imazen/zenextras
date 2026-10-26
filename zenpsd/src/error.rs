//! Error types for PSD/PSB decoding.

use alloc::string::String;

/// Result type alias for PSD/PSB operations.
pub type Result<T> = core::result::Result<T, whereat::At<PsdError>>;

/// Errors that can occur during PSD/PSB parsing and decoding.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PsdError {
    /// Input does not begin with the `8BPS` magic signature.
    #[error("invalid PSD signature (expected '8BPS')")]
    InvalidSignature,

    /// Input ended before a required header, section, or scanline was complete.
    #[error("unexpected end of PSD input")]
    UnexpectedEof,

    /// Version field is neither 1 (PSD) nor 2 (PSB).
    #[error("unsupported PSD version: {0}")]
    UnsupportedVersion(u16),

    /// Bit depth is not one of 1, 8, 16, or 32, or is unsupported for the color mode.
    #[error("unsupported PSD bit depth: {0}")]
    UnsupportedBitDepth(u16),

    /// Color mode is not recognized or not supported for merged decoding.
    #[error("unsupported PSD color mode: {0}")]
    UnsupportedColorMode(u16),

    /// Image Data compression method is not one of 0 (Raw), 1 (RLE), 2 (ZIP), or 3 (ZIP+prediction).
    #[error("unsupported PSD compression method: {0}")]
    UnsupportedCompression(u16),

    /// File header dimensions or channel count are outside the specification bounds.
    #[error("invalid PSD dimensions ({width}x{height}, {channels} channels)")]
    InvalidDimensions {
        /// Width from header.
        width: u32,
        /// Height from header.
        height: u32,
        /// Channel count from header.
        channels: u16,
    },

    /// Structural or decompression defect in the PSD/PSB bytes.
    #[error("malformed PSD: {0}")]
    Malformed(&'static str),

    /// File explicitly records `hasRealMergedData = false` in `VersionInfo` (`0x0421`)
    /// and strict/real-merged validation is active.
    #[error("PSD file does not contain real merged composite data")]
    MissingMergedData,

    /// Internal pixel buffer construction failed.
    #[error("pixel buffer error: {0}")]
    PixelBuffer(String),

    /// Fallible allocation failed (out of memory).
    #[error("out of memory: {0}")]
    OutOfMemory(String),

    /// Cooperative cancellation was requested.
    #[error("PSD decode stopped: {0}")]
    Stopped(enough::StopReason),

    /// An unsupported operation was requested via `zencodec`.
    #[cfg(feature = "zencodec")]
    #[error(transparent)]
    UnsupportedOperation(#[from] zencodec::UnsupportedOperation),

    /// A [`zencodec::ResourceLimits`] cap was exceeded.
    #[cfg(feature = "zencodec")]
    #[error("{0}")]
    Limit(#[from] zencodec::LimitExceeded),

    /// A decode-row-sink error while pushing decoded output through
    /// [`zencodec::helpers::copy_decode_to_sink`].
    #[cfg(feature = "zencodec")]
    #[error("sink error: {0}")]
    Sink(zencodec::decode::SinkError),
}

impl From<enough::StopReason> for PsdError {
    fn from(reason: enough::StopReason) -> Self {
        Self::Stopped(reason)
    }
}

#[cfg(feature = "zencodec")]
impl zencodec::CategorizedError for PsdError {
    fn codec_name(&self) -> Option<&'static str> {
        Some("zenpsd")
    }

    fn category(&self) -> zencodec::ErrorCategory {
        use zencodec::{
            ErrorCategory as C, ImageError as I, InternalKind, ResourceError,
            UnsupportedImageKind as U,
        };

        match self {
            PsdError::UnexpectedEof => C::Image(I::UnexpectedEof),
            PsdError::UnsupportedVersion(_) => C::Image(I::Unsupported(U::Type)),
            PsdError::UnsupportedBitDepth(_)
            | PsdError::UnsupportedColorMode(_)
            | PsdError::UnsupportedCompression(_)
            | PsdError::MissingMergedData => C::Image(I::Unsupported(U::Feature)),
            PsdError::InvalidSignature
            | PsdError::InvalidDimensions { .. }
            | PsdError::Malformed(_) => C::Image(I::Malformed),
            PsdError::PixelBuffer(_) => C::Internal(InternalKind::Bug),
            PsdError::OutOfMemory(_) => C::Resource(ResourceError::OutOfMemory),
            PsdError::Stopped(reason) => C::from(*reason),
            PsdError::UnsupportedOperation(op) => op.category(),
            PsdError::Limit(limit) => limit.category(),
            PsdError::Sink(_) => C::Io(zencodec::CodecIoKind::opaque()),
        }
    }
}

#[cfg(feature = "zencodec")]
impl From<PsdError> for whereat::At<zencodec::CodecError> {
    #[track_caller]
    fn from(e: PsdError) -> Self {
        use whereat::ErrorAtExt;
        zencodec::CodecError::of(e.start_at())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_variants() {
        assert!(PsdError::InvalidSignature.to_string().contains("8BPS"));
        assert!(
            PsdError::UnexpectedEof
                .to_string()
                .contains("unexpected end")
        );
        assert!(PsdError::UnsupportedVersion(9).to_string().contains("9"));
        assert!(
            PsdError::Malformed("bad rle")
                .to_string()
                .contains("bad rle")
        );
    }

    #[cfg(feature = "zencodec")]
    #[test]
    fn error_categories_match_contract() {
        use zencodec::{
            CategorizedError, ErrorCategory as C, ImageError as I, InternalKind, ResourceError,
            UnsupportedImageKind as U,
        };

        assert_eq!(PsdError::InvalidSignature.codec_name(), Some("zenpsd"));
        assert_eq!(
            PsdError::UnexpectedEof.category(),
            C::Image(I::UnexpectedEof)
        );
        assert_eq!(
            PsdError::UnsupportedVersion(3).category(),
            C::Image(I::Unsupported(U::Type))
        );
        assert_eq!(
            PsdError::UnsupportedColorMode(9).category(),
            C::Image(I::Unsupported(U::Feature))
        );
        assert_eq!(
            PsdError::InvalidSignature.category(),
            C::Image(I::Malformed)
        );
        assert_eq!(
            PsdError::PixelBuffer("x".into()).category(),
            C::Internal(InternalKind::Bug)
        );
        assert_eq!(
            PsdError::OutOfMemory("x".into()).category(),
            C::Resource(ResourceError::OutOfMemory)
        );
    }
}
