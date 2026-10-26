//! Core PSD (v1) and PSB (v2) header, resource, and merged-image parser.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

use enough::Stop;
use whereat::at;
use zenpixels::{PixelBuffer, PixelDescriptor};

use crate::alloc_util::{AllocPref, alloc_zeroed};
use crate::error::{PsdError, Result};

/// Maximum dimension allowed by the PSD (v1) specification.
pub const MAX_PSD_DIMENSION: u32 = 30_000;
/// Maximum dimension allowed by the PSB (v2, Large Document Format) specification.
pub const MAX_PSB_DIMENSION: u32 = 300_000;
/// Maximum channel count allowed by the PSD/PSB specification.
pub const MAX_CHANNELS: u16 = 56;

/// Document format version from the 26-byte File Header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Version {
    /// PSD (version 1): dimensions up to 30,000 × 30,000, 32-bit layer section length.
    Psd = 1,
    /// PSB (version 2, Large Document Format): dimensions up to 300,000 × 300,000, 64-bit layer section length.
    Psb = 2,
}

impl Version {
    /// Parse the version code from big-endian `u16`.
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            1 => Some(Self::Psd),
            2 => Some(Self::Psb),
            _ => None,
        }
    }

    /// Maximum width or height permitted by this version.
    pub fn max_dimension(self) -> u32 {
        match self {
            Self::Psd => MAX_PSD_DIMENSION,
            Self::Psb => MAX_PSB_DIMENSION,
        }
    }
}

/// Bits per channel sample in the PSD/PSB File Header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitDepth {
    /// 1-bit bitmap (`ColorMode::Bitmap`).
    One = 1,
    /// 8-bit unsigned integer per channel.
    Eight = 8,
    /// 16-bit big-endian unsigned integer per channel.
    Sixteen = 16,
    /// 32-bit big-endian IEEE-754 floating-point per channel.
    ThirtyTwo = 32,
}

impl BitDepth {
    /// Parse bit depth from big-endian `u16`.
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            1 => Some(Self::One),
            8 => Some(Self::Eight),
            16 => Some(Self::Sixteen),
            32 => Some(Self::ThirtyTwo),
            _ => None,
        }
    }

    /// Bits per sample (`1`, `8`, `16`, or `32`).
    pub fn bits(self) -> u8 {
        self as u8
    }

    /// Uncompressed bytes per scanline for one channel of `width` pixels.
    pub fn scanline_bytes(self, width: u32) -> Option<usize> {
        let w = width as usize;
        match self {
            Self::One => w.checked_add(7).map(|n| n / 8),
            Self::Eight => Some(w),
            Self::Sixteen => w.checked_mul(2),
            Self::ThirtyTwo => w.checked_mul(4),
        }
    }
}

/// Color mode from the PSD/PSB File Header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorMode {
    /// 1-bit monochrome bitmap (`0`).
    Bitmap = 0,
    /// Grayscale (`1`): 8, 16, or 32-bit.
    Grayscale = 1,
    /// Indexed palette (`2`): 8-bit with 768-byte RGB palette in Color Mode Data.
    Indexed = 2,
    /// RGB (`3`): 8, 16, or 32-bit.
    Rgb = 3,
    /// CMYK (`4`): 8 or 16-bit inverted ink percentages.
    Cmyk = 4,
    /// Multichannel (`7`).
    Multichannel = 7,
    /// Duotone (`8`): 8-bit, decoded as grayscale per the Adobe specification.
    Duotone = 8,
    /// CIE Lab (`9`).
    Lab = 9,
}

impl ColorMode {
    /// Parse the color mode from big-endian `u16`.
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            0 => Some(Self::Bitmap),
            1 => Some(Self::Grayscale),
            2 => Some(Self::Indexed),
            3 => Some(Self::Rgb),
            4 => Some(Self::Cmyk),
            7 => Some(Self::Multichannel),
            8 => Some(Self::Duotone),
            9 => Some(Self::Lab),
            _ => None,
        }
    }

    /// Number of base color channels before any alpha or spot channels.
    pub fn base_channels(self) -> Option<u16> {
        match self {
            Self::Bitmap | Self::Grayscale | Self::Indexed | Self::Duotone => Some(1),
            Self::Rgb => Some(3),
            Self::Cmyk => Some(4),
            Self::Multichannel | Self::Lab => None,
        }
    }

    /// Whether `(self, depth)` is a valid and supported combination for merged decoding.
    pub fn supports_depth(self, depth: BitDepth) -> bool {
        match self {
            Self::Bitmap => depth == BitDepth::One,
            Self::Grayscale | Self::Rgb => {
                matches!(
                    depth,
                    BitDepth::Eight | BitDepth::Sixteen | BitDepth::ThirtyTwo
                )
            }
            Self::Indexed | Self::Duotone => depth == BitDepth::Eight,
            Self::Cmyk => matches!(depth, BitDepth::Eight | BitDepth::Sixteen),
            Self::Multichannel | Self::Lab => false,
        }
    }
}

/// Compression method for the merged Image Data section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    /// `0` — Raw uncompressed planar channel bytes.
    Raw = 0,
    /// `1` — PackBits RLE per scanline per channel.
    Rle = 1,
    /// `2` — ZIP (zlib) stream without prediction.
    Zip = 2,
    /// `3` — ZIP (zlib) stream with horizontal differencing prediction.
    ZipPrediction = 3,
}

impl Compression {
    /// Parse compression method from big-endian `u16`.
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            0 => Some(Self::Raw),
            1 => Some(Self::Rle),
            2 => Some(Self::Zip),
            3 => Some(Self::ZipPrediction),
            _ => None,
        }
    }
}

/// Resolution unit from `ResolutionInfo` (`0x03ED`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionUnit {
    /// Pixels per inch (`1`).
    PixelsPerInch = 1,
    /// Pixels per centimeter (`2`).
    PixelsPerCentimeter = 2,
}

/// Resolution metadata from Image Resource `0x03ED` (`ResolutionInfo`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolutionInfo {
    /// Horizontal resolution in pixels per `h_unit`.
    pub h_res: f32,
    /// Unit for `h_res`.
    pub h_unit: ResolutionUnit,
    /// Vertical resolution in pixels per `v_unit`.
    pub v_res: f32,
    /// Unit for `v_res`.
    pub v_unit: ResolutionUnit,
}

/// Parsed 26-byte PSD/PSB File Header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PsdHeader {
    /// PSD (v1) or PSB (v2).
    pub version: Version,
    /// Total number of channels in the image (`1..=56`), including alpha/spot channels.
    pub channels: u16,
    /// Image height in pixels.
    pub height: u32,
    /// Image width in pixels.
    pub width: u32,
    /// Bits per channel (`1`, `8`, `16`, or `32`).
    pub depth: BitDepth,
    /// Color mode of the document.
    pub color_mode: ColorMode,
}

