//! `zencodec` trait implementations for PSD/PSB decoding.
//!
//! Implements the three-tier decode hierarchy:
//! - [`PsdDecoderConfig`] → [`zencodec::decode::DecoderConfig`]
//! - [`PsdDecodeJob`] → [`zencodec::decode::DecodeJob`]
//! - [`PsdDecoder`] → [`zencodec::decode::Decode`]

use alloc::borrow::Cow;
use alloc::sync::Arc;

use whereat::At;
use zencodec::decode::{
    Decode, DecodeCapabilities, DecodeJob, DecodeOutput, DecodePolicy, DecodeRowSink,
    DecoderConfig, OutputInfo,
};
use zencodec::{CodecError, ImageFormat, ImageInfo, ResourceLimits, StopToken, Unsupported};
use zenpixels::PixelDescriptor;

use crate::alloc_util::AllocPref;
use crate::error::PsdError;
use crate::format::psd_format;
use crate::parser::{self, PsdHeader, PsdInfo};

type CodecResult<T> = core::result::Result<T, At<CodecError>>;

fn alloc_pref_from_zencodec(pref: zencodec::AllocPreference) -> AllocPref {
    match pref {
        zencodec::AllocPreference::Fallible => AllocPref::Fallible,
        zencodec::AllocPreference::Infallible => AllocPref::Infallible,
        _ => AllocPref::CodecDefault,
    }
}

static DECODE_DESCRIPTORS: &[PixelDescriptor] = &[
    PixelDescriptor::RGB8_SRGB,
    PixelDescriptor::RGBA8_SRGB,
    PixelDescriptor::GRAY8_SRGB,
    PixelDescriptor::GRAYA8_SRGB,
    PixelDescriptor::RGB16_SRGB,
    PixelDescriptor::RGBA16_SRGB,
    PixelDescriptor::GRAY16_SRGB,
    PixelDescriptor::GRAYA16_SRGB,
    PixelDescriptor::RGBF32_LINEAR,
    PixelDescriptor::RGBAF32_LINEAR,
    PixelDescriptor::GRAYF32_LINEAR,
];

static PSD_DECODE_CAPS: DecodeCapabilities = DecodeCapabilities::new()
    .with_incremental(true)
    .with_icc(true)
    .with_exif(true)
    .with_xmp(true)
    .with_cheap_probe(true)
    .with_inventory(cfg!(feature = "inventory"))
    .with_native_gray(true)
    .with_native_alpha(true)
    .with_native_16bit(true)
    .with_native_f32(true)
    .with_enforces_max_pixels(true)
    .with_enforces_max_memory(true)
    .with_enforces_max_input_bytes(true);

#[derive(Debug, Clone, Copy)]
struct PsdSourceEncoding;

impl zencodec::SourceEncodingDetails for PsdSourceEncoding {
    fn source_generic_quality(&self) -> Option<f32> {
        None
    }

    fn is_lossless(&self) -> bool {
        true
    }
}

/// Reusable PSD/PSB decoder configuration.
#[derive(Clone, Debug, Default)]
pub struct PsdDecoderConfig {
    /// Reject PSD files whose `VersionInfo` (`0x0421`) explicitly sets `hasRealMergedData = false`.
    pub require_real_merged_data: bool,
}

impl PsdDecoderConfig {
    /// Create a new PSD decoder configuration with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Require `hasRealMergedData != false` in `VersionInfo` (`0x0421`).
    pub fn with_require_real_merged_data(mut self, require: bool) -> Self {
        self.require_real_merged_data = require;
        self
    }
}

impl DecoderConfig for PsdDecoderConfig {
    type Error = At<CodecError>;
    type Job<'a> = PsdDecodeJob;

    fn formats() -> &'static [ImageFormat] {
        static FORMATS: std::sync::LazyLock<[ImageFormat; 1]> =
            std::sync::LazyLock::new(|| [psd_format()]);
        &*FORMATS
    }

    fn supported_descriptors() -> &'static [PixelDescriptor] {
        DECODE_DESCRIPTORS
    }

    fn capabilities() -> &'static DecodeCapabilities {
        &PSD_DECODE_CAPS
    }

    fn estimate_decode_resources(
        &self,
        image: &zencodec::estimate::ImageCharacteristics,
        compute: &zencodec::estimate::ComputeEnvironment,
    ) -> zencodec::estimate::ResourceEstimate {
        use zencodec::estimate::{ResourceEstimate, ThreadingInformation};
        let bpp = image.descriptor().bytes_per_pixel() as u64;
        let output = image.pixels().saturating_mul(bpp);
        // Planar channel buffer + interleaved output buffer.
        let typ = output.saturating_mul(2).saturating_add(64 << 10);
        let time_ms = (image.pixels() as f64 / 200_000.0) as u64;
        ResourceEstimate::new(typ, time_ms)
            .with_peak_max(typ.saturating_mul(2))
            .with_threading(ThreadingInformation::SERIAL)
            .at_cores(compute.cores())
    }

    fn job<'a>(self) -> Self::Job<'a> {
        PsdDecodeJob {
            config: self,
            stop: None,
            limits: None,
            policy: None,
        }
    }
}

