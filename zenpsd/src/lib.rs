//! Adobe Photoshop PSD (v1) and PSB (v2) merged-render decoder for the zen codec ecosystem.
//!
//! `zenpsd` decodes the embedded composite render in the PSD/PSB Image Data
//! section rather than compositing layers, while parsing the File Header, Color
//! Mode Data, and Image Resources (ICC profiles, EXIF, XMP, resolution info,
//! version info, and alpha channel metadata).
//!
//! # Features
//!
//! - `zencodec` (default) — enables `DecoderConfig`/`DecodeJob`/`Decode` trait
//!   implementations for use in zen codec pipelines.
//! - `inventory` — enables structural byte-range inventory via
//!   `DecodeJob::inventory`.
//! - `std` (default) — enables standard library support.

#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

extern crate alloc;

mod alloc_util;
pub mod error;
pub mod format;
pub mod parser;

#[cfg(feature = "inventory")]
mod inventory;

#[cfg(feature = "zencodec")]
pub mod codec;

pub use error::{PsdError, Result};
pub use format::detect_psd;
pub use parser::{
    BitDepth, ColorMode, Compression, DecodedPsd, PsdHeader, PsdInfo, PsdMetadata, ResolutionInfo,
    ResolutionUnit, Version, decode, decode_with_stop, probe,
};

#[cfg(feature = "zencodec")]
pub use codec::{PsdDecodeJob, PsdDecoder, PsdDecoderConfig};
#[cfg(feature = "zencodec")]
pub use format::{PSD_FORMAT_DEFINITION, psd_format};

whereat::define_at_crate_info!();
