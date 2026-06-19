//! TIFF encoding.

use alloc::vec::Vec;
use enough::Stop;
use whereat::{ResultAtExt, at};
use zenpixels::{ChannelLayout, ChannelType, PixelDescriptor, PixelSlice};

use crate::error::{Result, TiffError};

/// Lowered, format-agnostic metadata to embed in the encoded TIFF.
///
/// All fields are already resolved (color-emit policy applied, EXIF filtered):
/// the encoder writes them verbatim, deciding only the correct IFD/tag for each.
/// Kept free of any `zencodec` types so the core encoder stays feature-agnostic;
/// the `codec` module lowers `zencodec::Metadata` into this.
#[derive(Clone, Debug, Default)]
#[cfg(feature = "zencodec")]
pub(crate) struct TiffEncodeMeta {
    /// ICC profile bytes → IFD0 tag 34675.
    pub icc: Option<Vec<u8>>,
    /// XMP packet bytes → IFD0 tag 700.
    pub xmp: Option<Vec<u8>>,
    /// Embedded EXIF blob (a standalone little/big-endian TIFF: a header → IFD0
    /// → optional EXIF sub-IFD via tag 0x8769 → optional GPS sub-IFD via tag
    /// 0x8825). It is *decomposed* and routed into the output TIFF's native IFDs:
    /// IFD0 descriptive tags (Make/Model/Copyright/DateTime/…) → output IFD0;
    /// the EXIF sub-IFD's tags → native EXIF sub-IFD (tag 34665); the GPS
    /// sub-IFD's tags → native GPS sub-IFD (tag 34853). Baseline/structural tags
    /// that the encoder sets itself (orientation, resolution, strip layout) and
    /// the IFD-pointer tags (0x8769/0x8825/0xA005) are reconstructed, not copied.
    pub exif: Option<Vec<u8>>,
    /// EXIF orientation value (1-8) → IFD0 tag 274.
    pub orientation: Option<u16>,
}

#[cfg(feature = "zencodec")]
impl TiffEncodeMeta {
    /// Whether any metadata field is present (so the metadata-aware encode path
    /// is needed at all).
    fn is_empty(&self) -> bool {
        self.icc.is_none()
            && self.xmp.is_none()
            && self.exif.is_none()
            && self.orientation.is_none()
    }
}

/// Compression method for TIFF encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Compression {
    /// No compression.
    #[default]
    Uncompressed,
    /// LZW compression (requires `lzw` feature).
    Lzw,
    /// DEFLATE/zlib compression (requires `deflate` feature).
    Deflate,
    /// PackBits run-length encoding.
    PackBits,
}

impl Compression {
    #[track_caller]
    fn to_tiff(self) -> Result<tiff::encoder::Compression> {
        match self {
            Self::Uncompressed => Ok(tiff::encoder::Compression::Uncompressed),
            #[cfg(feature = "lzw")]
            Self::Lzw => Ok(tiff::encoder::Compression::Lzw),
            #[cfg(not(feature = "lzw"))]
            Self::Lzw => Err(at!(TiffError::Unsupported(
                "LZW compression requires the `lzw` feature".into(),
            ))),
            #[cfg(feature = "deflate")]
            Self::Deflate => Ok(tiff::encoder::Compression::Deflate(
                tiff::encoder::DeflateLevel::Balanced,
            )),
            #[cfg(not(feature = "deflate"))]
            Self::Deflate => Err(at!(TiffError::Unsupported(
                "Deflate compression requires the `deflate` feature".into(),
            ))),
            Self::PackBits => Ok(tiff::encoder::Compression::Packbits),
        }
    }
}

/// Predictor for TIFF encoding.
///
/// Predictors simplify pixel data before compression, improving ratios.
/// Horizontal differencing works well with LZW (~35% improvement).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Predictor {
    /// No prediction.
    #[default]
    None,
    /// Horizontal differencing (each sample stores the difference from the previous).
    Horizontal,
}

impl Predictor {
    fn to_tiff(self) -> tiff::encoder::Predictor {
        match self {
            Self::None => tiff::encoder::Predictor::None,
            Self::Horizontal => tiff::encoder::Predictor::Horizontal,
        }
    }
}

/// Encode configuration for TIFF operations.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TiffEncodeConfig {
    /// Compression method.
    pub compression: Compression,
    /// Predictor (improves compression ratio).
    pub predictor: Predictor,
    /// Use BigTIFF format (64-bit offsets, supports >4GB files).
    pub big_tiff: bool,
}

impl TiffEncodeConfig {
    /// Create a config with LZW + horizontal prediction (good default).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set compression method.
    #[must_use]
    pub fn with_compression(mut self, compression: Compression) -> Self {
        self.compression = compression;
        self
    }

    /// Set predictor.
    #[must_use]
    pub fn with_predictor(mut self, predictor: Predictor) -> Self {
        self.predictor = predictor;
        self
    }

    /// Enable BigTIFF format for files >4GB.
    #[must_use]
    pub fn with_big_tiff(mut self, big: bool) -> Self {
        self.big_tiff = big;
        self
    }
}

impl Default for TiffEncodeConfig {
    fn default() -> Self {
        Self {
            compression: Compression::Lzw,
            predictor: Predictor::Horizontal,
            big_tiff: false,
        }
    }
}

