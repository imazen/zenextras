//! Error types for JPEG 2000 decoding.

use alloc::string::String;

/// Result type alias for JPEG 2000 operations.
pub type Result<T> = core::result::Result<T, whereat::At<Jp2Error>>;

/// Errors that can occur during JPEG 2000 decoding.
#[derive(Debug, thiserror::Error)]
pub enum Jp2Error {
    /// The input data is not a valid JPEG 2000 file.
    #[error("invalid JPEG 2000 data: {0}")]
    InvalidData(String),

    /// The decoder encountered an unsupported feature.
    #[error("unsupported JPEG 2000 feature: {0}")]
    Unsupported(String),

    /// A resource limit was exceeded.
    #[error("limit exceeded: {0}")]
    LimitExceeded(String),

    /// An unsupported operation was requested.
    #[cfg(feature = "zencodec")]
    #[error(transparent)]
    UnsupportedOperation(#[from] zencodec::UnsupportedOperation),

    /// A zencodec resource limit was exceeded.
    #[cfg(feature = "zencodec")]
    #[error(transparent)]
    ZencodecLimit(#[from] zencodec::LimitExceeded),
}

impl From<hayro_jpeg2000::error::DecodeError> for Jp2Error {
    fn from(e: hayro_jpeg2000::error::DecodeError) -> Self {
        Jp2Error::InvalidData(alloc::format!("{e}"))
    }
}
