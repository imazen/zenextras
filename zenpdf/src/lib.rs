#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

/// Allocation helpers honoring a per-site fallibility preference (the
/// crate-local mirror of `zencodec::AllocPreference`). zenpdf's raster is
/// produced inside `hayro`, so there is no crate-owned untrusted render
/// allocation to convert today; the module carries the boundary plumbing and
/// the tested 3-mode helpers for parity with the sibling codecs. Gated behind
/// `zencodec` since the preference only exists with the zencodec decode
/// boundary.
#[cfg(feature = "zencodec")]
mod alloc_util;

mod error;
mod render;
#[cfg(feature = "zencodec")]
mod zencodec_impl;

pub use error::{PdfError, Result};
pub use render::{
    PageSelection, PdfConfig, RenderBounds, RenderLimits, RenderedPage, page_count,
    page_dimensions, render_page, render_pages,
};
#[cfg(feature = "zencodec")]
pub use zencodec_impl::{PDF_FORMAT, PdfDecoderConfig};
