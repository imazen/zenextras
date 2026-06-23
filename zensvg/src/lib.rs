#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

/// Allocation helpers honoring a per-site fallibility preference (the
/// crate-local mirror of `zencodec::AllocPreference`). zensvg's raster is
/// allocated inside `tiny-skia`, so there is no crate-owned untrusted render
/// allocation to convert today; the module carries the boundary plumbing and
/// the tested 3-mode helpers for parity with the sibling codecs.
mod alloc_util;

pub mod error;
pub mod format;
pub mod render;

pub mod codec;

#[cfg(feature = "optimize")]
pub mod optimize;

// Re-export key types at crate root for convenience.
pub use error::SvgError;
pub use format::detect_svg;
pub use render::{FitMode, RenderOptions, RenderOutput, render, render_tree, svg_dimensions};

pub use codec::{SvgDecodeJob, SvgDecoder, SvgDecoderConfig};
pub use format::{SVG_FORMAT_DEFINITION, svg_format};

#[cfg(feature = "optimize")]
pub use optimize::{OptimizeOptions, optimize};