/// The TIFF predictor to actually use for `desc`, given the requested one.
///
/// `GrayAlpha` is written as a Gray colortype plus one `ExtraSamples` alpha
/// channel (2 samples/pixel). In `tiff 0.11.3` the *encoder* derives the
/// horizontal-predictor stride from the base colortype's sample count (1, for
/// Gray) and so predicts every byte against its immediate neighbour, while the
/// *decoder* reverses prediction with the full per-pixel sample count (2). That
/// stride mismatch corrupts the round-trip, so horizontal prediction is force-
/// disabled for `GrayAlpha`. The predictor is only a compression optimisation,
/// not a correctness feature, and the Gray + `ExtraSamples` form is already
/// half the samples of the old RGBA widening regardless. All other layouts keep
/// the requested predictor.
fn effective_predictor(
    desc: &PixelDescriptor,
    requested: tiff::encoder::Predictor,
) -> tiff::encoder::Predictor {
    if desc.layout() == ChannelLayout::GrayAlpha {
        tiff::encoder::Predictor::None
    } else {
        requested
    }
}

/// Encode a PixelBuffer to TIFF bytes.
///
/// Supports Gray, GrayAlpha, RGB, RGBA in u8, u16, and f32 channel types.
///
/// The `cancel` signal is checked before encoding; pass `&Unstoppable` when
/// cancellation is not needed.
#[track_caller]
pub fn encode(
    pixels: &PixelSlice<'_>,
    config: &TiffEncodeConfig,
    cancel: &dyn Stop,
) -> Result<Vec<u8>> {
    cancel.check().map_err(|e| at!(TiffError::from(e)))?;

    let desc = pixels.descriptor();
    let width = pixels.width();
    let height = pixels.rows();
    let data = pixels.contiguous_bytes();

    let compression = config.compression.to_tiff()?;
    let predictor = effective_predictor(&desc, config.predictor.to_tiff());

    let mut buf = std::io::Cursor::new(Vec::new());

    if config.big_tiff {
        let enc =
            tiff::encoder::TiffEncoder::new_big(&mut buf).map_err(|e| at!(TiffError::from(e)))?;
        let mut enc = enc.with_compression(compression).with_predictor(predictor);
        write_image(&mut enc, width, height, &desc, &data).at()?;
    } else {
        let enc = tiff::encoder::TiffEncoder::new(&mut buf).map_err(|e| at!(TiffError::from(e)))?;
        let mut enc = enc.with_compression(compression).with_predictor(predictor);
        write_image(&mut enc, width, height, &desc, &data).at()?;
    }

    Ok(buf.into_inner())
}

/// Encode a `PixelSlice` to TIFF, embedding the given metadata.
///
/// Like [`encode`], but writes ICC (tag 34675), XMP (tag 700), orientation
/// (tag 274 in IFD0), and a native EXIF sub-IFD (tag 34665) when present. With
/// empty metadata this is byte-identical to [`encode`].
#[track_caller]
#[cfg(feature = "zencodec")]
pub(crate) fn encode_with_meta(
    pixels: &PixelSlice<'_>,
    config: &TiffEncodeConfig,
    meta: &TiffEncodeMeta,
    cancel: &dyn Stop,
) -> Result<Vec<u8>> {
    if meta.is_empty() {
        return encode(pixels, config, cancel);
    }

    cancel.check().map_err(|e| at!(TiffError::from(e)))?;

    let desc = pixels.descriptor();
    let width = pixels.width();
    let height = pixels.rows();
    let data = pixels.contiguous_bytes();

    let compression = config.compression.to_tiff()?;
    let predictor = effective_predictor(&desc, config.predictor.to_tiff());

    let mut buf = std::io::Cursor::new(Vec::new());

    if config.big_tiff {
        let enc =
            tiff::encoder::TiffEncoder::new_big(&mut buf).map_err(|e| at!(TiffError::from(e)))?;
        let mut enc = enc.with_compression(compression).with_predictor(predictor);
        write_image_with_meta(&mut enc, width, height, &desc, &data, meta).at()?;
    } else {
        let enc = tiff::encoder::TiffEncoder::new(&mut buf).map_err(|e| at!(TiffError::from(e)))?;
        let mut enc = enc.with_compression(compression).with_predictor(predictor);
        write_image_with_meta(&mut enc, width, height, &desc, &data, meta).at()?;
    }

    Ok(buf.into_inner())
}

/// Encode a PixelSlice to TIFF, appending to the provided output buffer.
#[track_caller]
pub fn encode_into(
    pixels: &PixelSlice<'_>,
    config: &TiffEncodeConfig,
    cancel: &dyn Stop,
    output: &mut Vec<u8>,
) -> Result<()> {
    let encoded = encode(pixels, config, cancel).at()?;
    output.extend_from_slice(&encoded);
    Ok(())
}

/// Cast `&[u8]` to `&[u16]`, copying to an aligned buffer if needed.
#[track_caller]
fn as_u16_slice(data: &[u8]) -> Result<std::borrow::Cow<'_, [u16]>> {
    use std::borrow::Cow;
    match bytemuck::try_cast_slice(data) {
        Ok(s) => Ok(Cow::Borrowed(s)),
        Err(bytemuck::PodCastError::TargetAlignmentGreaterAndInputNotAligned) => Ok(Cow::Owned(
            data.chunks_exact(2)
                .map(|c| u16::from_ne_bytes([c[0], c[1]]))
                .collect(),
        )),
        Err(e) => Err(at!(TiffError::InvalidInput(alloc::format!(
            "cannot cast pixel data to &[u16]: {e:?}"
        )))),
    }
}