/// Per-operation PSD/PSB decode job.
pub struct PsdDecodeJob {
    config: PsdDecoderConfig,
    stop: Option<StopToken>,
    limits: Option<ResourceLimits>,
    policy: Option<DecodePolicy>,
}

impl<'a> DecodeJob<'a> for PsdDecodeJob {
    type Error = At<CodecError>;
    type Dec = PsdDecoder<'a>;
    type StreamDec = Unsupported<At<CodecError>>;
    type AnimationFrameDec = Unsupported<At<CodecError>>;

    fn with_stop(mut self, stop: StopToken) -> Self {
        self.stop = Some(stop);
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

    fn probe(&self, data: &[u8]) -> CodecResult<ImageInfo> {
        if let Some(ref limits) = self.limits {
            limits
                .check_input_size(data.len() as u64)
                .map_err(PsdError::Limit)?;
        }
        let layout = parser::parse_layout(data).map_err(CodecError::of)?;
        let strict = self.config.require_real_merged_data
            || self
                .policy
                .as_ref()
                .is_some_and(|p| p.resolve_strict(false));
        if strict {
            if !layout.reserved_zero {
                return Err(PsdError::Malformed("non-zero reserved header bytes").into());
            }
            if layout.info.metadata.has_real_merged_data == Some(false) {
                return Err(PsdError::MissingMergedData.into());
            }
        }
        check_limits(&layout.info.header, &layout.info, self.limits.as_ref())?;
        let mut info = psd_info_to_image_info(&layout.info);
        apply_policy_to_info(&mut info, self.policy.as_ref());
        Ok(info)
    }

    #[cfg(feature = "inventory")]
    fn inventory(&self, data: &[u8]) -> CodecResult<zencodec::inventory::Inventory> {
        use whereat::ErrorAtExt;
        use zencodec::CategorizedError;
        if let Some(ref limits) = self.limits {
            limits
                .check_input_size(data.len() as u64)
                .map_err(PsdError::Limit)?;
        }
        let stop: &dyn enough::Stop = self
            .stop
            .as_ref()
            .map_or(&enough::Unstoppable as &dyn enough::Stop, |s| s);
        let strict = self.config.require_real_merged_data
            || self
                .policy
                .as_ref()
                .is_some_and(|p| p.resolve_strict(false));
        let keep_icc = self.policy.as_ref().is_none_or(|p| p.resolve_icc(true));
        let keep_exif = self.policy.as_ref().is_none_or(|p| p.resolve_exif(true));
        let keep_xmp = self.policy.as_ref().is_none_or(|p| p.resolve_xmp(true));
        crate::inventory::inventory(
            data,
            stop,
            self.limits.as_ref(),
            strict,
            keep_icc,
            keep_exif,
            keep_xmp,
        )
        .map_err(|e| match e {
            crate::inventory::InvErr::Stopped(r) => PsdError::Stopped(r).into(),
            crate::inventory::InvErr::Inventory(ie) => {
                CodecError::from_parts(Some("zenpsd"), ie.category(), alloc::boxed::Box::new(ie))
                    .start_at()
            }
        })
    }

    fn output_info(&self, data: &[u8]) -> CodecResult<OutputInfo> {
        let layout = parser::parse_layout(data).map_err(CodecError::of)?;
        check_limits(&layout.info.header, &layout.info, self.limits.as_ref())?;
        let descriptor = layout.info.header.output_descriptor(&layout.info.metadata);
        Ok(OutputInfo::full_decode(
            layout.info.header.width,
            layout.info.header.height,
            descriptor,
        ))
    }

    fn push_decoder(
        self,
        data: Cow<'a, [u8]>,
        sink: &mut dyn DecodeRowSink,
        preferred: &[PixelDescriptor],
    ) -> CodecResult<OutputInfo> {
        zencodec::helpers::copy_decode_to_sink(self, data, sink, preferred, |e| {
            PsdError::Sink(e).into()
        })
    }

    fn decoder(
        self,
        data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> CodecResult<Self::Dec> {
        if let Some(ref limits) = self.limits {
            limits
                .check_input_size(data.len() as u64)
                .map_err(PsdError::Limit)?;
        }
        let alloc_pref = self
            .limits
            .as_ref()
            .map(|l| alloc_pref_from_zencodec(l.prefer_fallible_allocations))
            .unwrap_or_default();

        Ok(PsdDecoder {
            data,
            config: self.config,
            stop: self.stop,
            limits: self.limits,
            policy: self.policy,
            alloc_pref,
        })
    }

    fn streaming_decoder(
        self,
        _data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> CodecResult<Self::StreamDec> {
        Err(PsdError::UnsupportedOperation(zencodec::UnsupportedOperation::RowLevelDecode).into())
    }

    fn animation_frame_decoder(
        self,
        _data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> CodecResult<Self::AnimationFrameDec> {
        Err(PsdError::UnsupportedOperation(zencodec::UnsupportedOperation::AnimationDecode).into())
    }

    fn dyn_incremental_decoder(
        self,
        preferred: &[PixelDescriptor],
    ) -> Result<
        alloc::boxed::Box<zencodec::decode::DynIncrementalDecoder>,
        zencodec::decode::BoxedError,
    > {
        Ok(zencodec::decode::erase_incremental(
            zencodec::helpers::WholeFileIncremental::new(self, preferred),
        ))
    }
}

/// Single-image PSD/PSB decoder.
pub struct PsdDecoder<'a> {
    data: Cow<'a, [u8]>,
    config: PsdDecoderConfig,
    stop: Option<StopToken>,
    limits: Option<ResourceLimits>,
    policy: Option<DecodePolicy>,
    alloc_pref: AllocPref,
}

impl Decode for PsdDecoder<'_> {
    type Error = At<CodecError>;

    fn decode(self) -> CodecResult<DecodeOutput> {
        let stop: &dyn enough::Stop = self
            .stop
            .as_ref()
            .map_or(&enough::Unstoppable as &dyn enough::Stop, |s| s);
        stop.check().map_err(PsdError::Stopped)?;

        let layout = parser::parse_layout(&self.data).map_err(CodecError::of)?;
        let strict = self.config.require_real_merged_data
            || self
                .policy
                .as_ref()
                .is_some_and(|p| p.resolve_strict(false));
        if strict {
            if !layout.reserved_zero {
                return Err(PsdError::Malformed("non-zero reserved header bytes").into());
            }
            if layout.info.metadata.has_real_merged_data == Some(false) {
                return Err(PsdError::MissingMergedData.into());
            }
        }
        check_limits(&layout.info.header, &layout.info, self.limits.as_ref())?;

        let (pixels, _end) =
            parser::decode_merged_image(&self.data, &layout, stop, self.alloc_pref)
                .map_err(CodecError::of)?;

        let mut info = psd_info_to_image_info(&layout.info);
        apply_policy_to_info(&mut info, self.policy.as_ref());

        Ok(DecodeOutput::new(pixels, info).with_source_encoding_details(PsdSourceEncoding))
    }
}

pub(crate) fn check_limits(
    header: &PsdHeader,
    info: &PsdInfo,
    limits: Option<&ResourceLimits>,
) -> CodecResult<()> {
    let Some(limits) = limits else {
        return Ok(());
    };
    let width = header.width;
    let height = header.height;
    let pixels = (width as u64).saturating_mul(height as u64);

    if let Some(max_w) = limits.max_width
        && width > max_w
    {
        return Err(PsdError::Limit(zencodec::LimitExceeded::Width {
            actual: width,
            max: max_w,
        })
        .into());
    }
    if let Some(max_h) = limits.max_height
        && height > max_h
    {
        return Err(PsdError::Limit(zencodec::LimitExceeded::Height {
            actual: height,
            max: max_h,
        })
        .into());
    }
    if let Some(max_px) = limits.max_pixels
        && pixels > max_px
    {
        return Err(PsdError::Limit(zencodec::LimitExceeded::Pixels {
            actual: pixels,
            max: max_px,
        })
        .into());
    }
    if let Some(max_mem) = limits.max_memory_bytes {
        let bpp = header.output_descriptor(&info.metadata).bytes_per_pixel() as u64;
        let row_bytes = header.depth.scanline_bytes(width).unwrap_or(0) as u64;
        let plane_mem = row_bytes
            .saturating_mul(height as u64)
            .saturating_mul(header.used_channels() as u64);
        let out_mem = pixels.saturating_mul(bpp);
        let estimated = plane_mem.saturating_add(out_mem);
        if estimated > max_mem {
            return Err(PsdError::Limit(zencodec::LimitExceeded::Memory {
                actual: estimated,
                max: max_mem,
            })
            .into());
        }
    }
    Ok(())
}

fn psd_info_to_image_info(psd: &PsdInfo) -> ImageInfo {
    let has_alpha = psd.header.has_alpha(&psd.metadata);
    let mut info =
        ImageInfo::new(psd.header.width, psd.header.height, psd_format()).with_alpha(has_alpha);
    info.source_color.bit_depth = Some(psd.header.depth.bits());
    info.source_color.channel_count = Some(psd.header.used_channels() as u8);
    if let Some(ref icc) = psd.metadata.icc_profile {
        info.source_color.icc_profile = Some(Arc::from(icc.as_slice()));
    }
    if let Some(ref exif) = psd.metadata.exif {
        info.embedded_metadata.exif = Some(Arc::from(exif.as_slice()));
    }
    if let Some(ref xmp) = psd.metadata.xmp {
        info.embedded_metadata.xmp = Some(Arc::from(xmp.as_slice()));
    }
    info
}

fn apply_policy_to_info(info: &mut ImageInfo, policy: Option<&DecodePolicy>) {
    let Some(policy) = policy else { return };
    if !policy.resolve_icc(true) {
        info.source_color.icc_profile = None;
    }
    if !policy.resolve_exif(true) {
        info.embedded_metadata.exif = None;
    }
    if !policy.resolve_xmp(true) {
        info.embedded_metadata.xmp = None;
    }
}
