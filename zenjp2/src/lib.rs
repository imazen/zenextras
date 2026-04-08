//! JPEG 2000 decoder for the zen codec ecosystem.
//!
//! Wraps [`hayro_jpeg2000`] to provide JPEG 2000 (JP2 container and raw J2K
//! codestream) decoding with [`zencodec`] trait integration.
//!
//! # Features
//!
//! - `zencodec` (default) — enables `DecoderConfig`/`DecodeJob`/`Decode` trait
//!   implementations for use in zen codec pipelines.
//! - `std` (default) — enables standard library support.

#![forbid(unsafe_code)]

extern crate alloc;

pub mod error;

#[cfg(feature = "zencodec")]
pub mod codec;

pub use error::{Jp2Error, Result};
pub use hayro_jpeg2000::{ColorSpace, DecodeSettings, Image};

#[cfg(feature = "zencodec")]
pub use codec::{Jp2DecodeJob, Jp2Decoder, Jp2DecoderConfig};

whereat::define_at_crate_info!();

/// Detect whether data starts with JP2 container or J2K codestream magic bytes.
pub fn is_jpeg2000(data: &[u8]) -> bool {
    // JP2 container: 00 00 00 0C 6A 50 20 20
    let jp2 = data.len() >= 8
        && data[..4] == [0x00, 0x00, 0x00, 0x0C]
        && data[4..8] == [0x6A, 0x50, 0x20, 0x20];
    // Raw J2K codestream: FF 4F FF 51 (SOC + SIZ)
    let j2k =
        data.len() >= 4 && data[0] == 0xFF && data[1] == 0x4F && data[2] == 0xFF && data[3] == 0x51;
    jp2 || j2k
}