impl PsdHeader {
    /// Number of channels actually consumed when decoding the merged image
    /// (`base_channels`, plus `1` if at least one extra alpha channel is present
    /// for Grayscale, Duotone, RGB, or CMYK).
    pub fn used_channels(&self) -> u16 {
        let base = self.color_mode.base_channels().unwrap_or(self.channels);
        if matches!(
            self.color_mode,
            ColorMode::Grayscale | ColorMode::Duotone | ColorMode::Rgb | ColorMode::Cmyk
        ) && self.channels > base
        {
            base + 1
        } else {
            base
        }
    }

    /// Whether the decoded output carries an alpha channel (either via an extra
    /// channel in the header or via an indexed transparency index).
    pub fn has_alpha(&self, metadata: &PsdMetadata) -> bool {
        match self.color_mode {
            ColorMode::Indexed => metadata.transparency_index.is_some_and(|i| i < 256),
            ColorMode::Grayscale | ColorMode::Duotone | ColorMode::Rgb | ColorMode::Cmyk => {
                self.used_channels() > self.color_mode.base_channels().unwrap_or(0)
            }
            _ => false,
        }
    }

    /// Output [`PixelDescriptor`] produced when decoding this PSD/PSB.
    pub fn output_descriptor(&self, metadata: &PsdMetadata) -> PixelDescriptor {
        let alpha = self.has_alpha(metadata);
        match (self.color_mode, self.depth, alpha) {
            (ColorMode::Bitmap, _, _) => PixelDescriptor::GRAY8_SRGB,
            (ColorMode::Grayscale | ColorMode::Duotone, BitDepth::Eight, false) => {
                PixelDescriptor::GRAY8_SRGB
            }
            (ColorMode::Grayscale | ColorMode::Duotone, BitDepth::Eight, true) => {
                PixelDescriptor::GRAYA8_SRGB
            }
            (ColorMode::Grayscale | ColorMode::Duotone, BitDepth::Sixteen, false) => {
                PixelDescriptor::GRAY16_SRGB
            }
            (ColorMode::Grayscale | ColorMode::Duotone, BitDepth::Sixteen, true) => {
                PixelDescriptor::GRAYA16_SRGB
            }
            (ColorMode::Grayscale | ColorMode::Duotone, BitDepth::ThirtyTwo, false) => {
                PixelDescriptor::GRAYF32_LINEAR
            }
            (ColorMode::Grayscale | ColorMode::Duotone, BitDepth::ThirtyTwo, true) => {
                PixelDescriptor::RGBAF32_LINEAR
            }
            (ColorMode::Indexed, _, false) => PixelDescriptor::RGB8_SRGB,
            (ColorMode::Indexed, _, true) => PixelDescriptor::RGBA8_SRGB,
            (ColorMode::Rgb | ColorMode::Cmyk, BitDepth::Eight, false) => {
                PixelDescriptor::RGB8_SRGB
            }
            (ColorMode::Rgb | ColorMode::Cmyk, BitDepth::Eight, true) => {
                PixelDescriptor::RGBA8_SRGB
            }
            (ColorMode::Rgb | ColorMode::Cmyk, BitDepth::Sixteen, false) => {
                PixelDescriptor::RGB16_SRGB
            }
            (ColorMode::Rgb | ColorMode::Cmyk, BitDepth::Sixteen, true) => {
                PixelDescriptor::RGBA16_SRGB
            }
            (ColorMode::Rgb, BitDepth::ThirtyTwo, false) => PixelDescriptor::RGBF32_LINEAR,
            (ColorMode::Rgb, BitDepth::ThirtyTwo, true) => PixelDescriptor::RGBAF32_LINEAR,
            _ => PixelDescriptor::RGBA8_SRGB,
        }
    }
}

/// Metadata extracted from the PSD/PSB Image Resources section.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PsdMetadata {
    /// Embedded ICC profile from resource `0x040F` (1039).
    pub icc_profile: Option<Vec<u8>>,
    /// Embedded EXIF data from resource `0x0422` (1058) or `0x0423` (1059).
    pub exif: Option<Vec<u8>>,
    /// Embedded XMP packet from resource `0x0424` (1060).
    pub xmp: Option<Vec<u8>>,
    /// Resolution information from resource `0x03ED` (1005).
    pub resolution: Option<ResolutionInfo>,
    /// Index of the transparent palette entry for `ColorMode::Indexed` (`0x0417` / 1047).
    pub transparency_index: Option<u16>,
    /// `hasRealMergedData` flag from `VersionInfo` (`0x0421` / 1057).
    pub has_real_merged_data: Option<bool>,
    /// Alpha channel names from `0x03EE` (1006) or `0x0415` (1045).
    pub alpha_channel_names: Vec<String>,
}

/// Probed structure and metadata of a PSD/PSB file without decoding scanlines.
#[derive(Clone, Debug, PartialEq)]
pub struct PsdInfo {
    /// Parsed 26-byte File Header.
    pub header: PsdHeader,
    /// Compression method of the merged Image Data section.
    pub compression: Compression,
    /// Metadata extracted from the Image Resources section.
    pub metadata: PsdMetadata,
}

/// Internal section byte ranges discovered while parsing the container.
#[derive(Clone, Debug)]
#[cfg_attr(not(feature = "inventory"), allow(dead_code))]
pub(crate) struct ParsedLayout {
    pub info: PsdInfo,
    pub reserved_zero: bool,
    pub color_mode_len_range: Range<usize>,
    pub color_mode_data_range: Range<usize>,
    pub resources_len_range: Range<usize>,
    pub resources_data_range: Range<usize>,
    pub layer_mask_len_range: Range<usize>,
    pub layer_mask_data_range: Range<usize>,
    pub compression_range: Range<usize>,
    pub image_data_payload_start: usize,
}

/// Decoded PSD/PSB image and metadata.
#[derive(Debug)]
pub struct DecodedPsd {
    pixels: PixelBuffer,
    info: PsdInfo,
}

impl DecodedPsd {
    /// Borrow the decoded pixel buffer.
    pub fn pixels(&self) -> &PixelBuffer {
        &self.pixels
    }

    /// Consume and return the decoded pixel buffer.
    pub fn into_pixels(self) -> PixelBuffer {
        self.pixels
    }

    /// Borrow the parsed PSD header, compression mode, and metadata.
    pub fn info(&self) -> &PsdInfo {
        &self.info
    }

    /// Borrow the parsed 26-byte PSD header.
    pub fn header(&self) -> &PsdHeader {
        &self.info.header
    }

    /// Borrow the extracted metadata.
    pub fn metadata(&self) -> &PsdMetadata {
        &self.info.metadata
    }
}

/// Probe the header, sections, metadata, and merged image compression without
/// decompressing the merged image scanlines.
pub fn probe(data: &[u8]) -> Result<PsdInfo> {
    Ok(parse_layout(data)?.info)
}

/// Decode the embedded merged render of a PSD/PSB document into a [`DecodedPsd`].
pub fn decode(data: &[u8]) -> Result<DecodedPsd> {
    decode_with_stop(data, &enough::Unstoppable)
}

