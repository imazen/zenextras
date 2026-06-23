//! TIFF decoding and encoding with zenpixels integration.
//!
//! Wraps the [`tiff`] crate, providing a pixel-buffer-oriented API that
//! integrates with the zen* codec ecosystem.
//!
//! # Quick start
//!
//! ```no_run
//! use zentiff::{decode, probe, encode, TiffDecodeConfig, TiffEncodeConfig};
//! use enough::Unstoppable;
//!
//! // Decode
//! let data: &[u8] = &[]; // your TIFF bytes
//! let output = decode(data, &TiffDecodeConfig::default(), &Unstoppable)?;
//! println!("{}x{}", output.info.width, output.info.height);
//!
//! // Encode
//! let encoded = encode(&output.pixels.as_slice(), &TiffEncodeConfig::default(), &Unstoppable)?;
//! # Ok::<(), whereat::At<zentiff::TiffError>>(())
//! ```
//!
//! # Supported formats
//!
//! ## Decode
//!
//! All color types and sample depths supported by the `tiff` crate:
//! - Gray, GrayAlpha, RGB, RGBA in u8/u16/u32/u64/i8/i16/i32/i64/f16/f32/f64
//! - Palette (expanded to RGB8)
//! - CMYK/CMYKA (converted to RGBA)
//! - YCbCr, Lab (decoded as RGB)
//!
//! ## Encode
//!
//! - Gray, RGB, RGBA in u8/u16/f32
//! - GrayAlpha (expanded to RGBA for encoding)
//! - LZW, Deflate, PackBits, or uncompressed
//! - Horizontal prediction for improved compression
//! - Standard and BigTIFF formats

#![forbid(unsafe_code)]

extern crate alloc;
extern crate std;

// Crate info for whereat error tracing
whereat::define_at_crate_info!();

/// Allocation helpers honoring a per-site fallibility preference (the
/// crate-local mirror of `zencodec::AllocPreference`).
mod alloc_util;
#[cfg(feature = "zencodec")]
pub mod codec;
mod decode;
mod encode;
mod error;

/// Sweep-plan construction over the encoder knob space (variant-
/// generation playbook; see `zenjpeg/docs/VARIANT_GENERATION.md`).
/// The entire curated space is trial-class (lossless).
pub mod sweep;

//#[cfg(feature = "zennode")]
//pub mod zennode_defs;

pub use decode::{TiffDecodeConfig, TiffDecodeOutput, TiffInfo, decode, probe};
/// Cross-codec uniformity bundle (`__expert`-gated). Mirrors `zenjpeg`'s
/// `InternalParams` so external pipelines (calibration sweeps, picker
/// training) can drive every codec the same way. See
/// [`internal_params::InternalParams`] and
/// [`TiffEncodeConfig::with_internal_params`].
#[cfg(feature = "__expert")]
pub use encode::internal_params;
#[cfg(feature = "__expert")]
pub use encode::internal_params::InternalParams;
pub use encode::{Compression, Predictor, TiffEncodeConfig, encode, encode_into};
pub use error::TiffError;

/// Result type alias for zentiff operations.
pub type Result<T> = core::result::Result<T, whereat::At<TiffError>>;
