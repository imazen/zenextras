//! Pure-Rust VP9 uncompressed header / Annex B superframe parser (`#![no_std]`)
//! and `zencodec-media` VP9 video decoder/encoder sessions.
//!
//! - [`header`] is always available, `#![no_std] + alloc`, zero `unsafe`, and
//!   parses VP9 uncompressed frame headers (profiles 0–3, 8/10/12-bit,
//!   4:2:0/4:2:2/4:4:4, full/studio range, color space, coded & render sizes,
//!   Annex B superframes, and canonical 12-byte ISOBMFF FullBox v1 `vpcC`).
//! - [`media`] (behind `features = ["rusty-vp9-std", "media"]`) provides
//!   [`media::Vp9VideoDecoder`], [`media::Vp9DecodeFactory`],
//!   [`media::Vp9VideoEncoder`], and [`media::Vp9EncodeFactory`] implementing
//!   `zencodec_media::session::{VideoDecoder, VideoEncoder}` and
//!   `zencodec_media::registry::{VideoDecodeFactory, VideoEncodeFactory}`.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod header;

#[cfg(all(feature = "rusty-vp9-std", feature = "media"))]
pub mod media;
