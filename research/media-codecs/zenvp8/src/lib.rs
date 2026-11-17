//! zenvp8 — pure-Rust VP8 video codec.
//!
//! Decoder: push one VP8 frame payload per `push()`, pull displayed frames
//! with `next_frame()`. An experimental encoder (`Vp8Encoder`) is available
//! behind the `encoder` Cargo feature — it emits conformant VP8 streams that
//! this decoder, FFmpeg, and libvpx all decode byte-identically, but its
//! rate-distortion policy is minimal (see `PORTED-FROM.md`).
//!
//! Provenance:
//! - intra/keyframe core seeded from zenwebp `decoder/vp8v2` (bit-exact vs
//!   libwebp on stills).
//! - inter frame machinery ported from libvpx (BSD-3) at commit
//!   deaac25491db2edc430c2a71031109b65c23d1f1. See `../PORTED-FROM.md`.
//!
//! `#![forbid(unsafe_code)]` everywhere.

#![forbid(unsafe_code)]

mod boold;
mod decoder;
#[cfg(feature = "encoder")]
mod enc;
mod error;
mod framebuf;
mod header;
mod idct;
mod inter;
mod loopfilter;
mod mc;
mod predict;
mod tables;
mod tokens;
mod types;

pub use decoder::{DecodedFrame, Vp8Decoder};
#[cfg(feature = "encoder")]
pub use enc::{EncodeError, EncoderConfig, Vp8Encoder};
pub use error::DecodeError;

#[cfg(test)]
mod tests {
    #[test]
    fn it_compiles() {}
}
