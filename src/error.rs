use core::fmt;

/// Errors that can occur during SVG operations.
#[derive(Debug)]
pub enum SvgError {
    /// SVG parsing failed.
    Parse(String),
    /// Rendering failed (e.g., zero-size image, allocation failure).
    Render(String),
    /// The input data is not valid SVG.
    NotSvg,
    /// Resource limit exceeded.
    LimitExceeded(String),
    /// An unsupported operation was requested.
    #[cfg(feature = "zencodec")]
    Unsupported(zencodec::UnsupportedOperation),
    /// I/O error (optimization, SVGZ compression).
    #[cfg(feature = "optimize")]
    Io(std::io::Error),
}

impl fmt::Display for SvgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(msg) => write!(f, "SVG parse error: {msg}"),
            Self::Render(msg) => write!(f, "SVG render error: {msg}"),
            Self::NotSvg => f.write_str("input is not valid SVG"),
            Self::LimitExceeded(msg) => write!(f, "resource limit exceeded: {msg}"),
            #[cfg(feature = "zencodec")]
            Self::Unsupported(op) => write!(f, "unsupported operation: {op}"),
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

#[cfg(feature = "zencodec")]
impl From<zencodec::UnsupportedOperation> for SvgError {
    fn from(op: zencodec::UnsupportedOperation) -> Self {
        Self::Unsupported(op)
    }
}

#[cfg(feature = "optimize")]
impl From<std::io::Error> for SvgError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<usvg::Error> for SvgError {
    fn from(e: usvg::Error) -> Self {
        Self::Parse(e.to_string())
    }
}
