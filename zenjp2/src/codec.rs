//! zencodec trait implementations for JPEG 2000 decoding.
//!
//! Implements the three-tier decode hierarchy:
//! - [`Jp2DecoderConfig`] → [`zencodec::decode::DecoderConfig`]
//! - [`Jp2DecodeJob`] → [`zencodec::decode::DecodeJob`]
//! - [`Jp2Decoder`] → [`zencodec::decode::Decode`]

use alloc::borrow::Cow;
use alloc::sync::Arc;
use alloc::vec::Vec;

use whereat::At;
use zencodec::decode::{
    Decode, DecodeCapabilities, DecodeJob, DecodeOutput, DecodePolicy, DecodeRowSink,
    DecoderConfig, OutputInfo,
};
use zencodec::{CodecError, ImageFormat, ImageInfo, ResourceLimits, SharedPulse, Unsupported};
use zenpixels::{
    AlphaMode, ChannelOrder, ColorOrigin, ColorSpace, IccColorSpace, ImageSpec, PixelBuf,
    SampleLayout, SampleType, Transfer,
};

use crate::alloc_util::{AllocPref, alloc_zeroed, vec_with_capacity};
use crate::boxes::read_box;
use crate::error::Jp2Error;

/// Shorthand for this codec's zencodec-facing error type — the shared
/// envelope, located (Pattern B: `type Error = At<CodecError>`). A generic
/// consumer recovers the `ErrorCategory` from this type even after erasure to
/// `Box<dyn Error>` on the `Dyn*` dispatch path, by downcasting to it — see
/// `Jp2Error`'s `CategorizedError` impl and its bridge `From<Jp2Error> for
/// At<CodecError>` in `crate::error`.
type CodecResult<T> = core::result::Result<T, At<CodecError>>;

/// Lower the public zencodec [`AllocPreference`](zencodec::AllocPreference) onto
/// the crate-internal [`AllocPref`], keeping the decode core free of any
/// `zencodec` dependency. Any unrecognized (future, `#[non_exhaustive]`)
/// variant maps to [`AllocPref::CodecDefault`] (existing behavior).
fn alloc_pref_from_zencodec(pref: zencodec::AllocPreference) -> AllocPref {
    match pref {
        zencodec::AllocPreference::Fallible => AllocPref::Fallible,
        zencodec::AllocPreference::Infallible => AllocPref::Infallible,
        _ => AllocPref::CodecDefault,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Supported pixel descriptors
// ═══════════════════════════════════════════════════════════════════════════

/// Pixel formats the decoder can produce.
///
/// Eight-bit samples use a U8 layout by default. Higher precision uses U16
/// normalized to the full unsigned range. Embedded profiles determine the
/// active color independently of this list of representative formats.
static DECODE_DESCRIPTORS: &[ImageSpec] = &[
    ImageSpec::RGB8_SRGB,
    ImageSpec::RGBA8_SRGB,
    ImageSpec::GRAY8_SRGB,
    ImageSpec::GRAYA8_SRGB,
    ImageSpec::RGB16_SRGB,
    ImageSpec::RGBA16_SRGB,
    ImageSpec::GRAY16_SRGB,
    ImageSpec::new(
        SampleLayout::new(SampleType::U16, ChannelOrder::GrayAlpha),
        ColorSpace::Gray {
            transfer: Transfer::Srgb,
            range: zenpixels::SignalRange::Full,
            alpha: AlphaMode::Straight,
            white: None,
        },
    ),
];

// ═══════════════════════════════════════════════════════════════════════════
// Capabilities
// ═══════════════════════════════════════════════════════════════════════════

static JP2_DECODE_CAPS: DecodeCapabilities = DecodeCapabilities::new()
    .with_incremental(true)
    .with_icc(true)
    .with_cheap_probe(true)
    .with_inventory(cfg!(feature = "inventory"))
    .with_native_gray(true)
    .with_native_alpha(true)
    .with_stop(true) // polled before parsing and before decoding; hayro decodes without polling
    .with_enforces_max_pixels(true)
    .with_enforces_max_memory(true);

// ═══════════════════════════════════════════════════════════════════════════
// Source encoding details
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy)]
struct Jp2SourceEncoding;

