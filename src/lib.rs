#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

mod error;
mod render;
#[cfg(feature = "zencodec")]
mod zencodec_impl;

pub use error::{PdfError, Result};
pub use render::{
    PageSelection, PdfConfig, RenderBounds, RenderedPage, page_count, page_dimensions, render_page,
    render_pages,
};
#[cfg(feature = "zencodec")]
pub use zencodec_impl::{PDF_FORMAT, PdfDecoderConfig, PdfFullFrameDecoder};