/// Cast `&[u8]` to `&[f32]`, copying to an aligned buffer if needed.
#[track_caller]
fn as_f32_slice(data: &[u8]) -> Result<std::borrow::Cow<'_, [f32]>> {
    use std::borrow::Cow;
    match bytemuck::try_cast_slice(data) {
        Ok(s) => Ok(Cow::Borrowed(s)),
        Err(bytemuck::PodCastError::TargetAlignmentGreaterAndInputNotAligned) => Ok(Cow::Owned(
            data.chunks_exact(4)
                .map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
        )),
        Err(e) => Err(at!(TiffError::InvalidInput(alloc::format!(
            "cannot cast pixel data to &[f32]: {e:?}"
        )))),
    }
}

/// Write the image using the appropriate tiff encoder colortype.
#[track_caller]
fn write_image<W: std::io::Write + std::io::Seek, K: tiff::encoder::TiffKind>(
    enc: &mut tiff::encoder::TiffEncoder<W, K>,
    width: u32,
    height: u32,
    desc: &PixelDescriptor,
    data: &[u8],
) -> Result<()> {
    use tiff::encoder::colortype;

    let layout = desc.layout();
    let ct = desc.channel_type();

    match (layout, ct) {
        // Gray
        (ChannelLayout::Gray, ChannelType::U8) => {
            enc.write_image::<colortype::Gray8>(width, height, data)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        (ChannelLayout::Gray, ChannelType::U16) => {
            let samples = as_u16_slice(data)?;
            enc.write_image::<colortype::Gray16>(width, height, &samples)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        (ChannelLayout::Gray, ChannelType::F32) => {
            let samples = as_f32_slice(data)?;
            enc.write_image::<colortype::Gray32Float>(width, height, &samples)
                .map_err(|e| at!(TiffError::from(e)))?;
        }

        // GrayAlpha — the tiff crate has no dedicated gray+alpha colortype, but
        // a 2-samples-per-pixel image is expressed by writing the Gray colortype
        // and adding one `ExtraSamples = UnassociatedAlpha` channel. The
        // interleaved (gray, alpha) data is written verbatim — no widening to
        // RGBA, so the file carries 2 samples/pixel instead of 4.
        (ChannelLayout::GrayAlpha, ChannelType::U8) => {
            write_graya_image::<_, _, colortype::Gray8>(enc, width, height, data)?;
        }
        (ChannelLayout::GrayAlpha, ChannelType::U16) => {
            let samples = as_u16_slice(data)?;
            write_graya_image::<_, _, colortype::Gray16>(enc, width, height, &samples)?;
        }
        (ChannelLayout::GrayAlpha, ChannelType::F32) => {
            let samples = as_f32_slice(data)?;
            write_graya_image::<_, _, colortype::Gray32Float>(enc, width, height, &samples)?;
        }

        // RGB
        (ChannelLayout::Rgb, ChannelType::U8) => {
            enc.write_image::<colortype::RGB8>(width, height, data)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        (ChannelLayout::Rgb, ChannelType::U16) => {
            let samples = as_u16_slice(data)?;
            enc.write_image::<colortype::RGB16>(width, height, &samples)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        (ChannelLayout::Rgb, ChannelType::F32) => {
            let samples = as_f32_slice(data)?;
            enc.write_image::<colortype::RGB32Float>(width, height, &samples)
                .map_err(|e| at!(TiffError::from(e)))?;
        }

        // RGBA
        (ChannelLayout::Rgba, ChannelType::U8) => {
            enc.write_image::<colortype::RGBA8>(width, height, data)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        (ChannelLayout::Rgba, ChannelType::U16) => {
            let samples = as_u16_slice(data)?;
            enc.write_image::<colortype::RGBA16>(width, height, &samples)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        (ChannelLayout::Rgba, ChannelType::F32) => {
            let samples = as_f32_slice(data)?;
            enc.write_image::<colortype::RGBA32Float>(width, height, &samples)
                .map_err(|e| at!(TiffError::from(e)))?;
        }

        _ => {
            return Err(at!(TiffError::Unsupported(alloc::format!(
                "cannot encode {layout:?}/{ct:?} to TIFF"
            ))));
        }
    }

    Ok(())
}

/// Write a gray + alpha image as a Gray colortype with one
/// `ExtraSamples = UnassociatedAlpha` channel (2 samples/pixel).
///
/// `C` is the matching *single-channel* gray colortype (`Gray8`/`Gray16`/
/// `Gray32Float`); `samples` is the interleaved (gray, alpha) data, exactly
/// `width * height * 2` elements long. The data is written verbatim — no
/// widening to RGBA — so the file carries half the sample bytes of the old
/// RGBA-expansion path. `extra_samples` re-derives the per-row sample count and
/// rewrites `SamplesPerPixel`/`BitsPerSample` to 2 channels at finish time.
#[track_caller]
fn write_graya_image<W, K, C>(
    enc: &mut tiff::encoder::TiffEncoder<W, K>,
    width: u32,
    height: u32,
    samples: &[C::Inner],
) -> Result<()>
where
    W: std::io::Write + std::io::Seek,
    K: tiff::encoder::TiffKind,
    C: tiff::encoder::colortype::ColorType,
    [C::Inner]: tiff::encoder::TiffValue,
{
    use tiff::tags::ExtraSamples;

    let mut image = enc
        .new_image::<C>(width, height)
        .map_err(|e| at!(TiffError::from(e)))?;
    image
        .extra_samples(&[ExtraSamples::UnassociatedAlpha])
        .map_err(|e| at!(TiffError::from(e)))?;
    image
        .write_data(samples)
        .map_err(|e| at!(TiffError::from(e)))?;
    Ok(())
}

/// Resolved sub-IFD pointers + IFD0 entries produced by decomposing the EXIF
/// blob, ready to be written into the main image directory.
///
/// `exif_offset`/`gps_offset` are the file offsets of the already-written EXIF
/// (34665) / GPS (34853) sub-IFDs, recorded as IFD0 pointer tags (held as raw
/// `u64` and converted to the file's offset width at write time). `ifd0_entries`
/// are the blob's IFD0 descriptive tags (Make/Copyright/…) to write into IFD0.
#[cfg(feature = "zencodec")]
struct ResolvedSubIfds<'a> {
    exif_offset: Option<u64>,
    gps_offset: Option<u64>,
    ifd0_entries: &'a [ExifEntry],
}

/// Write a single image plus metadata tags using the `new_image` flow.
///
/// The EXIF and GPS sub-IFDs (if any) are written *first* — their offsets must
/// be known before the main IFD records the `ExifDirectory` (34665) / `GpsInfo`
/// (34853) pointers. The blob's IFD0 descriptive tags are written directly into
/// the main image directory (IFD0).
#[track_caller]
#[cfg(feature = "zencodec")]
fn write_image_with_meta<W: std::io::Write + std::io::Seek, K: tiff::encoder::TiffKind>(
    enc: &mut tiff::encoder::TiffEncoder<W, K>,
    width: u32,
    height: u32,
    desc: &PixelDescriptor,
    data: &[u8],
    meta: &TiffEncodeMeta,
) -> Result<()> {
    use tiff::encoder::colortype;

    // 1. Decompose the EXIF blob into IFD0 / EXIF / GPS entries, then write the
    //    EXIF and GPS sub-IFDs before the main image so their offsets are known.
    //    A malformed blob decomposes to all-empty (skipped, never fatal).
    let decomposed = meta
        .exif
        .as_deref()
        .map(decompose_exif_blob)
        .unwrap_or_default();
    let exif_offset = write_sub_ifd(enc, &decomposed.exif)?;
    let gps_offset = write_sub_ifd(enc, &decomposed.gps)?;
    let subs = ResolvedSubIfds {
        exif_offset,
        gps_offset,
        ifd0_entries: &decomposed.ifd0,
    };

    let layout = desc.layout();
    let ct = desc.channel_type();

    // 2. Match the pixel format to a tiff colortype, expanding GrayAlpha to RGBA
    //    (the tiff encoder has no GrayAlpha colortype) exactly like `write_image`.
    match (layout, ct) {
        // Gray
        (ChannelLayout::Gray, ChannelType::U8) => {
            write_one_image::<_, _, colortype::Gray8>(enc, width, height, data, meta, &subs)?;
        }
        (ChannelLayout::Gray, ChannelType::U16) => {
            let samples = as_u16_slice(data)?;
            write_one_image::<_, _, colortype::Gray16>(enc, width, height, &samples, meta, &subs)?;
        }
        (ChannelLayout::Gray, ChannelType::F32) => {
            let samples = as_f32_slice(data)?;
            write_one_image::<_, _, colortype::Gray32Float>(
                enc, width, height, &samples, meta, &subs,
            )?;
        }

        // GrayAlpha — Gray colortype + one `ExtraSamples = UnassociatedAlpha`
        // channel (2 samples/pixel), written verbatim. No RGBA widening.
        (ChannelLayout::GrayAlpha, ChannelType::U8) => {
            write_one_graya_image::<_, _, colortype::Gray8>(enc, width, height, data, meta, &subs)?;
        }
        (ChannelLayout::GrayAlpha, ChannelType::U16) => {
            let samples = as_u16_slice(data)?;
            write_one_graya_image::<_, _, colortype::Gray16>(
                enc, width, height, &samples, meta, &subs,
            )?;
        }
        (ChannelLayout::GrayAlpha, ChannelType::F32) => {
            let samples = as_f32_slice(data)?;
            write_one_graya_image::<_, _, colortype::Gray32Float>(
                enc, width, height, &samples, meta, &subs,
            )?;
        }

        // RGB
        (ChannelLayout::Rgb, ChannelType::U8) => {
            write_one_image::<_, _, colortype::RGB8>(enc, width, height, data, meta, &subs)?;
        }
        (ChannelLayout::Rgb, ChannelType::U16) => {
            let samples = as_u16_slice(data)?;
            write_one_image::<_, _, colortype::RGB16>(enc, width, height, &samples, meta, &subs)?;
        }
        (ChannelLayout::Rgb, ChannelType::F32) => {
            let samples = as_f32_slice(data)?;
            write_one_image::<_, _, colortype::RGB32Float>(
                enc, width, height, &samples, meta, &subs,
            )?;
        }

        // RGBA
        (ChannelLayout::Rgba, ChannelType::U8) => {
            write_one_image::<_, _, colortype::RGBA8>(enc, width, height, data, meta, &subs)?;
        }
        (ChannelLayout::Rgba, ChannelType::U16) => {
            let samples = as_u16_slice(data)?;
            write_one_image::<_, _, colortype::RGBA16>(enc, width, height, &samples, meta, &subs)?;
        }
        (ChannelLayout::Rgba, ChannelType::F32) => {
            let samples = as_f32_slice(data)?;
            write_one_image::<_, _, colortype::RGBA32Float>(
                enc, width, height, &samples, meta, &subs,
            )?;
        }

        _ => {
            return Err(at!(TiffError::Unsupported(alloc::format!(
                "cannot encode {layout:?}/{ct:?} to TIFF"
            ))));
        }
    }

    Ok(())
}

/// Write one image directory: tags first (ICC/XMP/orientation, the blob's IFD0
/// descriptive tags, and the EXIF/GPS sub-IFD pointers), then pixel data.
#[track_caller]
#[cfg(feature = "zencodec")]
fn write_one_image<W, K, C>(
    enc: &mut tiff::encoder::TiffEncoder<W, K>,
    width: u32,
    height: u32,
    samples: &[C::Inner],
    meta: &TiffEncodeMeta,
    subs: &ResolvedSubIfds<'_>,
) -> Result<()>
where
    W: std::io::Write + std::io::Seek,
    K: tiff::encoder::TiffKind,
    C: tiff::encoder::colortype::ColorType,
    [C::Inner]: tiff::encoder::TiffValue,
{
    use tiff::tags::Tag;

    let mut image = enc
        .new_image::<C>(width, height)
        .map_err(|e| at!(TiffError::from(e)))?;
    {
        let dir = image.encoder();

        if let Some(icc) = meta.icc.as_deref() {
            dir.write_tag(Tag::IccProfile, icc)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        if let Some(xmp) = meta.xmp.as_deref() {
            // XMP is carried in tag 700 as a byte (UNDEFINED/BYTE) array.
            dir.write_tag(Tag::Unknown(700), xmp)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        if let Some(orient) = meta.orientation
            && orient != 0
        {
            dir.write_tag(Tag::Orientation, orient)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        // The blob's IFD0 descriptive tags (Make/Model/Copyright/DateTime/…)
        // belong in the output IFD0, alongside the structural tags above.
        for entry in subs.ifd0_entries {
            write_exif_entry(dir, entry)?;
        }
        if let Some(off) = subs.exif_offset {
            let off = K::convert_offset(off).map_err(|e| at!(TiffError::from(e)))?;
            dir.write_tag(Tag::ExifDirectory, off)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        if let Some(off) = subs.gps_offset {
            let off = K::convert_offset(off).map_err(|e| at!(TiffError::from(e)))?;
            dir.write_tag(Tag::GpsDirectory, off)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
    }
    image
        .write_data(samples)
        .map_err(|e| at!(TiffError::from(e)))?;
    Ok(())
}

/// Like [`write_one_image`], but for gray + alpha: writes the Gray colortype
/// plus one `ExtraSamples = UnassociatedAlpha` channel (2 samples/pixel) so the
/// interleaved (gray, alpha) `samples` are stored verbatim, with no RGBA
/// widening. Metadata tags (ICC/XMP/orientation, the blob's IFD0 descriptive
/// tags, and the EXIF/GPS sub-IFD pointers) are written first, exactly as in
/// [`write_one_image`].
#[track_caller]
#[cfg(feature = "zencodec")]
fn write_one_graya_image<W, K, C>(
    enc: &mut tiff::encoder::TiffEncoder<W, K>,
    width: u32,
    height: u32,
    samples: &[C::Inner],
    meta: &TiffEncodeMeta,
    subs: &ResolvedSubIfds<'_>,
) -> Result<()>
where
    W: std::io::Write + std::io::Seek,
    K: tiff::encoder::TiffKind,
    C: tiff::encoder::colortype::ColorType,
    [C::Inner]: tiff::encoder::TiffValue,
{
    use tiff::tags::{ExtraSamples, Tag};

    let mut image = enc
        .new_image::<C>(width, height)
        .map_err(|e| at!(TiffError::from(e)))?;
    // `ExtraSamples` (and every other tag) is keyed into a `BTreeMap`, so the
    // IFD is emitted in ascending tag order regardless of call order here.
    image
        .extra_samples(&[ExtraSamples::UnassociatedAlpha])
        .map_err(|e| at!(TiffError::from(e)))?;
    {
        let dir = image.encoder();

        if let Some(icc) = meta.icc.as_deref() {
            dir.write_tag(Tag::IccProfile, icc)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        if let Some(xmp) = meta.xmp.as_deref() {
            dir.write_tag(Tag::Unknown(700), xmp)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        if let Some(orient) = meta.orientation
            && orient != 0
        {
            dir.write_tag(Tag::Orientation, orient)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        for entry in subs.ifd0_entries {
            write_exif_entry(dir, entry)?;
        }
        if let Some(off) = subs.exif_offset {
            let off = K::convert_offset(off).map_err(|e| at!(TiffError::from(e)))?;
            dir.write_tag(Tag::ExifDirectory, off)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        if let Some(off) = subs.gps_offset {
            let off = K::convert_offset(off).map_err(|e| at!(TiffError::from(e)))?;
            dir.write_tag(Tag::GpsDirectory, off)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
    }
    image
        .write_data(samples)
        .map_err(|e| at!(TiffError::from(e)))?;
    Ok(())
}

/// The EXIF blob decomposed into the output TIFF's native IFDs.
///
/// A `zencodec` EXIF blob is itself a mini-TIFF: a header → IFD0 (baseline +
/// descriptive camera tags) → an EXIF sub-IFD via IFD0 tag 0x8769 (the real EXIF
/// tags) → optionally a GPS sub-IFD via IFD0 tag 0x8825. We walk the whole tree
/// and route each level to where it belongs in the output TIFF:
/// - `ifd0`: descriptive IFD0 tags (Make/Model/Copyright/DateTime/…) → output IFD0
/// - `exif`: the EXIF sub-IFD's tags → native EXIF sub-IFD (tag 34665)
/// - `gps`: the GPS sub-IFD's tags → native GPS sub-IFD (tag 34853)
///
/// A TIFF-origin round-trip blob has its EXIF tags flattened directly into IFD0
/// (no 0x8769 pointer, as the decode side re-serializes only the EXIF sub-IFD);
/// those non-descriptive IFD0 tags are routed to `exif` so they still land in the
/// native EXIF sub-IFD. Structural/baseline tags and IFD-pointer tags are dropped
/// — they are reconstructed by the encoder, never copied.
#[cfg(feature = "zencodec")]
#[derive(Default)]
struct DecomposedExif {
    ifd0: Vec<ExifEntry>,
    exif: Vec<ExifEntry>,
    gps: Vec<ExifEntry>,
}

/// Decompose an embedded EXIF blob (a standalone little/big-endian TIFF) into the
/// output TIFF's native IFDs. A malformed blob decomposes to all-empty (skipped,
/// never fatal) — every read is endian-aware and bounds-checked.
#[cfg(feature = "zencodec")]
fn decompose_exif_blob(blob: &[u8]) -> DecomposedExif {
    let mut out = DecomposedExif::default();
    let Some((le, ifd0_off)) = parse_tiff_header(blob) else {
        return out;
    };
    let Some(ifd0) = parse_ifd_at(blob, le, ifd0_off) else {
        return out;
    };

    for entry in ifd0 {
        match entry.tag {
            // IFD-pointer tags: follow the offset into the sub-IFD. Their value
            // is a LONG/SHORT offset relative to the blob; the pointer itself is
            // reconstructed by the encoder, never copied verbatim.
            0x8769 => {
                if let Some(sub) = follow_ifd_pointer(blob, le, &entry) {
                    // `extend` (not assign) so any flattened IFD0 EXIF tags routed
                    // to `out.exif` by the catch-all below are preserved too.
                    out.exif.extend(sub);
                }
            }
            0x8825 => {
                if let Some(sub) = follow_ifd_pointer(blob, le, &entry) {
                    out.gps.extend(sub);
                }
            }
            // Interoperability pointer: no native carrier here, and its offset
            // is blob-relative — drop it (do not emit a dangling pointer).
            0xA005 => {}
            // Baseline/structural tags the encoder sets itself: drop.
            tag if is_structural_tag(tag) => {}
            // Descriptive IFD0 tags (camera identity, dates, rights, …) → IFD0.
            tag if is_ifd0_descriptive_tag(tag) => out.ifd0.push(entry),
            // Anything else in IFD0 is treated as flattened EXIF content (the
            // TIFF round-trip shape) and routed to the native EXIF sub-IFD.
            _ => out.exif.push(entry),
        }
    }
    out
}

/// Follow an IFD-pointer entry (0x8769 / 0x8825): read its value as the sub-IFD's
/// offset into the blob and parse that sub-IFD. Returns `None` if the offset is
/// missing, the type is unexpected, or the sub-IFD is malformed.
#[cfg(feature = "zencodec")]
fn follow_ifd_pointer(blob: &[u8], le: bool, entry: &ExifEntry) -> Option<Vec<ExifEntry>> {
    // Pointer value is a single LONG (4) or SHORT (3) offset.
    let off = match entry.field_type {
        4 if entry.bytes.len() >= 4 => {
            let a = [
                entry.bytes[0],
                entry.bytes[1],
                entry.bytes[2],
                entry.bytes[3],
            ];
            if le {
                u32::from_le_bytes(a)
            } else {
                u32::from_be_bytes(a)
            }
        }
        3 if entry.bytes.len() >= 2 => {
            let a = [entry.bytes[0], entry.bytes[1]];
            let v = if le {
                u16::from_le_bytes(a)
            } else {
                u16::from_be_bytes(a)
            };
            u32::from(v)
        }
        _ => return None,
    };
    parse_ifd_at(blob, le, off as usize)
}

/// One parsed EXIF IFD entry: tag id, TIFF field type, element count, the raw
/// value bytes (in the blob's byte order, exactly `type_size * count` long), and
/// whether that byte order is little-endian (so the writer can decode them).
#[cfg(feature = "zencodec")]
struct ExifEntry {
    tag: u16,
    field_type: u16,
    count: u32,
    /// Raw value bytes in source byte order (already gathered from inline-or-offset).
    bytes: Vec<u8>,
    /// Source byte order of `bytes`: `true` = little-endian.
    le: bool,
}

/// Byte size of a TIFF field type, or `None` for types we don't carry.
#[cfg(feature = "zencodec")]
fn tiff_type_size(field_type: u16) -> Option<usize> {
    Some(match field_type {
        1 | 2 | 6 | 7 => 1, // BYTE, ASCII, SBYTE, UNDEFINED
        3 | 8 => 2,         // SHORT, SSHORT
        4 | 9 | 11 => 4,    // LONG, SLONG, FLOAT
        5 | 10 | 12 => 8,   // RATIONAL, SRATIONAL, DOUBLE
        _ => return None,
    })
}

/// Validate the TIFF header of a standalone EXIF blob, returning
/// `(little_endian, ifd0_offset)`. `None` if the header is malformed.
#[cfg(feature = "zencodec")]
fn parse_tiff_header(blob: &[u8]) -> Option<(bool, usize)> {
    if blob.len() < 8 {
        return None;
    }
    let le = match &blob[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let magic = if le {
        u16::from_le_bytes([blob[2], blob[3]])
    } else {
        u16::from_be_bytes([blob[2], blob[3]])
    };
    if magic != 42 {
        return None;
    }
    let ifd0_off = if le {
        u32::from_le_bytes([blob[4], blob[5], blob[6], blob[7]])
    } else {
        u32::from_be_bytes([blob[4], blob[5], blob[6], blob[7]])
    };
    Some((le, ifd0_off as usize))
}

/// Parse the entries of a single IFD at `ifd_off` within a standalone EXIF/TIFF
/// blob, gathering each value from inline-or-offset storage. Same 12-byte entry
/// format for IFD0 and every sub-IFD. Returns `None` if the directory header is
/// out of bounds; individual unreadable/dangling entries are skipped.
#[cfg(feature = "zencodec")]
fn parse_ifd_at(blob: &[u8], le: bool, ifd_off: usize) -> Option<Vec<ExifEntry>> {
    let r16 = |b: &[u8]| -> u16 {
        let a = [b[0], b[1]];
        if le {
            u16::from_le_bytes(a)
        } else {
            u16::from_be_bytes(a)
        }
    };
    let r32 = |b: &[u8]| -> u32 {
        let a = [b[0], b[1], b[2], b[3]];
        if le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        }
    };

    let count_end = ifd_off.checked_add(2)?;
    if count_end > blob.len() {
        return None;
    }
    let n = r16(&blob[ifd_off..count_end]) as usize;
    let entries_end = count_end.checked_add(n.checked_mul(12)?)?;
    if entries_end > blob.len() {
        return None;
    }

    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let base = count_end + i * 12;
        let tag = r16(&blob[base..base + 2]);
        let field_type = r16(&blob[base + 2..base + 4]);
        let count = r32(&blob[base + 4..base + 8]);
        // IFD-pointer tags (0x8769/0x8825/0xA005) are LONG and so are kept here
        // like any other entry; the caller decides whether to follow them.
        let Some(tsize) = tiff_type_size(field_type) else {
            continue; // unknown type — skip this entry
        };
        let Some(total) = tsize.checked_mul(count as usize) else {
            continue;
        };
        let value_field = &blob[base + 8..base + 12];
        let bytes = if total <= 4 {
            value_field[..total].to_vec()
        } else {
            let off = r32(value_field) as usize;
            let Some(end) = off.checked_add(total) else {
                continue;
            };
            if end > blob.len() {
                continue; // dangling offset — skip rather than emit garbage
            }
            blob[off..end].to_vec()
        };
        out.push(ExifEntry {
            tag,
            field_type,
            count,
            bytes,
            le,
        });
    }
    Some(out)
}

/// Write a list of parsed EXIF entries into a fresh, unchained sub-IFD. Returns
/// the offset of the written sub-IFD (for an IFD0 pointer tag), or `None` if
/// nothing usable was written (so no dangling pointer is emitted).
#[track_caller]
#[cfg(feature = "zencodec")]
fn write_sub_ifd<W: std::io::Write + std::io::Seek, K: tiff::encoder::TiffKind>(
    enc: &mut tiff::encoder::TiffEncoder<W, K>,
    entries: &[ExifEntry],
) -> Result<Option<u64>> {
    if entries.is_empty() {
        return Ok(None);
    }

    let mut dir = enc.extra_directory().map_err(|e| at!(TiffError::from(e)))?;
    let mut wrote_any = false;
    for entry in entries {
        if write_exif_entry(&mut dir, entry)? {
            wrote_any = true;
        }
    }

    if !wrote_any {
        // Nothing usable was carried — finish the (empty) directory and don't
        // point at it. The flushed empty IFD is harmless and never referenced.
        let _ = dir.finish_with_offsets();
        return Ok(None);
    }

    let offset = dir
        .finish_with_offsets()
        .map_err(|e| at!(TiffError::from(e)))?;
    // `offset.offset: K::OffsetType` is `u32`/`u64`; widen to `u64` for storage,
    // re-narrowed via `K::convert_offset` when written as an IFD0 pointer tag.
    Ok(Some(offset.offset.into()))
}

/// Baseline image-structure tags: describe the blob's own (dummy) image, not
/// metadata content. The encoder sets the real ones from the actual image, so
/// these are dropped from the carried metadata.
#[cfg(feature = "zencodec")]
fn is_structural_tag(tag: u16) -> bool {
    matches!(
        tag,
        0x0100  // ImageWidth
            | 0x0101 // ImageLength
            | 0x0102 // BitsPerSample
            | 0x0103 // Compression
            | 0x0106 // PhotometricInterpretation
            | 0x0111 // StripOffsets
            | 0x0112 // Orientation (carried separately via meta.orientation)
            | 0x0115 // SamplesPerPixel
            | 0x0116 // RowsPerStrip
            | 0x0117 // StripByteCounts
            | 0x011A // XResolution
            | 0x011B // YResolution
            | 0x0128 // ResolutionUnit
            | 0x011C // PlanarConfiguration
    )
}

/// Whether a tag is a descriptive IFD0 (0th-IFD) tag that belongs in the output
/// IFD0 rather than the EXIF sub-IFD. These are the standard TIFF/EXIF-2.3 0th-IFD
/// descriptive tags (camera identity, dates, rights, free-text). Everything else
/// found in the blob's IFD0 is treated as flattened EXIF content (the round-trip
/// shape) and routed to the native EXIF sub-IFD.
#[cfg(feature = "zencodec")]
fn is_ifd0_descriptive_tag(tag: u16) -> bool {
    matches!(
        tag,
        0x010D  // DocumentName
            | 0x010E // ImageDescription
            | 0x010F // Make
            | 0x0110 // Model
            | 0x0131 // Software
            | 0x0132 // DateTime
            | 0x013B // Artist
            | 0x013C // HostComputer
            | 0x8298 // Copyright
            | 0x02BC // XMP packet (XML) — descriptive, IFD0-level
    )
}

/// Write one parsed EXIF entry into the sub-IFD directory, decoding its raw bytes
/// (source byte order) into typed values and writing them via `write_tag` (which
/// re-encodes to the output file's byte order). Returns `true` if a tag was
/// written, `false` if the type/count was unsupported or malformed.
#[track_caller]
#[cfg(feature = "zencodec")]
fn write_exif_entry<W: std::io::Write + std::io::Seek, K: tiff::encoder::TiffKind>(
    dir: &mut tiff::encoder::DirectoryEncoder<'_, W, K>,
    entry: &ExifEntry,
) -> Result<bool> {
    use tiff::encoder::{Rational, SRational};
    use tiff::tags::Tag;

    let tag = Tag::from_u16_exhaustive(entry.tag);
    let le = entry.le;
    let n = entry.count as usize;

    macro_rules! write_scalar_list {
        ($read:expr, $ty:ty) => {{
            let mut out: Vec<$ty> = Vec::with_capacity(n);
            let step = core::mem::size_of::<$ty>();
            if entry.bytes.len() < n * step {
                return Ok(false);
            }
            for k in 0..n {
                out.push($read(&entry.bytes[k * step..k * step + step]));
            }
            dir.write_tag(tag, &out[..])
                .map_err(|e| at!(TiffError::from(e)))?;
            true
        }};
    }

    let r_u16 = |b: &[u8]| {
        let a = [b[0], b[1]];
        if le {
            u16::from_le_bytes(a)
        } else {
            u16::from_be_bytes(a)
        }
    };
    let r_i16 = |b: &[u8]| r_u16(b) as i16;
    let r_u32 = |b: &[u8]| {
        let a = [b[0], b[1], b[2], b[3]];
        if le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        }
    };
    let r_i32 = |b: &[u8]| r_u32(b) as i32;
    let r_f32 = |b: &[u8]| f32::from_bits(r_u32(b));
    let r_f64 = |b: &[u8]| {
        let mut a = [0u8; 8];
        a.copy_from_slice(&b[..8]);
        if le {
            f64::from_le_bytes(a)
        } else {
            f64::from_be_bytes(a)
        }
    };

    let wrote = match entry.field_type {
        // BYTE (1) / UNDEFINED (7) → raw byte array.
        //
        // RESIDUAL: `tiff 0.11.3`'s `write_tag` has no raw-UNDEFINED constructor,
        // so an UNDEFINED entry is written with the BYTE (1) type code. The value
        // bytes are byte-for-byte identical; only the field-type code differs
        // (7 → 1). Fixing this would require an image-tiff change (out of scope:
        // zentiff uses crates.io `tiff = "0.11.3"`), so this divergence is left
        // documented rather than patched.
        1 | 7 => {
            if entry.bytes.len() < n {
                return Ok(false);
            }
            dir.write_tag(tag, &entry.bytes[..n])
                .map_err(|e| at!(TiffError::from(e)))?;
            true
        }
        // SBYTE.
        6 => {
            if entry.bytes.len() < n {
                return Ok(false);
            }
            let out: Vec<i8> = entry.bytes[..n].iter().map(|&b| b as i8).collect();
            dir.write_tag(tag, &out[..])
                .map_err(|e| at!(TiffError::from(e)))?;
            true
        }
        // ASCII → trim the trailing NUL(s) and write as a string.
        2 => {
            let s = ascii_from_bytes(&entry.bytes);
            dir.write_tag(tag, s.as_str())
                .map_err(|e| at!(TiffError::from(e)))?;
            true
        }
        3 => write_scalar_list!(r_u16, u16),
        8 => write_scalar_list!(r_i16, i16),
        4 => write_scalar_list!(r_u32, u32),
        9 => write_scalar_list!(r_i32, i32),
        11 => write_scalar_list!(r_f32, f32),
        12 => write_scalar_list!(r_f64, f64),
        // RATIONAL.
        5 => {
            if entry.bytes.len() < n * 8 {
                return Ok(false);
            }
            let out: Vec<Rational> = (0..n)
                .map(|k| Rational {
                    n: r_u32(&entry.bytes[k * 8..k * 8 + 4]),
                    d: r_u32(&entry.bytes[k * 8 + 4..k * 8 + 8]),
                })
                .collect();
            dir.write_tag(tag, &out[..])
                .map_err(|e| at!(TiffError::from(e)))?;
            true
        }
        // SRATIONAL.
        10 => {
            if entry.bytes.len() < n * 8 {
                return Ok(false);
            }
            let out: Vec<SRational> = (0..n)
                .map(|k| SRational {
                    n: r_i32(&entry.bytes[k * 8..k * 8 + 4]),
                    d: r_i32(&entry.bytes[k * 8 + 4..k * 8 + 8]),
                })
                .collect();
            dir.write_tag(tag, &out[..])
                .map_err(|e| at!(TiffError::from(e)))?;
            true
        }
        _ => false,
    };
    Ok(wrote)
}

/// Convert ASCII tag bytes (NUL-terminated, possibly NUL-padded) into a `String`,
/// dropping a single trailing NUL and any interior NULs (defensive — EXIF ASCII
/// is single-string).
#[cfg(feature = "zencodec")]
fn ascii_from_bytes(bytes: &[u8]) -> alloc::string::String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    alloc::string::String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = TiffEncodeConfig::default();
        assert_eq!(config.compression, Compression::Lzw);
        assert_eq!(config.predictor, Predictor::Horizontal);
        assert!(!config.big_tiff);
    }

    #[test]
    fn builder_chain() {
        let config = TiffEncodeConfig::new()
            .with_compression(Compression::Deflate)
            .with_predictor(Predictor::None)
            .with_big_tiff(true);
        assert_eq!(config.compression, Compression::Deflate);
        assert_eq!(config.predictor, Predictor::None);
        assert!(config.big_tiff);
    }
}