impl zencodec::SourceEncodingDetails for Jp2SourceEncoding {
    fn source_generic_quality(&self) -> Option<f32> {
        // Cannot determine quality from decoded JP2
        None
    }

    fn is_lossless(&self) -> bool {
        // Cannot reliably determine lossless vs lossy after decode
        false
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// DecoderConfig (Tier 1)
// ═══════════════════════════════════════════════════════════════════════════

/// Reusable JPEG 2000 decoder configuration.
#[derive(Clone, Debug)]
pub struct Jp2DecoderConfig {
    settings: hayro_jpeg2000::DecodeSettings,
}

impl Jp2DecoderConfig {
    /// Create a new decoder config with default settings.
    pub fn new() -> Self {
        Self {
            settings: hayro_jpeg2000::DecodeSettings::default(),
        }
    }
}

impl Default for Jp2DecoderConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl DecoderConfig for Jp2DecoderConfig {
    type Error = At<CodecError>;
    type Job<'a> = Jp2DecodeJob;

    fn formats() -> &'static [ImageFormat] {
        static FORMATS: [ImageFormat; 1] = [ImageFormat::Jp2];
        &FORMATS
    }

    fn supported_descriptors() -> &'static [ImageSpec] {
        DECODE_DESCRIPTORS
    }

