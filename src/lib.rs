#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

pub mod error;
pub mod format;
pub mod render;

#[cfg(feature = "zencodec")]
pub mod codec;

#[cfg(feature = "optimize")]
pub mod optimize;

// Re-export key types at crate root for convenience.
pub use error::SvgError;
pub use format::detect_svg;
pub use render::{FitMode, RenderOptions, RenderOutput, render, render_tree, svg_dimensions};

#[cfg(feature = "zencodec")]
pub use codec::{SvgDecodeJob, SvgDecoder, SvgDecoderConfig};

#[cfg(feature = "zencodec")]
pub use format::{SVG_FORMAT_DEFINITION, svg_format};

#[cfg(feature = "optimize")]
pub use optimize::{OptimizeOptions, optimize};
