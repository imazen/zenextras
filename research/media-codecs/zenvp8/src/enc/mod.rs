//! VP8 encoder (feature `encoder`) — ports of `vp8/encoder/` emission and
//! transform code, reusing the decoder's prediction/MC/IDCT machinery.
//!
//! Not compiled by default; see `encoder.rs` for the v1 scope constraints.

pub(crate) mod adapt;
pub(crate) mod boolw;
pub(crate) mod costs;
pub(crate) mod dct;
mod encoder;
pub(crate) mod lf;
pub(crate) mod metrics;
pub(crate) mod mv;
pub(crate) mod quant;
pub(crate) mod tokens;

pub use encoder::{EncodeError, EncoderConfig, Vp8Encoder};