    fn capabilities() -> &'static DecodeCapabilities {
        &JP2_DECODE_CAPS
    }

    /// Uncalibrated structural decode estimate (no heaptrack model yet).
    ///
    /// A JPEG 2000 decode reconstructs the image through the inverse wavelet
    /// transform on a per-tile basis, then assembles the full output plane.
    /// Peak memory therefore holds the full output buffer (W × H × bytes per
    /// output pixel) concurrently with the wavelet/tile working set for the
    /// tile currently being inverted. The codestream's tiling is not known
    /// without parsing, so the working set is approximated as a fraction of the
    /// output buffer plus a small fixed overhead — a structural guess, not a
    /// measured model. Decode is single-threaded.
    fn estimate_decode_resources(
        &self,
        image: &zencodec::estimate::ImageCharacteristics,
        compute: &zencodec::estimate::ComputeEnvironment,
    ) -> zencodec::estimate::ResourceEstimate {
        use zencodec::estimate::{ResourceEstimate, ThreadingInformation};
        let bpp = image.descriptor().layout.bytes_per_pixel() as u64;
        // Full output pixel plane: W * H * bytes-per-output-pixel.
        let output = image.pixels().saturating_mul(bpp);
        // Wavelet/tile working set: hayro reconstructs tile-by-tile, holding
        // coefficient and intermediate sub-band buffers for the active tile.
        // Untiled codestreams hold roughly a full image-sized coefficient plane;
        // budget ~1× output for the working set plus a small fixed overhead.
        let scratch = 8u64 << 20; // tile/codestream parsing + sub-band scratch
        let typ = output.saturating_mul(2).saturating_add(scratch);
        // ~60 Mpix/s rough (uncalibrated, structural — wavelet decode is
        // heavier per pixel than a DCT or row-filter decode).
        let time_ms = (image.pixels() as f64 / 60_000.0) as u64;
        ResourceEstimate::new(typ, time_ms)
            .with_peak_max(typ.saturating_mul(2))
            .with_threading(ThreadingInformation::SERIAL)
            .at_cores(compute.cores())
    }

    fn job<'a>(self) -> Self::Job<'a> {
        Jp2DecodeJob {
            settings: self.settings,
            pulse: SharedPulse::default(),
            limits: None,
            policy: None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// DecodeJob (Tier 2)
// ═══════════════════════════════════════════════════════════════════════════

/// Per-operation JPEG 2000 decode job.
pub struct Jp2DecodeJob {
    settings: hayro_jpeg2000::DecodeSettings,
    pulse: SharedPulse,
    limits: Option<ResourceLimits>,
    policy: Option<DecodePolicy>,
}

impl<'a> DecodeJob<'a> for Jp2DecodeJob {
    type Error = At<CodecError>;
    type Dec = Jp2Decoder<'a>;
    type StreamDec = Unsupported<At<CodecError>>;
    type AnimationFrameDec = Unsupported<At<CodecError>>;

    fn with_pulse(mut self, pulse: SharedPulse) -> Self {
        self.pulse = pulse;
        self
    }

    fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = Some(limits);
        self
    }

    fn with_policy(mut self, policy: DecodePolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    fn probe(&self, data: &[u8]) -> core::result::Result<ImageInfo, Self::Error> {
        let image = hayro_jpeg2000::Image::new(data, &self.settings).map_err(Jp2Error::Decode)?;

        let mut info = image_to_info(&image);
        apply_policy_to_info(&mut info, self.policy.as_ref());
        Ok(info)
    }

    #[cfg(feature = "inventory")]
    fn inventory(
        &self,
        data: &[u8],
    ) -> core::result::Result<zencodec::inventory::Inventory, At<CodecError>> {
        use whereat::ErrorAtExt;
        use zencodec::CategorizedError;
        if let Some(limits) = &self.limits {
            limits
                .check_input_size(data.len() as u64)
                .map_err(Jp2Error::Limit)?;
        }
        let stop: &dyn enough::Stop = &self.pulse;
        let keep_icc = self.policy.as_ref().is_none_or(|p| p.resolve_icc(true));
        crate::inventory::inventory(data, stop, keep_icc).map_err(|e| {
            CodecError::from_parts(Some("zenjp2"), e.category(), alloc::boxed::Box::new(e))
                .start_at()
        })
    }

    fn output_info(&self, data: &[u8]) -> core::result::Result<OutputInfo, Self::Error> {
        let image = hayro_jpeg2000::Image::new(data, &self.settings).map_err(Jp2Error::Decode)?;
        let info = image_to_info(&image);
        let bits = output_precision(data, image.original_bit_depth());
        let spec = pixel_spec(&image, &info, bits, &[]);
        Ok(OutputInfo::full_decode(image.width(), image.height(), spec))
    }

    fn push_decoder(
        self,
        data: Cow<'a, [u8]>,
        sink: &mut dyn DecodeRowSink,
        preferred: &[ImageSpec],
    ) -> core::result::Result<OutputInfo, Self::Error> {
        // JP2 doesn't support streaming — decode full image, then copy to sink.
        zencodec::helpers::copy_decode_to_sink(self, data, sink, preferred, |e| {
            Jp2Error::Sink(e).into()
        })
    }

    fn decoder(
        self,
        data: Cow<'a, [u8]>,
        preferred: &[ImageSpec],
    ) -> core::result::Result<Self::Dec, Self::Error> {
        // Pre-flight limit checks on input size
        if let Some(ref limits) = self.limits
            && let Some(max_input) = limits.max_input_bytes
        {
            let actual = data.len() as u64;
            if actual > max_input {
                return Err(Jp2Error::Limit(zencodec::LimitExceeded::InputSize {
                    actual,
                    max: max_input,
                })
                .into());
            }
        }
        check_pulse(&self.pulse)?;

        let alloc_pref = self
            .limits
            .as_ref()
            .map(|l| alloc_pref_from_zencodec(l.prefer_fallible_allocations))
            .unwrap_or_default();

        Ok(Jp2Decoder {
            data,
            settings: self.settings,
            limits: self.limits,
            policy: self.policy,
            alloc_pref,
            preferred: preferred.iter().map(|spec| spec.layout).collect(),
            pulse: self.pulse,
        })
    }

    fn streaming_decoder(
        self,
        _data: Cow<'a, [u8]>,
        _preferred: &[ImageSpec],
    ) -> core::result::Result<Self::StreamDec, Self::Error> {
        Err(Jp2Error::UnsupportedOperation(zencodec::UnsupportedOperation::RowLevelDecode).into())
    }

    fn animation_frame_decoder(
        self,
        _data: Cow<'a, [u8]>,
        _preferred: &[ImageSpec],
    ) -> core::result::Result<Self::AnimationFrameDec, Self::Error> {
        Err(Jp2Error::UnsupportedOperation(zencodec::UnsupportedOperation::AnimationDecode).into())
    }

    /// Incremental (sans-I/O) decode: the header is reported as soon as it
    /// probes, and the image decodes through `push_decoder` once the whole
    /// file is offered. The JPEG 2000 decoder reads its codestream from a complete buffer.
    fn dyn_incremental_decoder(
        self,
        preferred: &[ImageSpec],
    ) -> Result<
        alloc::boxed::Box<zencodec::decode::DynIncrementalDecoder>,
        zencodec::decode::BoxedError,
    > {
        Ok(zencodec::decode::erase_incremental(
            zencodec::helpers::WholeFileIncremental::new(self, preferred),
        ))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Decode (Tier 3)
// ═══════════════════════════════════════════════════════════════════════════

/// Single-image JPEG 2000 decoder.
pub struct Jp2Decoder<'a> {
    data: Cow<'a, [u8]>,
    settings: hayro_jpeg2000::DecodeSettings,
    limits: Option<ResourceLimits>,
    policy: Option<DecodePolicy>,
    alloc_pref: AllocPref,
    preferred: Vec<SampleLayout>,
    /// The job's pulse, polled when the decode starts and again once the
    /// header is parsed, before the codestream is decoded.
    pulse: SharedPulse,
}

impl Decode for Jp2Decoder<'_> {
    type Error = At<CodecError>;

    fn decode(self) -> core::result::Result<DecodeOutput, Self::Error> {
        check_pulse(&self.pulse)?;
        let image =
            hayro_jpeg2000::Image::new(&self.data, &self.settings).map_err(Jp2Error::Decode)?;

        let mut info = image_to_info(&image);
        let header_bits = output_precision(&self.data, image.original_bit_depth());
        let descriptor = pixel_spec(&image, &info, header_bits, &self.preferred);
        check_limits(&image, self.limits.as_ref(), &descriptor)?;
        check_pulse(&self.pulse)?;

        let mut ctx = hayro_jpeg2000::DecoderContext::default();
        let decoded = image.decode(&mut ctx).map_err(Jp2Error::Decode)?;
        let components = decoded.components();
        let bits = components
            .iter()
            .map(|c| c.bit_depth())
            .max()
            .ok_or_else(|| {
                Jp2Error::PixelBuffer(alloc::string::String::from(
                    "decoded JPEG 2000 has no components",
                ))
            })?;
        let descriptor = pixel_spec(&image, &info, bits, &self.preferred);
        check_limits(&image, self.limits.as_ref(), &descriptor)?;
        let pixel_buffer = interleave(
            &decoded,
            image.width(),
            image.height(),
            descriptor,
            self.alloc_pref,
        )?
        .with_origin(
            info.source_color
                .icc_profile
                .clone()
                .map_or_else(ColorOrigin::assumed, ColorOrigin::from_icc),
        );

        apply_policy_to_info(&mut info, self.policy.as_ref());

        Ok(DecodeOutput::new(pixel_buffer, info).with_source_encoding_details(Jp2SourceEncoding))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Build an [`ImageInfo`] from a parsed (but not decoded) JPEG 2000 image.
fn image_to_info(image: &hayro_jpeg2000::Image<'_>) -> ImageInfo {
    let mut info = ImageInfo::new(image.width(), image.height(), ImageFormat::Jp2)
        .with_alpha(image.has_alpha());

    // Attach ICC profile if the color space carries one
    if let hayro_jpeg2000::ColorSpace::Icc { profile, .. } = image.color_space() {
        info.source_color.icc_profile = Some(Arc::from(profile.as_slice()));
    }

    info
}

/// Determine output precision without decoding. Hayro's header accessor
/// reports the first codestream component, even when a palette supplies the
/// actual output. Validate component counts and mapping offsets as we read.
fn output_precision(data: &[u8], fallback: u8) -> u8 {
    fn boxes(data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
        let mut pos = 0;
        core::iter::from_fn(move || {
            let b = read_box(data, pos, data.len() as u64).ok()?;
            pos = b.end;
            let body = b.payload();
            Some((b.ty, &data[body.start as usize..body.end as usize]))
        })
    }

    fn component_precisions(codestream: &[u8]) -> impl Iterator<Item = u8> {
        let count = codestream
            .get(40..42)
            .map_or(0, |c| u16::from_be_bytes([c[0], c[1]]) as usize);
        codestream
            .get(42..)
            .unwrap_or_default()
            .as_chunks::<3>()
            .0
            .iter()
            .take(count)
            .map(|c| (c[0] & 0x7f) + 1)
    }

    if data.starts_with(&[0xff, 0x4f]) {
        return component_precisions(data).max().unwrap_or(fallback);
    }
    let mut header = &[][..];
    let mut codestream = &[][..];
    for (ty, body) in boxes(data) {
        match &ty {
            b"jp2h" => header = body,
            b"jp2c" => codestream = body,
            _ => {}
        }
    }

    let mut palette = None;
    let mut mapping = None;
    for (ty, body) in boxes(header) {
        match &ty {
            b"pclr" => {
                let columns = body.get(2).copied().unwrap_or(0) as usize;
                palette = body.get(3..3 + columns);
            }
            b"cmap" => mapping = Some(body.as_chunks::<4>().0),
            _ => {}
        }
    }
    let Some(palette) = palette else {
        return component_precisions(codestream).max().unwrap_or(fallback);
    };
    let palette_depth = |column: &u8| (column & 0x7f) + 1;
    if let Some(mapping) = mapping {
        mapping
            .iter()
            .filter_map(|&[hi, lo, method, column]| match method {
                0 => component_precisions(codestream).nth(u16::from_be_bytes([hi, lo]) as usize),
                1 => palette.get(usize::from(column)).map(palette_depth),
                _ => None,
            })
            .max()
            .unwrap_or(fallback)
    } else {
        palette.iter().map(palette_depth).max().unwrap_or(fallback)
    }
}

fn active_icc(profile: &[u8]) -> Option<IccColorSpace> {
    let declared = u32::from_be_bytes(profile.get(..4)?.try_into().ok()?) as usize;
    let tags = u32::from_be_bytes(profile.get(128..132)?.try_into().ok()?) as usize;
    if profile.get(36..40)? != b"acsp"
        || declared > profile.len()
        || tags == 0
        || tags > declared.saturating_sub(132) / 12
    {
        return None;
    }
    IccColorSpace::from_icc(profile)
}

/// Describe the interleaved raster produced by the adapter, including color
/// meaning without applying any transform to ICC-based components.
fn pixel_spec(
    image: &hayro_jpeg2000::Image<'_>,
    info: &ImageInfo,
    bits: u8,
    preferred: &[SampleLayout],
) -> ImageSpec {
    use hayro_jpeg2000::ColorSpace as SourceSpace;
    let source = image.color_space();
    let channels = source.num_channels();
    let has_alpha = image.has_alpha();
    let profile = info.source_color.icc_profile.as_ref();
    let icc = profile.and_then(|p| active_icc(p));
    let order = match (channels, has_alpha, icc) {
        (1, false, _) => ChannelOrder::Gray,
        (1, true, Some(IccColorSpace::Gray)) => ChannelOrder::GrayAlpha,
        (1 | 3, true, _) => ChannelOrder::Rgba,
        (3, false, _) => ChannelOrder::Rgb,
        (4, false, _) => ChannelOrder::Cmyk,
        (4, true, _) => ChannelOrder::Cmyka,
        _ => ChannelOrder::Generic(channels + u8::from(has_alpha)),
    };
    let sample = preferred
        .iter()
        .find_map(|layout| {
            (layout.order == order && matches!(layout.sample, SampleType::U8 | SampleType::U16))
                .then_some(layout.sample)
        })
        .unwrap_or(if bits > 8 {
            SampleType::U16
        } else {
            SampleType::U8
        });
    let alpha = if has_alpha {
        AlphaMode::Straight
    } else {
        AlphaMode::None
    };
    let color = match (icc, profile) {
        (Some(IccColorSpace::Gray), Some(profile)) if order.is_gray_family() => ColorSpace::Icc {
            profile: profile.clone(),
            channels: 1,
            alpha,
            intent: Default::default(),
        },
        (Some(IccColorSpace::Rgb), Some(profile)) if order.is_rgb_family() => ColorSpace::Icc {
            profile: profile.clone(),
            channels: 3,
            alpha,
            intent: Default::default(),
        },
        (_, _) if order.is_cmyk_family() => ColorSpace::Cmyk {
            inverted: false,
            alpha,
            icc: profile
                .filter(|_| icc == Some(IccColorSpace::Cmyk))
                .cloned(),
        },
        (_, _) if order.is_gray_family() => {
            ColorSpace::GRAY_SRGB
                .with_alpha(alpha)
                .with_transfer(if profile.is_some() {
                    Transfer::Unknown
                } else {
                    Transfer::Srgb
                })
        }
        (_, _) if order.is_rgb_family() => {
            // Hayro already converts enumerated Lab to RGB. An opaque/malformed
            // embedded ICC is provenance only and does not imply an sRGB TRC.
            let transfer =
                if matches!(source, SourceSpace::Icc { .. }) && icc != Some(IccColorSpace::Lab) {
                    Transfer::Unknown
                } else {
                    Transfer::Srgb
                };
            ColorSpace::SRGB.with_alpha(alpha).with_transfer(transfer)
        }
        _ => ColorSpace::Untagged {
            channels: order.channels() as u8,
            alpha,
        },
    };
    ImageSpec::new(SampleLayout::new(sample, order), color)
}

fn interleave(
    decoded: &hayro_jpeg2000::DecodedImage<'_>,
    width: u32,
    height: u32,
    spec: ImageSpec,
    alloc_pref: AllocPref,
) -> CodecResult<PixelBuf> {
    let components = decoded.components();
    let sample_count = components[0].samples().len();
    let channels = spec.layout.channels();
    let expand_gray = components.len() == 2 && spec.layout.order == ChannelOrder::Rgba;
    if components.iter().any(|c| c.samples().len() != sample_count)
        || (!expand_gray && channels != components.len())
    {
        return Err(Jp2Error::PixelBuffer("unequal or unexpected component layout".into()).into());
    }
    let count = sample_count
        .checked_mul(channels)
        .ok_or_else(|| Jp2Error::PixelBuffer("pixel buffer size overflow".into()))?;
    let order = if expand_gray {
        &[0, 0, 0, 1][..]
    } else {
        &[][..]
    };
    let indices = (0..channels).map(|c| order.get(c).copied().unwrap_or(c));

    let result = if spec.layout.sample == SampleType::U8
        && !expand_gray
        && components.iter().all(|c| c.bit_depth() < 32)
    {
        let mut raw = alloc_zeroed(alloc_pref, true, count).map_err(CodecError::of)?;
        decoded.store_u8_into(&mut raw);
        PixelBuf::from_typed(raw, width, height, spec)
    } else {
        // Use f64 so an integer code through 16 bits rounds to the exact
        // full-range U16 grid, e.g. round(code * 65535 / 4095) for 12-bit data.
        let scale: Vec<_> = components
            .iter()
            .map(|c| 1.0 / (2.0f64.powi(i32::from(c.bit_depth())) - 1.0))
            .collect();
        let samples = (0..sample_count).flat_map(|i| {
            indices.clone().map({
                let scale = &scale;
                move |c| f64::from(components[c].samples()[i]) * scale[c]
            })
        });
        if spec.layout.sample == SampleType::U16 {
            let mut raw = vec_with_capacity(alloc_pref, true, count).map_err(CodecError::of)?;
            raw.extend(samples.map(|s| (s * 65535.0).round() as u16));
            PixelBuf::from_typed(raw, width, height, spec)
        } else {
            let mut raw = vec_with_capacity(alloc_pref, true, count).map_err(CodecError::of)?;
            raw.extend(samples.map(|s| (s * 255.0).round() as u8));
            PixelBuf::from_typed(raw, width, height, spec)
        }
    };
    result.map_err(|err| Jp2Error::PixelBuffer(alloc::format!("{err}")).into())
}

/// Poll the job's pulse; a stop keeps its reason.
fn check_pulse(stop: &dyn enough::Stop) -> core::result::Result<(), At<CodecError>> {
    use whereat::ErrorAtExt;
    stop.check()
        .map_err(|reason| CodecError::new(Some("zenjp2"), reason.into()).start_at())
}

/// Check resource limits before decoding. Each cap maps to a distinct
/// [`zencodec::LimitKind`] via the typed [`zencodec::LimitExceeded`] — these
/// previously collapsed into one untyped string (2026-07-13 caterr audit
/// finding #2).
fn check_limits(
    image: &hayro_jpeg2000::Image<'_>,
    limits: Option<&ResourceLimits>,
    descriptor: &ImageSpec,
) -> CodecResult<()> {
    let Some(limits) = limits else {
        return Ok(());
    };

    let width = image.width();
    let height = image.height();
    let pixels = (width as u64).saturating_mul(height as u64);

    if let Some(max_w) = limits.max_width
        && width > max_w
    {
        return Err(Jp2Error::Limit(zencodec::LimitExceeded::Width {
            actual: width,
            max: max_w,
        })
        .into());
    }
    if let Some(max_h) = limits.max_height
        && height > max_h
    {
        return Err(Jp2Error::Limit(zencodec::LimitExceeded::Height {
            actual: height,
            max: max_h,
        })
        .into());
    }
    if let Some(max_px) = limits.max_pixels
        && pixels > max_px
    {
        return Err(Jp2Error::Limit(zencodec::LimitExceeded::Pixels {
            actual: pixels,
            max: max_px,
        })
        .into());
    }
    if let Some(max_mem) = limits.max_memory_bytes {
        // Preserve the existing four-byte minimum and account for wider output.
        let bytes_per_pixel = descriptor.layout.bytes_per_pixel().max(4) as u64;
        let estimated_memory = pixels.saturating_mul(bytes_per_pixel);
        if estimated_memory > max_mem {
            return Err(Jp2Error::Limit(zencodec::LimitExceeded::Memory {
                actual: estimated_memory,
                max: max_mem,
            })
            .into());
        }
    }

    Ok(())
}

/// Suppress metadata fields based on decode policy.
fn apply_policy_to_info(info: &mut ImageInfo, policy: Option<&DecodePolicy>) {
    let Some(policy) = policy else { return };
    if policy.allow_icc == Some(false) {
        info.source_color.icc_profile = None;
    }
    if policy.allow_exif == Some(false) {
        info.embedded_metadata.exif = None;
    }
    if policy.allow_xmp == Some(false) {
        info.embedded_metadata.xmp = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_returns_jp2_format() {
        let formats = Jp2DecoderConfig::formats();
        assert_eq!(formats.len(), 1);
        assert_eq!(formats[0], ImageFormat::Jp2);
    }

    #[test]
    fn capabilities_advertised() {
        let caps = Jp2DecoderConfig::capabilities();
        assert!(caps.cheap_probe());
        assert!(caps.native_gray());
        assert!(caps.native_alpha());
    }

    #[test]
    fn unsupported_streaming() {
        let config = Jp2DecoderConfig::new();
        let job = config.job();
        let result = job.streaming_decoder(Cow::Borrowed(&[]), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn unsupported_animation() {
        let config = Jp2DecoderConfig::new();
        let job = config.job();
        let result = job.animation_frame_decoder(Cow::Borrowed(&[]), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn input_size_limit_enforced() {
        let config = Jp2DecoderConfig::new();
        let mut limits = ResourceLimits::default();
        limits.max_input_bytes = Some(10);
        let job = config.job().with_limits(limits);
        let data = vec![0u8; 100];
        let result = job.decoder(Cow::Owned(data), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn format_detection() {
        // JP2 container signature
        assert!(crate::is_jpeg2000(b"\x00\x00\x00\x0C\x6A\x50\x20\x20"));
        // Raw J2K codestream (SOC + SIZ)
        assert!(crate::is_jpeg2000(b"\xFF\x4F\xFF\x51"));
        assert!(!crate::is_jpeg2000(b"not jp2"));
        assert!(!crate::is_jpeg2000(b""));
    }

    #[test]
    fn estimate_decode_resources_scales_with_output() {
        use zencodec::estimate::{ComputeEnvironment, ImageCharacteristics, ThreadingInformation};
        let img = ImageCharacteristics::new(1000, 1000, ImageSpec::RGBA8_SRGB);
        let env = ComputeEnvironment::conservative().with_cores(8);
        let est = Jp2DecoderConfig::new().estimate_decode_resources(&img, &env);
        // Peak holds output (1000*1000*4 = 4 MB) at least twice plus scratch.
        let output = 1000u64 * 1000 * 4;
        assert!(est.peak_memory_bytes_est().unwrap() >= output.saturating_mul(2));
        assert!(est.peak_memory_bytes_max().unwrap() >= est.peak_memory_bytes_est().unwrap());
        assert!(est.wall_ms().is_some());
        // Decode is serial → at_cores does not divide wall time below 1 thread.
        assert_eq!(est.threading(), Some(ThreadingInformation::SERIAL));
    }

    /// The 3-mode allocation preference lowers cleanly at the decode boundary
    /// in every mode and produces a valid decoder. zenjp2's output buffer is
    /// hayro-owned, so the preference is a no-op for pixel output, but the
    /// boundary plumbing must stay sound under all three modes. A real
    /// byte-identity round trip needs a decodable JP2 fixture, which is not
    /// available in-tree (zenjp2 is decode-only and there is no JP2 encoder in
    /// the workspace); the helper-level byte identity is covered by
    /// `crate::alloc_util`'s `alloc_zeroed_all_modes_equal_bytes`.
    #[test]
    fn alloc_pref_boundary_lowers_in_all_modes() {
        use zencodec::AllocPreference;
        for pref in [
            AllocPreference::CodecDefault,
            AllocPreference::Fallible,
            AllocPreference::Infallible,
        ] {
            let limits = ResourceLimits::none().with_prefer_fallible_allocations(pref);
            // A non-JP2 payload that still passes the input-size gate: decoder()
            // construction must succeed (it does not parse the codestream), so
            // the lowered preference is exercised on a valid decoder in each
            // mode without depending on a real fixture.
            let job = Jp2DecoderConfig::new().job().with_limits(limits);
            let data = vec![0u8; 64];
            let dec = job
                .decoder(Cow::Owned(data), &[])
                .expect("decoder construction succeeds under every alloc mode");
            let expected = match pref {
                AllocPreference::Fallible => AllocPref::Fallible,
                AllocPreference::Infallible => AllocPref::Infallible,
                _ => AllocPref::CodecDefault,
            };
            assert_eq!(dec.alloc_pref, expected);
        }
    }
}
