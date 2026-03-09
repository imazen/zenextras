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

mod decode;
mod encode;
mod error;

pub use decode::{TiffDecodeConfig, TiffDecodeOutput, TiffInfo, decode, probe};
pub use encode::{Compression, Predictor, TiffEncodeConfig, encode, encode_into};
pub use error::TiffError;

/// Result type alias for zentiff operations.
pub type Result<T> = core::result::Result<T, whereat::At<TiffError>>;
