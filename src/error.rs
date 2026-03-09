//! TIFF error types.

use alloc::string::String;

/// Errors from TIFF encode/decode operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TiffError {
    /// TIFF decoding error from the underlying tiff crate.
    #[error("TIFF decode error: {0}")]
    Decode(String),

    /// TIFF encoding error from the underlying tiff crate.
    #[error("TIFF encode error: {0}")]
    Encode(String),

    /// Invalid input (dimensions, buffer size, pixel format, etc.).
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// Unsupported TIFF feature (color type, compression, etc.).
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// Resource limit exceeded.
    #[error("limit exceeded: {0}")]
    LimitExceeded(String),

    /// Operation stopped by cooperative cancellation.
    #[error("stopped: {0}")]
    Stopped(enough::StopReason),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(std::io::Error),

    /// Pixel buffer error.
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
            tiff::TiffError::UnsupportedError(ref ue) => {
                TiffError::Unsupported(alloc::format!("{ue}"))
            }
            tiff::TiffError::IoError(io) => TiffError::Io(io),
            tiff::TiffError::LimitsExceeded => {
                TiffError::LimitExceeded("tiff decoder limits exceeded".into())
            }
            tiff::TiffError::IntSizeError => {
                TiffError::LimitExceeded("image dimensions exceed platform limits".into())
            }
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

/// Result type alias for zentiff operations with location tracking.
pub type Result<T> = core::result::Result<T, whereat::At<TiffError>>;