/// Decode the embedded merged render of a PSD/PSB document with cooperative cancellation.
pub fn decode_with_stop(data: &[u8], stop: &dyn Stop) -> Result<DecodedPsd> {
    decode_internal(data, stop, AllocPref::CodecDefault, false)
}

pub(crate) fn decode_internal(
    data: &[u8],
    stop: &dyn Stop,
    alloc_pref: AllocPref,
    require_real_merged_data: bool,
) -> Result<DecodedPsd> {
    stop.check().map_err(|r| at!(PsdError::Stopped(r)))?;
    let layout = parse_layout(data)?;
    if require_real_merged_data {
        if !layout.reserved_zero {
            return Err(at!(PsdError::Malformed("non-zero reserved header bytes")));
        }
        if layout.info.metadata.has_real_merged_data == Some(false) {
            return Err(at!(PsdError::MissingMergedData));
        }
    }
    let (pixels, _end) = decode_merged_image(data, &layout, stop, alloc_pref)?;
    Ok(DecodedPsd {
        pixels,
        info: layout.info,
    })
}

fn read_u16_be(data: &[u8], at: usize) -> Option<u16> {
    let b = data.get(at..at + 2)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

fn read_u32_be(data: &[u8], at: usize) -> Option<u32> {
    let b = data.get(at..at + 4)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_u64_be(data: &[u8], at: usize) -> Option<u64> {
    let b = data.get(at..at + 8)?;
    Some(u64::from_be_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

pub(crate) fn parse_file_header(data: &[u8]) -> Result<(PsdHeader, bool)> {
    if data.len() < 4 {
        if data.iter().zip(b"8BPS").all(|(a, b)| a == b) {
            return Err(at!(PsdError::UnexpectedEof));
        }
        return Err(at!(PsdError::InvalidSignature));
    }
    if &data[0..4] != b"8BPS" {
        return Err(at!(PsdError::InvalidSignature));
    }
    if data.len() < 26 {
        return Err(at!(PsdError::UnexpectedEof));
    }
    let raw_version = read_u16_be(data, 4).unwrap();
    let Some(version) = Version::from_u16(raw_version) else {
        return Err(at!(PsdError::UnsupportedVersion(raw_version)));
    };
    let reserved_zero = data[6..12] == [0; 6];
    let channels = read_u16_be(data, 12).unwrap();
    let height = read_u32_be(data, 14).unwrap();
    let width = read_u32_be(data, 18).unwrap();
    let raw_depth = read_u16_be(data, 22).unwrap();
    let raw_mode = read_u16_be(data, 24).unwrap();

    if channels == 0
        || channels > MAX_CHANNELS
        || width == 0
        || height == 0
        || width > version.max_dimension()
        || height > version.max_dimension()
    {
        return Err(at!(PsdError::InvalidDimensions {
            width,
            height,
            channels,
        }));
    }

    let Some(depth) = BitDepth::from_u16(raw_depth) else {
        return Err(at!(PsdError::UnsupportedBitDepth(raw_depth)));
    };
    let Some(color_mode) = ColorMode::from_u16(raw_mode) else {
        return Err(at!(PsdError::UnsupportedColorMode(raw_mode)));
    };
    if !color_mode.supports_depth(depth) {
        if matches!(color_mode, ColorMode::Multichannel | ColorMode::Lab) {
            return Err(at!(PsdError::UnsupportedColorMode(raw_mode)));
        }
        return Err(at!(PsdError::UnsupportedBitDepth(raw_depth)));
    }
    let min_channels = color_mode.base_channels().unwrap_or(1);
    if channels < min_channels {
        return Err(at!(PsdError::InvalidDimensions {
            width,
            height,
            channels,
        }));
    }

    Ok((
        PsdHeader {
            version,
            channels,
            height,
            width,
            depth,
            color_mode,
        },
        reserved_zero,
    ))
}

pub(crate) fn parse_layout(data: &[u8]) -> Result<ParsedLayout> {
    let (header, reserved_zero) = parse_file_header(data)?;

    // 2. Color Mode Data Section
    let color_mode_len =
        read_u32_be(data, 26).ok_or_else(|| at!(PsdError::UnexpectedEof))? as usize;
    let color_mode_len_range = 26..30;
    let color_mode_end = 30usize
        .checked_add(color_mode_len)
        .ok_or_else(|| at!(PsdError::Malformed("color mode section length overflow")))?;
    if color_mode_end > data.len() {
        return Err(at!(PsdError::UnexpectedEof));
    }
    let color_mode_data_range = 30..color_mode_end;
    match header.color_mode {
        ColorMode::Indexed => {
            if color_mode_len != 768 {
                return Err(at!(PsdError::Malformed(
                    "indexed color mode requires a 768-byte color table"
                )));
            }
        }
        ColorMode::Duotone => {
            if color_mode_len == 0 {
                return Err(at!(PsdError::Malformed(
                    "duotone color mode requires non-empty duotone data"
                )));
            }
        }
        _ => {
            if color_mode_len != 0 {
                return Err(at!(PsdError::Malformed(
                    "non-empty color mode data for non-indexed/duotone mode"
                )));
            }
        }
    }

    // 3. Image Resources Section
    let res_len =
        read_u32_be(data, color_mode_end).ok_or_else(|| at!(PsdError::UnexpectedEof))? as usize;
    let resources_len_range = color_mode_end..color_mode_end + 4;
    let resources_start = color_mode_end + 4;
    let resources_end = resources_start
        .checked_add(res_len)
        .ok_or_else(|| at!(PsdError::Malformed("image resources length overflow")))?;
    if resources_end > data.len() {
        return Err(at!(PsdError::UnexpectedEof));
    }
    let resources_data_range = resources_start..resources_end;
    let metadata = parse_image_resources(&data[resources_data_range.clone()])?;

    // 4. Layer and Mask Information Section
    let (layer_mask_len, layer_mask_len_bytes) = match header.version {
        Version::Psd => {
            let n = read_u32_be(data, resources_end).ok_or_else(|| at!(PsdError::UnexpectedEof))?;
            (n as usize, 4usize)
        }
        Version::Psb => {
            let n = read_u64_be(data, resources_end).ok_or_else(|| at!(PsdError::UnexpectedEof))?;
            let n_usize = usize::try_from(n)
                .map_err(|_| at!(PsdError::Malformed("layer section length overflow")))?;
            (n_usize, 8usize)
        }
    };
    let layer_mask_len_range = resources_end..resources_end + layer_mask_len_bytes;
    let layer_mask_start = resources_end + layer_mask_len_bytes;
    let layer_mask_end = layer_mask_start
        .checked_add(layer_mask_len)
        .ok_or_else(|| at!(PsdError::Malformed("layer section length overflow")))?;
    if layer_mask_end > data.len() {
        return Err(at!(PsdError::UnexpectedEof));
    }
    let layer_mask_data_range = layer_mask_start..layer_mask_end;

    // 5. Image Data Section (starts with u16 compression)
    let raw_comp = read_u16_be(data, layer_mask_end).ok_or_else(|| at!(PsdError::UnexpectedEof))?;
    let Some(compression) = Compression::from_u16(raw_comp) else {
        return Err(at!(PsdError::UnsupportedCompression(raw_comp)));
    };
    let compression_range = layer_mask_end..layer_mask_end + 2;
    let image_data_payload_start = layer_mask_end + 2;

    Ok(ParsedLayout {
        info: PsdInfo {
            header,
            compression,
            metadata,
        },
        reserved_zero,
        color_mode_len_range,
        color_mode_data_range,
        resources_len_range,
        resources_data_range,
        layer_mask_len_range,
        layer_mask_data_range,
        compression_range,
        image_data_payload_start,
    })
}

/// Known 4-byte signatures for Photoshop Image Resource Blocks.
pub(crate) fn is_resource_signature(sig: &[u8]) -> bool {
    matches!(
        sig,
        b"8BIM" | b"MeSa" | b"AgHg" | b"PHUT" | b"DCSR" | b"Lr16" | b"Lr32"
    )
}

fn parse_image_resources(res_bytes: &[u8]) -> Result<PsdMetadata> {
    let mut meta = PsdMetadata::default();
    let mut pos = 0usize;
    while pos < res_bytes.len() {
        if pos + 4 > res_bytes.len() {
            return Err(at!(PsdError::Malformed(
                "truncated image resource block signature"
            )));
        }
        let sig = &res_bytes[pos..pos + 4];
        if !is_resource_signature(sig) {
            return Err(at!(PsdError::Malformed(
                "invalid image resource block signature"
            )));
        }
        let id = read_u16_be(res_bytes, pos + 4)
            .ok_or_else(|| at!(PsdError::Malformed("truncated image resource ID")))?;
        let name_len = *res_bytes
            .get(pos + 6)
            .ok_or_else(|| at!(PsdError::Malformed("truncated image resource name")))?
            as usize;
        // Pascal string (1 byte length + name_len bytes), padded to even length.
        let padded_name = (1 + name_len + 1) & !1;
        let len_offset = pos + 6 + padded_name;
        let data_len = read_u32_be(res_bytes, len_offset)
            .ok_or_else(|| at!(PsdError::Malformed("truncated image resource data length")))?
            as usize;
        let data_start = len_offset + 4;
        let data_end = data_start
            .checked_add(data_len)
            .ok_or_else(|| at!(PsdError::Malformed("image resource data length overflow")))?;
        let padded_end = data_start
            .checked_add((data_len + 1) & !1)
            .ok_or_else(|| at!(PsdError::Malformed("image resource data padding overflow")))?;
        if data_end > res_bytes.len() {
            return Err(at!(PsdError::Malformed(
                "image resource block exceeds section length"
            )));
        }
        let payload = &res_bytes[data_start..data_end];
        match id {
            0x03ED if payload.len() >= 16 => {
                // ResolutionInfo: hRes (fixed 16.16), hResUnit (u16), widthUnit (u16),
                //                 vRes (fixed 16.16), vResUnit (u16), heightUnit (u16)
                let h_fixed = read_u32_be(payload, 0).unwrap();
                let h_unit_raw = read_u16_be(payload, 4).unwrap();
                let v_fixed = read_u32_be(payload, 8).unwrap();
                let v_unit_raw = read_u16_be(payload, 12).unwrap();
                let h_unit = if h_unit_raw == 2 {
                    ResolutionUnit::PixelsPerCentimeter
                } else {
                    ResolutionUnit::PixelsPerInch
                };
                let v_unit = if v_unit_raw == 2 {
                    ResolutionUnit::PixelsPerCentimeter
                } else {
                    ResolutionUnit::PixelsPerInch
                };
                meta.resolution = Some(ResolutionInfo {
                    h_res: (h_fixed as f64 / 65536.0) as f32,
                    h_unit,
                    v_res: (v_fixed as f64 / 65536.0) as f32,
                    v_unit,
                });
            }
            0x03EE => {
                // Alpha channel names (series of Pascal strings)
                let mut i = 0;
                while i < payload.len() {
                    let n = payload[i] as usize;
                    i += 1;
                    if i + n > payload.len() {
                        break;
                    }
                    let s = String::from_utf8_lossy(&payload[i..i + n]).into_owned();
                    meta.alpha_channel_names.push(s);
                    i += n;
                }
            }
            0x040F if !payload.is_empty() => {
                meta.icc_profile = Some(payload.to_vec());
            }
            0x0417 if payload.len() >= 2 => {
                meta.transparency_index = read_u16_be(payload, 0);
            }
            0x0421 if payload.len() >= 5 => {
                // VersionInfo: u32 version (1), u8 hasRealMergedData
                let version = read_u32_be(payload, 0).unwrap();
                if version == 1 {
                    meta.has_real_merged_data = Some(payload[4] != 0);
                }
            }
            0x0422 if !payload.is_empty() => {
                meta.exif = Some(payload.to_vec());
            }
            0x0423 if !payload.is_empty() && meta.exif.is_none() => {
                meta.exif = Some(payload.to_vec());
            }
            0x0424 if !payload.is_empty() => {
                meta.xmp = Some(payload.to_vec());
            }
            _ => {}
        }
        pos = padded_end.min(res_bytes.len());
    }
    Ok(meta)
}

/// Decompress and interleave the merged Image Data section.
/// Returns `(PixelBuffer, end_offset_in_file)`.
pub(crate) fn decode_merged_image(
    data: &[u8],
    layout: &ParsedLayout,
    stop: &dyn Stop,
    alloc_pref: AllocPref,
) -> Result<(PixelBuffer, usize)> {
    let header = &layout.info.header;
    let total_channels = header.channels as usize;
    let used_channels = header.used_channels() as usize;
    let height = header.height as usize;
    let width = header.width as usize;

    let row_bytes = header
        .depth
        .scanline_bytes(header.width)
        .ok_or_else(|| at!(PsdError::Malformed("scanline byte size overflow")))?;
    let plane_bytes = row_bytes
        .checked_mul(height)
        .ok_or_else(|| at!(PsdError::Malformed("channel plane size overflow")))?;
    let used_plane_bytes = plane_bytes
        .checked_mul(used_channels)
        .ok_or_else(|| at!(PsdError::Malformed("total plane size overflow")))?;

    let payload_start = layout.image_data_payload_start;
    if payload_start > data.len() {
        return Err(at!(PsdError::UnexpectedEof));
    }
    let payload = &data[payload_start..];

    let (mut planes, consumed_bytes) = match layout.info.compression {
        Compression::Raw => {
            let total_raw = plane_bytes
                .checked_mul(total_channels)
                .ok_or_else(|| at!(PsdError::Malformed("raw image data size overflow")))?;
            if payload.len() < total_raw {
                return Err(at!(PsdError::UnexpectedEof));
            }
            let mut buf = alloc_zeroed(alloc_pref, true, used_plane_bytes)?;
            buf.copy_from_slice(&payload[..used_plane_bytes]);
            (buf, total_raw)
        }
        Compression::Rle => {
            let total_scanlines = total_channels
                .checked_mul(height)
                .ok_or_else(|| at!(PsdError::Malformed("scanline count overflow")))?;
            let count_size = match header.version {
                Version::Psd => 2usize,
                Version::Psb => 4usize,
            };
            let table_bytes = total_scanlines
                .checked_mul(count_size)
                .ok_or_else(|| at!(PsdError::Malformed("RLE table size overflow")))?;
            if payload.len() < table_bytes {
                return Err(at!(PsdError::UnexpectedEof));
            }

            let mut total_compressed = table_bytes;
            for line in 0..total_scanlines {
                let len = match header.version {
                    Version::Psd => read_u16_be(payload, line * 2).unwrap() as usize,
                    Version::Psb => read_u32_be(payload, line * 4).unwrap() as usize,
                };
                total_compressed = total_compressed
                    .checked_add(len)
                    .ok_or_else(|| at!(PsdError::Malformed("RLE compressed length overflow")))?;
            }
            if payload.len() < total_compressed {
                return Err(at!(PsdError::UnexpectedEof));
            }

            let mut buf = alloc_zeroed(alloc_pref, true, used_plane_bytes)?;
            let used_scanlines = used_channels * height;
            let mut src_pos = table_bytes;
            for line in 0..used_scanlines {
                if (line & 63) == 0 {
                    stop.check().map_err(|r| at!(PsdError::Stopped(r)))?;
                }
                let len = match header.version {
                    Version::Psd => read_u16_be(payload, line * 2).unwrap() as usize,
                    Version::Psb => read_u32_be(payload, line * 4).unwrap() as usize,
                };
                let src = &payload[src_pos..src_pos + len];
                let dst = &mut buf[line * row_bytes..(line + 1) * row_bytes];
                decode_packbits_row(src, dst)?;
                src_pos += len;
            }
            (buf, total_compressed)
        }
        Compression::Zip | Compression::ZipPrediction => {
            if matches!(header.depth, BitDepth::One) {
                return Err(at!(PsdError::Malformed(
                    "ZIP compression is not valid for 1-bit Bitmap mode"
                )));
            }
            let (mut buf, consumed) = decompress_zip_planes(
                payload,
                plane_bytes,
                used_channels,
                total_channels,
                stop,
                alloc_pref,
            )?;
            if layout.info.compression == Compression::ZipPrediction {
                for ch in 0..used_channels {
                    let plane = &mut buf[ch * plane_bytes..(ch + 1) * plane_bytes];
                    apply_zip_prediction(plane, width, height, row_bytes, header.depth)?;
                }
            }
            (buf, consumed)
        }
    };

    stop.check().map_err(|r| at!(PsdError::Stopped(r)))?;
    let palette = if header.color_mode == ColorMode::Indexed {
        Some(&data[layout.color_mode_data_range.clone()])
    } else {
        None
    };
    let pixel_buffer = interleave_planes(
        &mut planes,
        header,
        &layout.info.metadata,
        palette,
        row_bytes,
        plane_bytes,
        alloc_pref,
    )?;
    Ok((pixel_buffer, payload_start + consumed_bytes))
}

/// Decode a single PackBits RLE scanline (`src`) into `dst` (`row_bytes` bytes).
fn decode_packbits_row(src: &[u8], dst: &mut [u8]) -> Result<()> {
    let mut si = 0usize;
    let mut di = 0usize;
    while si < src.len() && di < dst.len() {
        let header = src[si] as i8;
        si += 1;
        if header >= 0 {
            let count = (header as usize) + 1;
            let end_s = si
                .checked_add(count)
                .ok_or_else(|| at!(PsdError::Malformed("PackBits literal overflow")))?;
            let end_d = di
                .checked_add(count)
                .ok_or_else(|| at!(PsdError::Malformed("PackBits literal exceeds scanline")))?;
            if end_s > src.len() {
                return Err(at!(PsdError::Malformed(
                    "truncated PackBits literal run in scanline"
                )));
            }
            if end_d > dst.len() {
                return Err(at!(PsdError::Malformed(
                    "PackBits literal run overflows scanline width"
                )));
            }
            dst[di..end_d].copy_from_slice(&src[si..end_s]);
            si = end_s;
            di = end_d;
        } else if header != -128 {
            let count = (1isize - (header as isize)) as usize;
            let val = *src.get(si).ok_or_else(|| {
                at!(PsdError::Malformed(
                    "truncated PackBits repeat byte in scanline"
                ))
            })?;
            si += 1;
            let end_d = di
                .checked_add(count)
                .ok_or_else(|| at!(PsdError::Malformed("PackBits repeat exceeds scanline")))?;
            if end_d > dst.len() {
                return Err(at!(PsdError::Malformed(
                    "PackBits repeat run overflows scanline width"
                )));
            }
            dst[di..end_d].fill(val);
            di = end_d;
        }
    }
    if di != dst.len() {
        return Err(at!(PsdError::Malformed(
            "PackBits scanline decoded fewer bytes than width"
        )));
    }
    Ok(())
}

/// Decompress zlib-compressed planar image data.
/// Supports both single-zlib-stream (all channels concatenated) and per-channel zlib streams.
fn decompress_zip_planes(
    payload: &[u8],
    plane_bytes: usize,
    used_channels: usize,
    total_channels: usize,
    stop: &dyn Stop,
    alloc_pref: AllocPref,
) -> Result<(Vec<u8>, usize)> {
    if payload.is_empty() {
        return Err(at!(PsdError::UnexpectedEof));
    }
    let used_total = plane_bytes
        .checked_mul(used_channels)
        .ok_or_else(|| at!(PsdError::Malformed("ZIP buffer size overflow")))?;
    let all_total = plane_bytes
        .checked_mul(total_channels)
        .ok_or_else(|| at!(PsdError::Malformed("ZIP total size overflow")))?;

    let mut out = alloc_zeroed(alloc_pref, true, used_total)?;
    let mut z = flate2::Decompress::new(true);
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;
    let mut discard = [0u8; 8192];

    while out_pos < all_total {
        stop.check().map_err(|r| at!(PsdError::Stopped(r)))?;
        let before_in = z.total_in();
        let before_out = z.total_out();
        let status = if out_pos < used_total {
            z.decompress(
                &payload[in_pos..],
                &mut out[out_pos..used_total],
                flate2::FlushDecompress::None,
            )
        } else {
            let rem = (all_total - out_pos).min(discard.len());
            z.decompress(
                &payload[in_pos..],
                &mut discard[..rem],
                flate2::FlushDecompress::None,
            )
        }
        .map_err(|_| at!(PsdError::Malformed("corrupt zlib stream in PSD image data")))?;

        let consumed = (z.total_in() - before_in) as usize;
        let produced = (z.total_out() - before_out) as usize;
        in_pos += consumed;
        out_pos += produced;

        if status == flate2::Status::StreamEnd {
            if out_pos == all_total {
                break;
            }
            // Per-channel zlib streams: if StreamEnd lands on a channel boundary, reset for next channel.
            if plane_bytes > 0 && out_pos.is_multiple_of(plane_bytes) {
                z.reset(true);
                continue;
            }
            return Err(at!(PsdError::Malformed(
                "zlib stream ended before all PSD channel scanlines were decoded"
            )));
        }
        if consumed == 0 && produced == 0 {
            return Err(at!(PsdError::UnexpectedEof));
        }
    }
    Ok((out, in_pos))
}

/// Undo horizontal differencing prediction in-place on a single channel plane.
fn apply_zip_prediction(
    plane: &mut [u8],
    width: usize,
    height: usize,
    row_bytes: usize,
    depth: BitDepth,
) -> Result<()> {
    match depth {
        BitDepth::Eight => {
            for y in 0..height {
                let row = &mut plane[y * row_bytes..(y + 1) * row_bytes];
                for x in 1..width {
                    row[x] = row[x].wrapping_add(row[x - 1]);
                }
            }
        }
        BitDepth::Sixteen => {
            for y in 0..height {
                let row = &mut plane[y * row_bytes..(y + 1) * row_bytes];
                if width >= 2 {
                    let mut prev = u16::from_be_bytes([row[0], row[1]]);
                    for px in row[2..].as_chunks_mut::<2>().0 {
                        prev = prev.wrapping_add(u16::from_be_bytes(*px));
                        *px = prev.to_be_bytes();
                    }
                }
            }
        }
        BitDepth::ThirtyTwo => {
            // Adobe 32-bit floating-point predictor:
            // 1. Byte-level horizontal delta across all 4*width bytes in the scanline.
            // 2. De-interleave the 4 byte planes [MSB0..MSBw, B1_0..B1_w, B2_0..B2_w, LSB0..LSBw]
            //    back into big-endian 4-byte floats per pixel.
            let mut scratch = vec![0u8; row_bytes];
            for y in 0..height {
                let row = &mut plane[y * row_bytes..(y + 1) * row_bytes];
                for i in 1..row_bytes {
                    row[i] = row[i].wrapping_add(row[i - 1]);
                }
                scratch.copy_from_slice(row);
                for x in 0..width {
                    row[x * 4] = scratch[x];
                    row[x * 4 + 1] = scratch[width + x];
                    row[x * 4 + 2] = scratch[width * 2 + x];
                    row[x * 4 + 3] = scratch[width * 3 + x];
                }
            }
        }
        BitDepth::One => {}
    }
    Ok(())
}

/// Convert decoded planar channel data into an interleaved [`PixelBuffer`].
fn interleave_planes(
    planes: &mut [u8],
    header: &PsdHeader,
    metadata: &PsdMetadata,
    palette: Option<&[u8]>,
    row_bytes: usize,
    plane_bytes: usize,
    alloc_pref: AllocPref,
) -> Result<PixelBuffer> {
    let width = header.width;
    let height = header.height;
    let w = width as usize;
    let h = height as usize;
    let num_pixels = w
        .checked_mul(h)
        .ok_or_else(|| at!(PsdError::Malformed("pixel count overflow")))?;
    let descriptor = header.output_descriptor(metadata);
    let out_bytes = num_pixels
        .checked_mul(descriptor.bytes_per_pixel())
        .ok_or_else(|| at!(PsdError::Malformed("output buffer size overflow")))?;
    let has_alpha = header.has_alpha(metadata);

    let out = match (header.color_mode, header.depth, has_alpha) {
        (ColorMode::Bitmap, BitDepth::One, _) => {
            // 1-bit MSB-first per row; in Photoshop Bitmap mode 0 = white (255), 1 = black (0).
            let mut out = alloc_zeroed(alloc_pref, true, out_bytes)?;
            for y in 0..h {
                let row = &planes[y * row_bytes..(y + 1) * row_bytes];
                let dst = &mut out[y * w..(y + 1) * w];
                for x in 0..w {
                    let bit = (row[x >> 3] >> (7 - (x & 7))) & 1;
                    dst[x] = if bit == 0 { 255 } else { 0 };
                }
            }
            out
        }
        (ColorMode::Grayscale | ColorMode::Duotone, BitDepth::Eight, false) => {
            let mut out = alloc_zeroed(alloc_pref, true, out_bytes)?;
            out.copy_from_slice(&planes[..plane_bytes]);
            out
        }
        (ColorMode::Grayscale | ColorMode::Duotone, BitDepth::Eight, true) => {
            let mut out = alloc_zeroed(alloc_pref, true, out_bytes)?;
            let (g_plane, a_plane) = planes.split_at(plane_bytes);
            for i in 0..num_pixels {
                out[i * 2] = g_plane[i];
                out[i * 2 + 1] = a_plane[i];
            }
            out
        }
        (ColorMode::Grayscale | ColorMode::Duotone, BitDepth::Sixteen, false) => {
            let mut out = alloc_zeroed(alloc_pref, true, out_bytes)?;
            for (src, dst) in planes[..plane_bytes]
                .as_chunks::<2>()
                .0
                .iter()
                .zip(out.as_chunks_mut::<2>().0)
            {
                *dst = u16::from_be_bytes(*src).to_ne_bytes();
            }
            out
        }
        (ColorMode::Grayscale | ColorMode::Duotone, BitDepth::Sixteen, true) => {
            let mut out = alloc_zeroed(alloc_pref, true, out_bytes)?;
            let (g_plane, a_plane) = planes.split_at(plane_bytes);
            for i in 0..num_pixels {
                let g = u16::from_be_bytes([g_plane[i * 2], g_plane[i * 2 + 1]]).to_ne_bytes();
                let a = u16::from_be_bytes([a_plane[i * 2], a_plane[i * 2 + 1]]).to_ne_bytes();
                out[i * 4..i * 4 + 2].copy_from_slice(&g);
                out[i * 4 + 2..i * 4 + 4].copy_from_slice(&a);
            }
            out
        }
        (ColorMode::Grayscale | ColorMode::Duotone, BitDepth::ThirtyTwo, false) => {
            let mut out = alloc_zeroed(alloc_pref, true, out_bytes)?;
            for (src, dst) in planes[..plane_bytes]
                .as_chunks::<4>()
                .0
                .iter()
                .zip(out.as_chunks_mut::<4>().0)
            {
                *dst = f32::from_bits(u32::from_be_bytes(*src)).to_ne_bytes();
            }
            out
        }
        (ColorMode::Grayscale | ColorMode::Duotone, BitDepth::ThirtyTwo, true) => {
            let mut out = alloc_zeroed(alloc_pref, true, out_bytes)?;
            let (g_plane, a_plane) = planes.split_at(plane_bytes);
            for i in 0..num_pixels {
                let g = f32::from_bits(u32::from_be_bytes([
                    g_plane[i * 4],
                    g_plane[i * 4 + 1],
                    g_plane[i * 4 + 2],
                    g_plane[i * 4 + 3],
                ]))
                .to_ne_bytes();
                let a = f32::from_bits(u32::from_be_bytes([
                    a_plane[i * 4],
                    a_plane[i * 4 + 1],
                    a_plane[i * 4 + 2],
                    a_plane[i * 4 + 3],
                ]))
                .to_ne_bytes();
                out[i * 16..i * 16 + 4].copy_from_slice(&g);
                out[i * 16 + 4..i * 16 + 8].copy_from_slice(&g);
                out[i * 16 + 8..i * 16 + 12].copy_from_slice(&g);
                out[i * 16 + 12..i * 16 + 16].copy_from_slice(&a);
            }
            out
        }
        (ColorMode::Indexed, BitDepth::Eight, _) => {
            let pal = palette.ok_or_else(|| at!(PsdError::Malformed("missing indexed palette")))?;
            let r_pal = &pal[0..256];
            let g_pal = &pal[256..512];
            let b_pal = &pal[512..768];
            let mut out = alloc_zeroed(alloc_pref, true, out_bytes)?;
            if let Some(trans_idx) = metadata.transparency_index.filter(|&i| i < 256) {
                let ti = trans_idx as u8;
                for (i, &idx) in planes[..num_pixels].iter().enumerate() {
                    let k = idx as usize;
                    out[i * 4] = r_pal[k];
                    out[i * 4 + 1] = g_pal[k];
                    out[i * 4 + 2] = b_pal[k];
                    out[i * 4 + 3] = if idx == ti { 0 } else { 255 };
                }
            } else {
                for (i, &idx) in planes[..num_pixels].iter().enumerate() {
                    let k = idx as usize;
                    out[i * 3] = r_pal[k];
                    out[i * 3 + 1] = g_pal[k];
                    out[i * 3 + 2] = b_pal[k];
                }
            }
            out
        }
        (ColorMode::Rgb, BitDepth::Eight, false) => {
            let mut out = alloc_zeroed(alloc_pref, true, out_bytes)?;
            let r_plane = &planes[0..plane_bytes];
            let g_plane = &planes[plane_bytes..plane_bytes * 2];
            let b_plane = &planes[plane_bytes * 2..plane_bytes * 3];
            for i in 0..num_pixels {
                out[i * 3] = r_plane[i];
                out[i * 3 + 1] = g_plane[i];
                out[i * 3 + 2] = b_plane[i];
            }
            out
        }
        (ColorMode::Rgb, BitDepth::Eight, true) => {
            let mut out = alloc_zeroed(alloc_pref, true, out_bytes)?;
            let r_plane = &planes[0..plane_bytes];
            let g_plane = &planes[plane_bytes..plane_bytes * 2];
            let b_plane = &planes[plane_bytes * 2..plane_bytes * 3];
            let a_plane = &planes[plane_bytes * 3..plane_bytes * 4];
            for i in 0..num_pixels {
                out[i * 4] = r_plane[i];
                out[i * 4 + 1] = g_plane[i];
                out[i * 4 + 2] = b_plane[i];
                out[i * 4 + 3] = a_plane[i];
            }
            out
        }
        (ColorMode::Rgb, BitDepth::Sixteen, false) => {
            let mut out = alloc_zeroed(alloc_pref, true, out_bytes)?;
            let r_plane = &planes[0..plane_bytes];
            let g_plane = &planes[plane_bytes..plane_bytes * 2];
            let b_plane = &planes[plane_bytes * 2..plane_bytes * 3];
            for i in 0..num_pixels {
                let r = u16::from_be_bytes([r_plane[i * 2], r_plane[i * 2 + 1]]).to_ne_bytes();
                let g = u16::from_be_bytes([g_plane[i * 2], g_plane[i * 2 + 1]]).to_ne_bytes();
                let b = u16::from_be_bytes([b_plane[i * 2], b_plane[i * 2 + 1]]).to_ne_bytes();
                out[i * 6..i * 6 + 2].copy_from_slice(&r);
                out[i * 6 + 2..i * 6 + 4].copy_from_slice(&g);
                out[i * 6 + 4..i * 6 + 6].copy_from_slice(&b);
            }
            out
        }
        (ColorMode::Rgb, BitDepth::Sixteen, true) => {
            let mut out = alloc_zeroed(alloc_pref, true, out_bytes)?;
            let r_plane = &planes[0..plane_bytes];
            let g_plane = &planes[plane_bytes..plane_bytes * 2];
            let b_plane = &planes[plane_bytes * 2..plane_bytes * 3];
            let a_plane = &planes[plane_bytes * 3..plane_bytes * 4];
            for i in 0..num_pixels {
                let r = u16::from_be_bytes([r_plane[i * 2], r_plane[i * 2 + 1]]).to_ne_bytes();
                let g = u16::from_be_bytes([g_plane[i * 2], g_plane[i * 2 + 1]]).to_ne_bytes();
                let b = u16::from_be_bytes([b_plane[i * 2], b_plane[i * 2 + 1]]).to_ne_bytes();
                let a = u16::from_be_bytes([a_plane[i * 2], a_plane[i * 2 + 1]]).to_ne_bytes();
                out[i * 8..i * 8 + 2].copy_from_slice(&r);
                out[i * 8 + 2..i * 8 + 4].copy_from_slice(&g);
                out[i * 8 + 4..i * 8 + 6].copy_from_slice(&b);
                out[i * 8 + 6..i * 8 + 8].copy_from_slice(&a);
            }
            out
        }
        (ColorMode::Rgb, BitDepth::ThirtyTwo, false) => {
            let mut out = alloc_zeroed(alloc_pref, true, out_bytes)?;
            let r_plane = &planes[0..plane_bytes];
            let g_plane = &planes[plane_bytes..plane_bytes * 2];
            let b_plane = &planes[plane_bytes * 2..plane_bytes * 3];
            for i in 0..num_pixels {
                let j = i * 4;
                let r = f32::from_bits(u32::from_be_bytes([
                    r_plane[j],
                    r_plane[j + 1],
                    r_plane[j + 2],
                    r_plane[j + 3],
                ]))
                .to_ne_bytes();
                let g = f32::from_bits(u32::from_be_bytes([
                    g_plane[j],
                    g_plane[j + 1],
                    g_plane[j + 2],
                    g_plane[j + 3],
                ]))
                .to_ne_bytes();
                let b = f32::from_bits(u32::from_be_bytes([
                    b_plane[j],
                    b_plane[j + 1],
                    b_plane[j + 2],
                    b_plane[j + 3],
                ]))
                .to_ne_bytes();
                out[i * 12..i * 12 + 4].copy_from_slice(&r);
                out[i * 12 + 4..i * 12 + 8].copy_from_slice(&g);
                out[i * 12 + 8..i * 12 + 12].copy_from_slice(&b);
            }
            out
        }
        (ColorMode::Rgb, BitDepth::ThirtyTwo, true) => {
            let mut out = alloc_zeroed(alloc_pref, true, out_bytes)?;
            let r_plane = &planes[0..plane_bytes];
            let g_plane = &planes[plane_bytes..plane_bytes * 2];
            let b_plane = &planes[plane_bytes * 2..plane_bytes * 3];
            let a_plane = &planes[plane_bytes * 3..plane_bytes * 4];
            for i in 0..num_pixels {
                let j = i * 4;
                let r = f32::from_bits(u32::from_be_bytes([
                    r_plane[j],
                    r_plane[j + 1],
                    r_plane[j + 2],
                    r_plane[j + 3],
                ]))
                .to_ne_bytes();
                let g = f32::from_bits(u32::from_be_bytes([
                    g_plane[j],
                    g_plane[j + 1],
                    g_plane[j + 2],
                    g_plane[j + 3],
                ]))
                .to_ne_bytes();
                let b = f32::from_bits(u32::from_be_bytes([
                    b_plane[j],
                    b_plane[j + 1],
                    b_plane[j + 2],
                    b_plane[j + 3],
                ]))
                .to_ne_bytes();
                let a = f32::from_bits(u32::from_be_bytes([
                    a_plane[j],
                    a_plane[j + 1],
                    a_plane[j + 2],
                    a_plane[j + 3],
                ]))
                .to_ne_bytes();
                out[i * 16..i * 16 + 4].copy_from_slice(&r);
                out[i * 16 + 4..i * 16 + 8].copy_from_slice(&g);
                out[i * 16 + 8..i * 16 + 12].copy_from_slice(&b);
                out[i * 16 + 12..i * 16 + 16].copy_from_slice(&a);
            }
            out
        }
        (ColorMode::Cmyk, BitDepth::Eight, false) => {
            // In PSD CMYK, 0 = 100% ink and 255 = 0% ink (white).
            let mut out = alloc_zeroed(alloc_pref, true, out_bytes)?;
            let c_plane = &planes[0..plane_bytes];
            let m_plane = &planes[plane_bytes..plane_bytes * 2];
            let y_plane = &planes[plane_bytes * 2..plane_bytes * 3];
            let k_plane = &planes[plane_bytes * 3..plane_bytes * 4];
            for i in 0..num_pixels {
                let k = k_plane[i] as u32;
                out[i * 3] = ((c_plane[i] as u32 * k + 127) / 255) as u8;
                out[i * 3 + 1] = ((m_plane[i] as u32 * k + 127) / 255) as u8;
                out[i * 3 + 2] = ((y_plane[i] as u32 * k + 127) / 255) as u8;
            }
            out
        }
        (ColorMode::Cmyk, BitDepth::Eight, true) => {
            let mut out = alloc_zeroed(alloc_pref, true, out_bytes)?;
            let c_plane = &planes[0..plane_bytes];
            let m_plane = &planes[plane_bytes..plane_bytes * 2];
            let y_plane = &planes[plane_bytes * 2..plane_bytes * 3];
            let k_plane = &planes[plane_bytes * 3..plane_bytes * 4];
            let a_plane = &planes[plane_bytes * 4..plane_bytes * 5];
            for i in 0..num_pixels {
                let k = k_plane[i] as u32;
                out[i * 4] = ((c_plane[i] as u32 * k + 127) / 255) as u8;
                out[i * 4 + 1] = ((m_plane[i] as u32 * k + 127) / 255) as u8;
                out[i * 4 + 2] = ((y_plane[i] as u32 * k + 127) / 255) as u8;
                out[i * 4 + 3] = a_plane[i];
            }
            out
        }
        (ColorMode::Cmyk, BitDepth::Sixteen, false) => {
            let mut out = alloc_zeroed(alloc_pref, true, out_bytes)?;
            let c_plane = &planes[0..plane_bytes];
            let m_plane = &planes[plane_bytes..plane_bytes * 2];
            let y_plane = &planes[plane_bytes * 2..plane_bytes * 3];
            let k_plane = &planes[plane_bytes * 3..plane_bytes * 4];
            for i in 0..num_pixels {
                let j = i * 2;
                let c = u16::from_be_bytes([c_plane[j], c_plane[j + 1]]) as u64;
                let m = u16::from_be_bytes([m_plane[j], m_plane[j + 1]]) as u64;
                let y = u16::from_be_bytes([y_plane[j], y_plane[j + 1]]) as u64;
                let k = u16::from_be_bytes([k_plane[j], k_plane[j + 1]]) as u64;
                let r = (((c * k + 32767) / 65535) as u16).to_ne_bytes();
                let g = (((m * k + 32767) / 65535) as u16).to_ne_bytes();
                let b = (((y * k + 32767) / 65535) as u16).to_ne_bytes();
                out[i * 6..i * 6 + 2].copy_from_slice(&r);
                out[i * 6 + 2..i * 6 + 4].copy_from_slice(&g);
                out[i * 6 + 4..i * 6 + 6].copy_from_slice(&b);
            }
            out
        }
        (ColorMode::Cmyk, BitDepth::Sixteen, true) => {
            let mut out = alloc_zeroed(alloc_pref, true, out_bytes)?;
            let c_plane = &planes[0..plane_bytes];
            let m_plane = &planes[plane_bytes..plane_bytes * 2];
            let y_plane = &planes[plane_bytes * 2..plane_bytes * 3];
            let k_plane = &planes[plane_bytes * 3..plane_bytes * 4];
            let a_plane = &planes[plane_bytes * 4..plane_bytes * 5];
            for i in 0..num_pixels {
                let j = i * 2;
                let c = u16::from_be_bytes([c_plane[j], c_plane[j + 1]]) as u64;
                let m = u16::from_be_bytes([m_plane[j], m_plane[j + 1]]) as u64;
                let y = u16::from_be_bytes([y_plane[j], y_plane[j + 1]]) as u64;
                let k = u16::from_be_bytes([k_plane[j], k_plane[j + 1]]) as u64;
                let a = u16::from_be_bytes([a_plane[j], a_plane[j + 1]]).to_ne_bytes();
                let r = (((c * k + 32767) / 65535) as u16).to_ne_bytes();
                let g = (((m * k + 32767) / 65535) as u16).to_ne_bytes();
                let b = (((y * k + 32767) / 65535) as u16).to_ne_bytes();
                out[i * 8..i * 8 + 2].copy_from_slice(&r);
                out[i * 8 + 2..i * 8 + 4].copy_from_slice(&g);
                out[i * 8 + 4..i * 8 + 6].copy_from_slice(&b);
                out[i * 8 + 6..i * 8 + 8].copy_from_slice(&a);
            }
            out
        }
        _ => {
            return Err(at!(PsdError::UnsupportedColorMode(
                header.color_mode as u16
            )));
        }
    };

    PixelBuffer::from_vec(out, width, height, descriptor)
        .map_err(|e| at!(PsdError::PixelBuffer(alloc::format!("{e}"))))
}
