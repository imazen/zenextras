//! OpenEXR decoding into zen pixel buffers, backed by the Rust `exr` crate.
//!
//! One flat RGB/RGBA part, at the largest resolution, without tone mapping,
//! exposure scaling, color conversion or alpha removal. See [`ExrDecoderConfig`]
//! for resource and cancellation semantics. The complete upstream [`Header`]
//! remains available to interpret chromaticities, windows and luminance units.
//!
//! ```no_run
//! use enough::Unstoppable;
//! use zenexr::ExrDecoderConfig;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let bytes = std::fs::read("reference.exr")?;
//! let image = ExrDecoderConfig::new().decode(&bytes, &Unstoppable)?;
//! let pixels = image.pixels();
//! let source_header = image.header();
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

use enough::{Stop, StopReason};
use exr::meta::header::Header;
use exr::prelude::{ReadChannels, ReadLayers, SampleType};
use std::cell::RefCell;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use whereat::{At, at};
use zenpixels::{AlphaMode, ColorPrimaries, PixelBuffer, PixelDescriptor};

whereat::define_at_crate_info!();

/// Errors from the EXR wrapper or its upstream reader.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExrError {
    /// The upstream reader rejected the image.
    #[error("EXR decode: {0}")]
    Decode(#[from] exr::error::Error),
    /// The file uses a layout outside the wrapper's declared surface.
    #[error("unsupported EXR: {0}")]
    Unsupported(&'static str),
    /// An input, output or pixel-count bound was exceeded.
    #[error("EXR limit exceeded: {0}")]
    LimitExceeded(&'static str),
    /// The fallible output allocation could not be satisfied.
    #[error("EXR output allocation failed")]
    Allocation,
    /// Cooperative cancellation was requested.
    #[error("EXR stopped: {0}")]
    Stopped(StopReason),
}

/// Located wrapper result.
pub type Result<T> = std::result::Result<T, At<ExrError>>;

/// Decoded data-window pixels and their unchanged OpenEXR header.
#[derive(Debug)]
pub struct ExrImage {
    pixels: PixelBuffer,
    header: Header,
}
impl ExrImage {
    /// Linear RGB/RGBA f32. Query the pixel buffer for dimensions and row stride.
    /// Negative and supernormal values are preserved; alpha remains associated.
    pub fn pixels(&self) -> &PixelBuffer {
        &self.pixels
    }
    /// Source metadata, including the data-window origin and display window.
    pub fn header(&self) -> &Header {
        &self.header
    }
    /// Take the pixel buffer without copying. Read needed metadata first.
    pub fn into_pixels(self) -> PixelBuffer {
        self.pixels
    }
}

/// Reusable single-part RGB/RGBA reader.
///
/// Defaults: 120 MP, 1 GiB input, 1 GiB output. Limits do not bound upstream
/// metadata/decompression scratch or total RSS. The `exr` decoder runs serially;
/// stop checks occur at I/O boundaries and before/after decode, not inside an
/// active decompression block. No conversion of color or luminance units occurs.
#[derive(Clone, Debug)]
pub struct ExrDecoderConfig {
    max_pixels: u64,
    max_input_bytes: u64,
    max_output_bytes: u64,
}
impl Default for ExrDecoderConfig {
    fn default() -> Self {
        Self::new()
    }
}
impl ExrDecoderConfig {
    /// Create a reader with the documented default bounds.
    pub fn new() -> Self {
        Self {
            max_pixels: 120_000_000,
            max_input_bytes: 1 << 30,
            max_output_bytes: 1 << 30,
        }
    }
    /// Set the maximum data-window pixel count.
    pub fn with_max_pixels(mut self, maximum: u64) -> Self {
        self.max_pixels = maximum;
        self
    }
    /// Set the maximum encoded byte count, checked before parsing metadata.
    pub fn with_max_input_bytes(mut self, maximum: u64) -> Self {
        self.max_input_bytes = maximum;
        self
    }
    /// Set the maximum output pixel bytes, checked before output allocation.
    /// This is not a cap on upstream scratch or total resident memory.
    pub fn with_max_output_bytes(mut self, maximum: u64) -> Self {
        self.max_output_bytes = maximum;
        self
    }

    /// Read and validate this wrapper's layout/size contract without decoding
    /// pixel blocks. Compression support and pixel integrity are checked by decode.
    pub fn probe(&self, data: &[u8], stop: &dyn Stop) -> Result<Header> {
        let chunks = self.open(data, stop)?;
        self.layout(chunks.headers())?;
        check_stop(stop)?;
        Ok(chunks.into_meta_data().headers.remove(0))
    }

    /// Decode the largest resolution of a single flat RGB/RGBA part. All format
    /// parsing and compression handling are delegated to `exr`; errors propagate.
    pub fn decode(&self, data: &[u8], stop: &dyn Stop) -> Result<ExrImage> {
        let chunks = self.open(data, stop)?;
        let (width, height, channels, _bytes) = self.layout(chunks.headers())?;
        let header = chunks.headers()[0].clone();
        let primaries = if header.shared_attributes.chromaticities.is_none() {
            ColorPrimaries::Bt709
        } else {
            ColorPrimaries::Unknown
        };
        let descriptor = if channels == 4 {
            PixelDescriptor::RGBAF32_LINEAR.with_alpha_mode(Some(AlphaMode::Premultiplied))
        } else {
            PixelDescriptor::RGBF32_LINEAR
        };
        let pixels = PixelBuffer::try_new(width, height, descriptor.with_primaries(primaries))
            .map_err(|_| at!(ExrError::Allocation))?;
        let allocated = RefCell::new(Some(pixels));
        // The single validated part creates one largest-resolution collector.
        // Supplying its already allocated storage keeps allocation fallible.
        let image = exr::image::read::read()
            .no_deep_data()
            .largest_resolution_level()
            .rgba_channels(
                |_, _| {
                    allocated
                        .borrow_mut()
                        .take()
                        .expect("single-part pixel collector")
                },
                move |pixels: &mut PixelBuffer,
                      position: exr::math::Vec2<usize>,
                      (r, g, b, a): (f32, f32, f32, f32)| {
                    let mut view = pixels.as_slice_mut();
                    let row = view.row_mut(position.y() as u32);
                    let offset = position.x() * channels * 4;
                    for (dst, value) in row[offset..offset + channels * 4]
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip([r, g, b, a])
                    {
                        dst.copy_from_slice(&value.to_ne_bytes());
                    }
                },
            )
            .first_valid_layer()
            .all_attributes()
            .pedantic()
            .non_parallel()
            .from_chunks(chunks)
            .map_err(|e| upstream_error(e, stop))?;
        check_stop(stop)?;
        let pixels = image.layer_data.channel_data.pixels;
        Ok(ExrImage { pixels, header })
    }

    fn open<'a>(
        &self,
        data: &'a [u8],
        stop: &'a dyn Stop,
    ) -> Result<exr::block::reader::Reader<StoppedReader<'a>>> {
        check_stop(stop)?;
        if data.len() as u64 > self.max_input_bytes {
            return Err(at!(ExrError::LimitExceeded("input bytes")));
        }
        exr::block::read(
            StoppedReader {
                cursor: Cursor::new(data),
                stop,
            },
            true,
        )
        .map_err(|e| upstream_error(e, stop))
    }
    fn layout(&self, headers: &[Header]) -> Result<(u32, u32, usize, usize)> {
        if headers.len() != 1 {
            return Err(at!(ExrError::Unsupported("exactly one part required")));
        }
        let header = &headers[0];
        if header.deep {
            return Err(at!(ExrError::Unsupported("deep samples")));
        }
        let width = u32::try_from(header.layer_size.width())
            .map_err(|_| at!(ExrError::LimitExceeded("width")))?;
        let height = u32::try_from(header.layer_size.height())
            .map_err(|_| at!(ExrError::LimitExceeded("height")))?;
        let count = u64::from(width) * u64::from(height);
        if count == 0 || count > self.max_pixels {
            return Err(at!(ExrError::LimitExceeded("pixel count")));
        }
        let mut seen = [false; 4];
        for channel in &header.channels.list {
            let lane = match channel.name.as_slice() {
                b"R" => 0,
                b"G" => 1,
                b"B" => 2,
                b"A" => 3,
                _ => {
                    return Err(at!(ExrError::Unsupported(
                        "only R,G,B and optional A channels"
                    )));
                }
            };
            if channel.sampling != exr::math::Vec2(1, 1) {
                return Err(at!(ExrError::Unsupported("subsampled channels")));
            }
            if channel.sample_type == SampleType::U32 {
                return Err(at!(ExrError::Unsupported(
                    "UINT channels require a lossless integer output contract"
                )));
            }
            if seen[lane] {
                return Err(at!(ExrError::Unsupported("duplicate channel")));
            }
            seen[lane] = true;
        }
        if !seen[..3].iter().all(|v| *v) {
            return Err(at!(ExrError::Unsupported("R,G,B channels required")));
        }
        let channels = if seen[3] { 4 } else { 3 };
        let bytes = count
            .checked_mul(channels as u64 * 4)
            .filter(|n| *n <= self.max_output_bytes)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| at!(ExrError::LimitExceeded("output bytes")))?;
        Ok((width, height, channels, bytes))
    }
}

fn check_stop(stop: &dyn Stop) -> Result<()> {
    stop.check().map_err(|e| at!(ExrError::Stopped(e)))
}
fn upstream_error(error: exr::error::Error, stop: &dyn Stop) -> At<ExrError> {
    match stop.check() {
        Err(reason) => at!(ExrError::Stopped(reason)),
        Ok(()) => at!(ExrError::Decode(error)),
    }
}
struct StoppedReader<'a> {
    cursor: Cursor<&'a [u8]>,
    stop: &'a dyn Stop,
}
impl Read for StoppedReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stop
            .check()
            .map_err(|e| io::Error::other(e.to_string()))?;
        self.cursor.read(buf)
    }
}
impl Seek for StoppedReader<'_> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.stop
            .check()
            .map_err(|e| io::Error::other(e.to_string()))?;
        self.cursor.seek(pos)
    }
}
