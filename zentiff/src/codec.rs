//! zencodec trait implementations for zentiff.
//!
//! Provides `TiffEncoderConfig` / `TiffDecoderCodecConfig` for integration
//! with the zencodec trait hierarchy.
//!
//! Feature-gated behind `zencodec`.

use alloc::borrow::Cow;
use alloc::format;
use alloc::vec::Vec;
use enough::Stop;
use zencodec::decode::{DecodeCapabilities, DecodeOutput, DecodePolicy, OutputInfo};
use zencodec::encode::{EncodeCapabilities, EncodeOutput, EncodePolicy};
use zencodec::{
    Cicp, ColorAuthority, ColorEmitPolicy, IccDisposition, ImageFormat, ImageInfo, ImageSequence,
    Metadata, Orientation, OrientationHint, Resolution, ResolutionUnit, ResourceLimits,
    SourceColor, resolve_color_emit,
};
use zenpixels::{PixelDescriptor, PixelSlice};

use whereat::At;
#[allow(unused_imports)]
use whereat::at;

use crate::error::TiffError;
use crate::{TiffDecodeConfig, TiffEncodeConfig, TiffInfo};

// ══════════════════════════════════════════════════════════════════════
// Source encoding details
// ══════════════════════════════════════════════════════════════════════

/// Source encoding details for TIFF (always lossless).
#[derive(Debug, Clone, Copy)]
pub struct TiffSourceEncoding;

impl zencodec::SourceEncodingDetails for TiffSourceEncoding {
    fn source_generic_quality(&self) -> Option<f32> {
        None
    }

    fn is_lossless(&self) -> bool {
        true
    }
}

// ══════════════════════════════════════════════════════════════════════
// Capabilities and descriptors
// ══════════════════════════════════════════════════════════════════════

static TIFF_ENCODE_CAPS: EncodeCapabilities = EncodeCapabilities::new()
    .with_lossless(true)
    .with_stop(true)
    .with_native_gray(true)
    .with_native_16bit(true)
    .with_native_f32(true)
    .with_native_alpha(true)
    // Metadata carriers TIFF can embed on encode (tags 34675 / 34665 / 700).
    // No CICP carrier: TIFF has no standardized CICP/nclx box, so `cicp` stays
    // false and a CICP-only source is lowered to a synthesized ICC instead.
    .with_icc(true)
    .with_exif(true)
    .with_xmp(true)
    .with_enforces_max_pixels(true);

static TIFF_DECODE_CAPS: DecodeCapabilities = DecodeCapabilities::new()
    .with_cheap_probe(true)
    .with_icc(true)
    .with_exif(true)
    .with_xmp(true)
    .with_stop(true)
    .with_native_gray(true)
    .with_native_16bit(true)
    .with_native_f32(true)
    .with_native_alpha(true)
    .with_hdr(true)
    // Multi-page TIFF: decode reports `ImageSequence::Multi` for >1 IFD.
    .with_multi_image(true)
    .with_enforces_max_pixels(true)
    .with_enforces_max_memory(true);

/// Pixel formats the TIFF encoder accepts.
///
/// Gray, GrayAlpha, RGB, RGBA in u8/u16/f32.
/// (GrayAlpha is written as Gray + an `ExtraSamples` alpha channel — 2
/// samples/pixel — not widened to RGBA.)
static TIFF_ENCODE_DESCRIPTORS: &[PixelDescriptor] = &[
    PixelDescriptor::RGB8_SRGB,
    PixelDescriptor::RGBA8_SRGB,
    PixelDescriptor::GRAY8_SRGB,
    PixelDescriptor::RGB16_SRGB,
    PixelDescriptor::RGBA16_SRGB,
    PixelDescriptor::GRAY16_SRGB,
    PixelDescriptor::RGBF32_LINEAR,
    PixelDescriptor::RGBAF32_LINEAR,
    PixelDescriptor::GRAYF32_LINEAR,
    PixelDescriptor::GRAYA8_SRGB,
    PixelDescriptor::GRAYA16_SRGB,
    PixelDescriptor::GRAYAF32_LINEAR,
];

/// Pixel formats the TIFF decoder can output.
///
/// Covers all standard decode outputs (see `descriptor_for()` in decode.rs).
static TIFF_DECODE_DESCRIPTORS: &[PixelDescriptor] = &[
    PixelDescriptor::RGB8_SRGB,
    PixelDescriptor::RGBA8_SRGB,
    PixelDescriptor::GRAY8_SRGB,
    PixelDescriptor::RGB16_SRGB,
    PixelDescriptor::RGBA16_SRGB,
    PixelDescriptor::GRAY16_SRGB,
    PixelDescriptor::RGBF32_LINEAR,
    PixelDescriptor::RGBAF32_LINEAR,
    PixelDescriptor::GRAYF32_LINEAR,
    PixelDescriptor::GRAYA8_SRGB,
    PixelDescriptor::GRAYA16_SRGB,
    PixelDescriptor::GRAYAF32_LINEAR,
];

// ══════════════════════════════════════════════════════════════════════
// Encode: TiffEncoderCodecConfig → TiffEncodeJob → TiffCodecEncoder
// ══════════════════════════════════════════════════════════════════════

// ── TiffEncoderCodecConfig ────────────────────────────────────────────

/// Encoding configuration for TIFF via zencodec traits.
///
/// Wraps [`TiffEncodeConfig`] and implements [`zencodec::encode::EncoderConfig`].
/// TIFF is always lossless; quality/effort knobs are no-ops.
#[derive(Clone, Debug)]
pub struct TiffEncoderCodecConfig {
    inner: TiffEncodeConfig,
}

impl Default for TiffEncoderCodecConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl TiffEncoderCodecConfig {
    /// Create a new TIFF encoder config with default settings (LZW + horizontal prediction).
    pub fn new() -> Self {
        Self {
            inner: TiffEncodeConfig::default(),
        }
    }

    /// Create from an existing [`TiffEncodeConfig`].
    pub fn from_config(config: TiffEncodeConfig) -> Self {
        Self { inner: config }
    }

    /// Access the inner [`TiffEncodeConfig`].
    pub fn inner(&self) -> &TiffEncodeConfig {
        &self.inner
    }

    /// Mutably access the inner [`TiffEncodeConfig`].
    pub fn inner_mut(&mut self) -> &mut TiffEncodeConfig {
        &mut self.inner
    }
}

impl zencodec::encode::EncoderConfig for TiffEncoderCodecConfig {
    type Error = At<TiffError>;
    type Job = TiffEncodeJob;

    fn format() -> ImageFormat {
        ImageFormat::Tiff
    }

    fn supported_descriptors() -> &'static [PixelDescriptor] {
        TIFF_ENCODE_DESCRIPTORS
    }

    fn capabilities() -> &'static EncodeCapabilities {
        &TIFF_ENCODE_CAPS
    }

    fn is_lossless(&self) -> Option<bool> {
        Some(true)
    }

    /// Uncalibrated structural estimate (no heaptrack model yet).
    ///
    /// TIFF encode is single-threaded: it buffers the full input image, emits an
    /// output buffer (~input bytes for uncompressed; smaller for deflate/lzw/
    /// packbits), plus a small (~1 MB) per-strip predictor/compress scratch. Peak
    /// is therefore roughly `input + output + scratch`. The throughput constant is
    /// a rough structural guess, not a measured model.
    fn estimate_encode_resources(
        &self,
        image: &zencodec::estimate::ImageCharacteristics,
        compute: &zencodec::estimate::ComputeEnvironment,
    ) -> zencodec::estimate::ResourceEstimate {
        use zencodec::estimate::{ResourceEstimate, ThreadingInformation};
        let input = image.input_bytes();
        let scratch = 1u64 << 20;
        // input + ~output + scratch
        let typ = input.saturating_add(input).saturating_add(scratch);
        // ~200 Mpix/s rough (uncalibrated, structural)
        let time_ms = (image.pixels() as f64 / 200_000.0) as f32;
        ResourceEstimate::new(typ, time_ms as u64)
            .with_peak_max(typ.saturating_mul(2))
            .with_threading(ThreadingInformation::SERIAL)
            .at_cores(compute.cores())
    }

    fn job(self) -> TiffEncodeJob {
        TiffEncodeJob {
            config: self,
            stop: None,
            limits: None,
            metadata: None,
            policy: EncodePolicy::none(),
        }
    }
}

// ── TiffEncodeJob ─────────────────────────────────────────────────────

/// Per-operation TIFF encode job.
pub struct TiffEncodeJob {
    config: TiffEncoderCodecConfig,
    stop: Option<zencodec::StopToken>,
    limits: Option<ResourceLimits>,
    metadata: Option<Metadata>,
    policy: EncodePolicy,
}

impl zencodec::encode::EncodeJob for TiffEncodeJob {
    type Error = At<TiffError>;
    type Enc = TiffCodecEncoder;
    type AnimationFrameEnc = ();

    fn with_stop(mut self, stop: zencodec::StopToken) -> Self {
        self.stop = Some(stop);
        self
    }

    fn with_policy(mut self, policy: EncodePolicy) -> Self {
        self.policy = policy;
        self
    }

    // `with_metadata` is deprecated in favor of `with_metadata_policy`, but the
    // codec must still implement it (the provided `with_metadata_policy` filters
    // then delegates here). Store the (already-filtered) metadata to embed.
    #[allow(deprecated)]
    fn with_metadata(mut self, meta: Metadata) -> Self {
        self.metadata = Some(meta);
        self
    }

    fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = Some(limits);
        self
    }

    fn encoder(self) -> Result<TiffCodecEncoder, At<TiffError>> {
        Ok(TiffCodecEncoder {
            config: self.config,
            stop: self.stop,
            limits: self.limits,
            metadata: self.metadata,
            policy: self.policy,
        })
    }

    fn animation_frame_encoder(self) -> Result<(), At<TiffError>> {
        Err(at!(TiffError::from(
            zencodec::UnsupportedOperation::AnimationEncode,
        )))
    }
}

// ── TiffCodecEncoder ──────────────────────────────────────────────────

/// Single-image TIFF encoder implementing [`zencodec::encode::Encoder`].
pub struct TiffCodecEncoder {
    config: TiffEncoderCodecConfig,
    stop: Option<zencodec::StopToken>,
    limits: Option<ResourceLimits>,
    metadata: Option<Metadata>,
    policy: EncodePolicy,
}

impl TiffCodecEncoder {
    fn check_limits(&self, pixels: &PixelSlice<'_>) -> Result<(), At<TiffError>> {
        if let Some(ref limits) = self.limits {
            let width = pixels.width();
            let height = pixels.rows();
            let pixel_count = width as u64 * height as u64;
            if let Some(max_px) = limits.max_pixels
                && pixel_count > max_px
            {
                return Err(at!(TiffError::LimitExceeded(format!(
                    "pixel count {pixel_count} exceeds limit {max_px}"
                ))));
            }
            if let Some(max_w) = limits.max_width
                && width > max_w
            {
                return Err(at!(TiffError::LimitExceeded(format!(
                    "width {width} exceeds limit {max_w}"
                ))));
            }
            if let Some(max_h) = limits.max_height
                && height > max_h
            {
                return Err(at!(TiffError::LimitExceeded(format!(
                    "height {height} exceeds limit {max_h}"
                ))));
            }
            if let Some(max_mem) = limits.max_memory_bytes {
                let bpp = pixels.descriptor().bytes_per_pixel() as u64;
                let estimated = pixel_count * bpp;
                if estimated > max_mem {
                    return Err(at!(TiffError::LimitExceeded(format!(
                        "estimated memory {estimated} bytes exceeds limit {max_mem}"
                    ))));
                }
            }
        }
        Ok(())
    }
}

impl zencodec::encode::Encoder for TiffCodecEncoder {
    type Error = At<TiffError>;

    fn reject(op: zencodec::UnsupportedOperation) -> At<TiffError> {
        at!(TiffError::from(op))
    }

    fn encode(self, pixels: PixelSlice<'_>) -> Result<EncodeOutput, At<TiffError>> {
        let stop: &dyn Stop = match &self.stop {
            Some(s) => s,
            None => &enough::Unstoppable,
        };

        self.check_limits(&pixels)?;

        let channel_count = pixels.descriptor().channels() as u8;
        let encode_meta = lower_metadata(self.metadata.as_ref(), &self.policy, channel_count);

        let encoded =
            crate::encode::encode_with_meta(&pixels, &self.config.inner, &encode_meta, stop)?;
        Ok(EncodeOutput::new(encoded, ImageFormat::Tiff))
    }
}

/// Lower the requested [`Metadata`] (already filtered by any
/// [`with_metadata_policy`](zencodec::encode::EncodeJob::with_metadata_policy))
/// plus the color-emit policy into the format-agnostic
/// [`crate::encode::TiffEncodeMeta`] the encoder writes.
///
/// Color: TIFF has no CICP carrier ([`EncodeCapabilities`]`cicp == false`), so
/// [`resolve_color_emit`] is asked what to do with the ICC channel — a
/// CICP-only source resolves to [`IccDisposition::SynthesizeFrom`], which we
/// lower to a profile via the transfer-aware
/// [`zenpixels_convert::icc_profiles::synthesize_icc_for_cicp`] (so an HDR transfer
/// is never mis-tagged with an SDR-TRC profile); `plan.cicp` itself is discarded
/// because there is nowhere to write it.
fn lower_metadata(
    meta: Option<&Metadata>,
    policy: &EncodePolicy,
    channel_count: u8,
) -> crate::encode::TiffEncodeMeta {
    let Some(meta) = meta else {
        return crate::encode::TiffEncodeMeta::default();
    };

    // --- Color (ICC) via resolve_color_emit ------------------------------
    let mut src = SourceColor::default().with_channel_count(channel_count);
    if let Some(c) = meta.cicp {
        src = src.with_cicp(c).with_color_authority(ColorAuthority::Cicp);
    }
    if let Some(icc) = &meta.icc_profile {
        src = src
            .with_icc_profile(icc.clone())
            .with_color_authority(ColorAuthority::Icc);
    }

    let color_policy = policy.resolve_color(ColorEmitPolicy::Balanced);
    let plan = resolve_color_emit(&src, &TIFF_ENCODE_CAPS, color_policy);

    // Coarse `embed_icc` gate (best-effort) overrides the plan toward dropping.
    let icc: Option<Vec<u8>> = if policy.resolve_icc(true) {
        match plan.icc {
            IccDisposition::KeepSource => meta.icc_profile.as_deref().map(|b| b.to_vec()),
            IccDisposition::SynthesizeFrom(cicp) => synth_icc_from_cicp(cicp),
            IccDisposition::Drop => None,
            // `IccDisposition` is `#[non_exhaustive]`; a future disposition we
            // don't understand must not emit unknown ICC bytes — drop instead.
            _ => None,
        }
    } else {
        None
    };

    // --- EXIF / XMP (already retention-filtered by the policy layer) ------
    let exif: Option<Vec<u8>> = if policy.resolve_exif(true) {
        meta.exif.as_deref().map(|b| b.to_vec())
    } else {
        None
    };
    let xmp: Option<Vec<u8>> = if policy.resolve_xmp(true) {
        meta.xmp.as_deref().map(|b| b.to_vec())
    } else {
        None
    };

    // --- Orientation → IFD0 tag 274 --------------------------------------
    let orientation = match meta.orientation {
        Orientation::Identity => None,
        o => Some(o.to_exif() as u16),
    };

    crate::encode::TiffEncodeMeta {
        icc,
        xmp,
        exif,
        orientation,
    }
}

/// Materialize an ICC profile for a full CICP (primaries **and** transfer) via
/// the transfer-aware [`synthesize_icc_for_cicp`]. Returns `None` for the
/// sRGB/BT.709 default and whenever no faithful profile is available
/// (`NeedsCms`/`CmsUnsupported`) — TIFF has no CICP carrier, so the caller simply
/// embeds no ICC rather than a mis-tagged one (a BT.2020-PQ source must not get
/// the SDR-TRC Rec.2020 profile that the primaries-only lookup would have
/// returned).
///
/// [`synthesize_icc_for_cicp`]: zenpixels_convert::icc_profiles::synthesize_icc_for_cicp
fn synth_icc_from_cicp(cicp: Cicp) -> Option<Vec<u8>> {
    use zenpixels_convert::icc_profiles::SynthesizedIcc;
    match zenpixels_convert::icc_profiles::synthesize_icc_for_cicp(cicp) {
        SynthesizedIcc::Profile(bytes) => Some(bytes.into_owned()),
        _ => None,
    }
}

// ══════════════════════════════════════════════════════════════════════
// Decode: TiffDecoderCodecConfig → TiffDecodeJob → TiffCodecDecoder
// ══════════════════════════════════════════════════════════════════════

// ── TiffDecoderCodecConfig ────────────────────────────────────────────

/// Decoding configuration for TIFF via zencodec traits.
///
/// Wraps [`TiffDecodeConfig`] and implements [`zencodec::decode::DecoderConfig`].
#[derive(Clone, Debug)]
pub struct TiffDecoderCodecConfig {
    inner: TiffDecodeConfig,
}

impl Default for TiffDecoderCodecConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl TiffDecoderCodecConfig {
    /// Create a new TIFF decoder config with default resource limits.
    pub fn new() -> Self {
        Self {
            inner: TiffDecodeConfig::default(),
        }
    }

    /// Create from an existing [`TiffDecodeConfig`].
    pub fn from_config(config: TiffDecodeConfig) -> Self {
        Self { inner: config }
    }

    /// Access the inner [`TiffDecodeConfig`].
    pub fn inner(&self) -> &TiffDecodeConfig {
        &self.inner
    }

    /// Mutably access the inner [`TiffDecodeConfig`].
    pub fn inner_mut(&mut self) -> &mut TiffDecodeConfig {
        &mut self.inner
    }
}

impl zencodec::decode::DecoderConfig for TiffDecoderCodecConfig {
    type Error = At<TiffError>;
    type Job<'a> = TiffDecodeJob;

    fn formats() -> &'static [ImageFormat] {
        &[ImageFormat::Tiff]
    }

    fn supported_descriptors() -> &'static [PixelDescriptor] {
        TIFF_DECODE_DESCRIPTORS
    }

    fn capabilities() -> &'static DecodeCapabilities {
        &TIFF_DECODE_CAPS
    }

    fn job<'a>(self) -> Self::Job<'a> {
        TiffDecodeJob {
            config: self,
            stop: None,
            limits: None,
            max_input_bytes: None,
            policy: None,
            orientation: OrientationHint::Preserve,
        }
    }
}

// ── TiffDecodeJob ─────────────────────────────────────────────────────

/// Per-operation TIFF decode job.
pub struct TiffDecodeJob {
    config: TiffDecoderCodecConfig,
    stop: Option<zencodec::StopToken>,
    limits: Option<ResourceLimits>,
    max_input_bytes: Option<u64>,
    policy: Option<DecodePolicy>,
    /// How to resolve the stored EXIF [`Orientation`] tag (tag 274) during
    /// decode. Default [`OrientationHint::Preserve`] — pixels stay in stored
    /// orientation and the intrinsic tag is reported. See
    /// [`DecodeJob::with_orientation`](zencodec::decode::DecodeJob::with_orientation).
    orientation: OrientationHint,
}

impl TiffDecodeJob {
    /// Build a `TiffDecodeConfig` that merges zencodec `ResourceLimits` with
    /// the base config's limits, preferring the per-job limits.
    fn effective_decode_config(&self) -> TiffDecodeConfig {
        let base = &self.config.inner;
        if let Some(ref limits) = self.limits {
            TiffDecodeConfig {
                max_pixels: limits.max_pixels.or(base.max_pixels),
                max_memory_bytes: limits.max_memory_bytes.or(base.max_memory_bytes),
                max_width: limits.max_width.or(base.max_width),
                max_height: limits.max_height.or(base.max_height),
            }
        } else {
            base.clone()
        }
    }

    /// Apply decode policy to suppress metadata fields from probe results.
    fn apply_policy_to_info(&self, info: &mut ImageInfo) {
        if let Some(ref policy) = self.policy {
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
    }
}

impl<'a> zencodec::decode::DecodeJob<'a> for TiffDecodeJob {
    type Error = At<TiffError>;
    type Dec = TiffCodecDecoder<'a>;
    type StreamDec = zencodec::Unsupported<At<TiffError>>;
    type AnimationFrameDec = zencodec::Unsupported<At<TiffError>>;

    fn with_stop(mut self, stop: zencodec::StopToken) -> Self {
        self.stop = Some(stop);
        self
    }

    fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.max_input_bytes = limits.max_input_bytes;
        self.limits = Some(limits);
        self
    }

    fn with_policy(mut self, policy: DecodePolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    fn with_orientation(mut self, hint: OrientationHint) -> Self {
        self.orientation = hint;
        self
    }

    fn probe(&self, data: &[u8]) -> Result<ImageInfo, At<TiffError>> {
        let tiff_info = crate::probe(data)?;
        let mut info = tiff_info_to_image_info(&tiff_info);
        // Report consistently with what `decode()` produces under this hint.
        // `Preserve` (default) keeps the stored dims + intrinsic EXIF tag that
        // `tiff_info_to_image_info` already set; the bake hints report the
        // display (post-orientation) dims + `Identity`.
        info = report_probe_for_hint(info, self.orientation);
        self.apply_policy_to_info(&mut info);
        Ok(info)
    }

    fn output_info(&self, data: &[u8]) -> Result<OutputInfo, At<TiffError>> {
        let tiff_info = crate::probe(data)?;
        let has_alpha = has_alpha_from_color_type(tiff_info.color_type);
        let native_format = descriptor_for_probe(&tiff_info);
        // Report the post-orientation output geometry + the transform the
        // decoder will apply. `Preserve` applies nothing (output = stored dims,
        // `Identity` recorded); a bake hint outputs the resolved orientation's
        // dims and records it.
        let intrinsic = tiff_info
            .orientation
            .and_then(|v| Orientation::from_exif(v as u8))
            .unwrap_or(Orientation::Identity);
        let resolved = if self.orientation.bakes() {
            resolve_orientation(self.orientation, intrinsic)
        } else {
            Orientation::Identity
        };
        let (ow, oh) = resolved.output_dimensions(tiff_info.width, tiff_info.height);
        Ok(OutputInfo::full_decode(ow, oh, native_format)
            .with_alpha(has_alpha)
            .with_orientation_applied(resolved))
    }

    fn decoder(
        self,
        data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> Result<TiffCodecDecoder<'a>, At<TiffError>> {
        if let Some(max) = self.max_input_bytes
            && data.len() as u64 > max
        {
            return Err(at!(TiffError::LimitExceeded(format!(
                "input size {} exceeds limit {max}",
                data.len()
            ))));
        }
        let decode_config = self.effective_decode_config();
        Ok(TiffCodecDecoder {
            config: self.config,
            decode_config,
            data,
            stop: self.stop,
            policy: self.policy,
            orientation: self.orientation,
        })
    }

    fn push_decoder(
        self,
        data: Cow<'a, [u8]>,
        sink: &mut dyn zencodec::decode::DecodeRowSink,
        preferred: &[PixelDescriptor],
    ) -> Result<OutputInfo, Self::Error> {
        zencodec::helpers::copy_decode_to_sink(self, data, sink, preferred, |e| {
            at!(TiffError::InvalidInput(e.to_string()))
        })
    }

    fn streaming_decoder(
        self,
        _data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> Result<zencodec::Unsupported<At<TiffError>>, At<TiffError>> {
        Err(at!(TiffError::from(
            zencodec::UnsupportedOperation::RowLevelDecode,
        )))
    }

    fn animation_frame_decoder(
        self,
        _data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> Result<zencodec::Unsupported<At<TiffError>>, At<TiffError>> {
        Err(at!(TiffError::from(
            zencodec::UnsupportedOperation::AnimationDecode,
        )))
    }
}

// ── TiffCodecDecoder ──────────────────────────────────────────────────

/// Single-image TIFF decoder implementing [`zencodec::decode::Decode`].
pub struct TiffCodecDecoder<'a> {
    config: TiffDecoderCodecConfig,
    decode_config: TiffDecodeConfig,
    data: Cow<'a, [u8]>,
    stop: Option<zencodec::StopToken>,
    policy: Option<DecodePolicy>,
    /// Resolved from [`TiffDecodeJob::orientation`]; drives the bake path in
    /// [`Decode::decode`](zencodec::decode::Decode::decode).
    orientation: OrientationHint,
}

impl TiffCodecDecoder<'_> {
    /// Apply decode policy to suppress metadata fields from probe results.
    fn apply_policy_to_info(&self, info: &mut ImageInfo) {
        if let Some(ref policy) = self.policy {
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
    }
}

impl zencodec::decode::Decode for TiffCodecDecoder<'_> {
    type Error = At<TiffError>;

    fn decode(self) -> Result<DecodeOutput, At<TiffError>> {
        let stop: &dyn Stop = match &self.stop {
            Some(s) => s,
            None => &enough::Unstoppable,
        };
        let _ = self.config; // available for future config-level overrides

        let output = crate::decode(&self.data, &self.decode_config, stop)?;

        let mut info = tiff_info_to_image_info(&output.info);
        self.apply_policy_to_info(&mut info);

        let decoded =
            DecodeOutput::new(output.pixels, info).with_source_encoding_details(TiffSourceEncoding);
        // `Preserve` (default) returns the decoded output unchanged: stored
        // pixels + stored dims + intrinsic EXIF tag. A bake hint physically
        // rotates the buffer and rewrites the reported dims/tag to match.
        Ok(apply_orientation_to_output(decoded, self.orientation))
    }
}

// ══════════════════════════════════════════════════════════════════════
// Helpers
// ══════════════════════════════════════════════════════════════════════

/// Convert a [`TiffInfo`] into a [`zencodec::ImageInfo`].
fn tiff_info_to_image_info(tiff: &TiffInfo) -> ImageInfo {
    let has_alpha = has_alpha_from_color_type(tiff.color_type);

    let orientation = tiff
        .orientation
        .and_then(|v| Orientation::from_exif(v as u8))
        .unwrap_or_default();

    // `with_bit_depth` / `with_channel_count` populate `source_color.bit_depth`
    // and `source_color.channel_count` (they are convenience setters for those
    // SourceColor fields — `ImageInfo` has no separate top-level copies), so the
    // color-emit resolver and any consumer see the source's depth/channels.
    let mut info = ImageInfo::new(tiff.width, tiff.height, ImageFormat::Tiff)
        .with_alpha(has_alpha)
        .with_orientation(orientation)
        .with_bit_depth(tiff.bit_depth)
        .with_channel_count(tiff.channels as u8)
        .with_source_encoding_details(TiffSourceEncoding);

    // Multi-page TIFF
    if let Some(page_count) = tiff.page_count
        && page_count > 1
    {
        info = info.with_sequence(ImageSequence::Multi {
            image_count: Some(page_count),
            random_access: true,
        });
    }

    // Resolution
    if let Some((x_dpi, y_dpi)) = tiff.dpi {
        let unit = match tiff.resolution_unit {
            Some(3) => ResolutionUnit::Centimeter,
            _ => ResolutionUnit::Inch,
        };
        // Store original resolution values (not converted DPI)
        let (x_res, y_res) = if unit == ResolutionUnit::Centimeter {
            // Convert DPI back to dots per centimeter for the Resolution struct
            (x_dpi / 2.54, y_dpi / 2.54)
        } else {
            (x_dpi, y_dpi)
        };
        info = info.with_resolution(Resolution {
            x: x_res,
            y: y_res,
            unit,
        });
    }

    // ICC profile
    if let Some(ref icc) = tiff.icc_profile {
        info = info.with_icc_profile(icc.clone());
    }

    // EXIF
    if let Some(ref exif) = tiff.exif {
        info = info.with_exif(exif.clone());
    }

    // XMP
    if let Some(ref xmp) = tiff.xmp {
        info = info.with_xmp(xmp.clone());
    }

    info
}

// ══════════════════════════════════════════════════════════════════════
// Orientation (EXIF tag 274) — adapter-only baking
// ══════════════════════════════════════════════════════════════════════
//
// TIFF carries orientation natively as the `Orientation` IFD tag (274); the
// raster the `tiff` crate decodes is always the *stored* orientation. The
// zencodec adapter honors `OrientationHint`:
//   - `Preserve` (default): leave the pixels stored; report the stored (coded)
//     dims + the intrinsic EXIF tag (`tiff_info_to_image_info` already does
//     this). The caller applies the orientation (e.g. via `display_width`).
//   - `Correct` / `CorrectAndTransform` / `ExactTransform`: physically bake the
//     resolved orientation into the buffer via `zenpixels_convert::orient`, then
//     report the display dims + `Orientation::Identity` (no orientation remains).
//
// image-tiff has no orientation bake of its own (see the Orientation tag note in
// decode.rs); the rotation is the adapter's responsibility, mirroring zenwebp's
// and zenavif's EXIF-orientation handling.

/// Resolve the net [`Orientation`] to bake into the *stored* pixels for `hint`,
/// given the image's intrinsic EXIF `intrinsic` orientation.
///
/// - [`Preserve`](OrientationHint::Preserve): nothing to bake — returns
///   [`Identity`](Orientation::Identity) (callers gate on [`OrientationHint::bakes`] first,
///   so this arm is a defensive default).
/// - [`Correct`](OrientationHint::Correct): the intrinsic orientation (applying
///   it to the stored pixels yields the upright image).
/// - [`ExactTransform`](OrientationHint::ExactTransform): the literal transform,
///   ignoring EXIF.
/// - [`CorrectAndTransform`](OrientationHint::CorrectAndTransform): the intrinsic
///   correction first, then the requested transform.
fn resolve_orientation(hint: OrientationHint, intrinsic: Orientation) -> Orientation {
    match hint {
        OrientationHint::Preserve => Orientation::Identity,
        OrientationHint::Correct => intrinsic,
        OrientationHint::ExactTransform(t) => t,
        OrientationHint::CorrectAndTransform(t) => intrinsic.then(t),
        // `OrientationHint` is `#[non_exhaustive]`; treat any future variant as a
        // no-op bake rather than guessing — the reported tag stays consistent.
        _ => Orientation::Identity,
    }
}

/// Bake `hint` into a decoded [`DecodeOutput`].
///
/// On the [`Preserve`](OrientationHint::Preserve) path ([`OrientationHint::bakes`] is
/// `false`) the output is returned unchanged: pixels stay in stored orientation
/// and `ImageInfo` keeps the stored dims + intrinsic EXIF tag that
/// [`tiff_info_to_image_info`] already set.
///
/// Otherwise the resolved orientation (see [`resolve_orientation`]) is physically
/// applied to the pixels via [`zenpixels_convert::orient::apply_orientation`],
/// and the reported `ImageInfo` is rewritten to the baked buffer's dimensions
/// with [`Orientation::Identity`] (the pixels are final — no orientation remains
/// to apply). The intrinsic EXIF orientation is read from the `ImageInfo` the
/// decode path already computed, so it matches what `probe()` reports.
fn apply_orientation_to_output(output: DecodeOutput, hint: OrientationHint) -> DecodeOutput {
    if !hint.bakes() {
        return output;
    }
    let mut info = output.info().clone();
    let intrinsic = info.orientation;
    let resolved = resolve_orientation(hint, intrinsic);

    let buf = output.into_buffer();

    // Even when the resolved transform is Identity (e.g. `Correct` on an
    // upright image) we still rewrite the reported orientation to Identity so a
    // consumer never double-applies the (now-stale) intrinsic tag. The pixel
    // copy is skipped in that case.
    let baked = if resolved.is_identity() {
        buf
    } else {
        zenpixels_convert::orient::apply_orientation(buf.as_slice(), resolved)
    };

    // `ImageInfo` has no dimension setter; the fields are public. Report the
    // baked buffer's geometry + Identity (pixels are now final).
    info.width = baked.width();
    info.height = baked.height();
    info = info.with_orientation(Orientation::Identity);

    DecodeOutput::new(baked, info).with_source_encoding_details(TiffSourceEncoding)
}

/// Rewrite a probe [`ImageInfo`] (stored dims + intrinsic EXIF tag, as produced
/// by [`tiff_info_to_image_info`]) to match what [`apply_orientation_to_output`]
/// reports for `hint`.
///
/// On the [`Preserve`](OrientationHint::Preserve) path the info is returned
/// unchanged. On a bake hint the dims are set to the resolved orientation's
/// output geometry and the tag becomes [`Orientation::Identity`].
fn report_probe_for_hint(mut info: ImageInfo, hint: OrientationHint) -> ImageInfo {
    if !hint.bakes() {
        return info;
    }
    let resolved = resolve_orientation(hint, info.orientation);
    let (ow, oh) = resolved.output_dimensions(info.width, info.height);
    info.width = ow;
    info.height = oh;
    info.with_orientation(Orientation::Identity)
}

/// Determine whether a tiff `ColorType` has an alpha channel.
fn has_alpha_from_color_type(ct: tiff::ColorType) -> bool {
    matches!(
        ct,
        tiff::ColorType::GrayA(_)
            | tiff::ColorType::RGBA(_)
            | tiff::ColorType::CMYKA(_)
            | tiff::ColorType::Multiband {
                num_samples: 2 | 4,
                ..
            }
    )
}

/// Best-fit pixel descriptor for a TIFF probe result.
///
/// Uses the same logic as `descriptor_for()` in decode.rs but works
/// from probe info (where `is_float` may not be known).
fn descriptor_for_probe(tiff: &TiffInfo) -> PixelDescriptor {
    let is_float = tiff.is_float;
    match tiff.color_type {
        tiff::ColorType::Gray(d) => match d {
            1..=8 => PixelDescriptor::GRAY8_SRGB,
            9..=16 if is_float => PixelDescriptor::GRAYF32_LINEAR,
            9..=16 => PixelDescriptor::GRAY16_SRGB,
            _ if is_float => PixelDescriptor::GRAYF32_LINEAR,
            _ => PixelDescriptor::GRAY16_SRGB,
        },
        tiff::ColorType::GrayA(d) => match d {
            1..=8 => PixelDescriptor::GRAYA8_SRGB,
            9..=16 if is_float => PixelDescriptor::GRAYAF32_LINEAR,
            9..=16 => PixelDescriptor::GRAYA16_SRGB,
            _ if is_float => PixelDescriptor::GRAYAF32_LINEAR,
            _ => PixelDescriptor::GRAYA16_SRGB,
        },
        tiff::ColorType::RGB(d) | tiff::ColorType::YCbCr(d) | tiff::ColorType::Lab(d) => match d {
            1..=8 => PixelDescriptor::RGB8_SRGB,
            9..=16 if is_float => PixelDescriptor::RGBF32_LINEAR,
            9..=16 => PixelDescriptor::RGB16_SRGB,
            _ if is_float => PixelDescriptor::RGBF32_LINEAR,
            _ => PixelDescriptor::RGB16_SRGB,
        },
        tiff::ColorType::RGBA(d) => match d {
            1..=8 => PixelDescriptor::RGBA8_SRGB,
            9..=16 if is_float => PixelDescriptor::RGBAF32_LINEAR,
            9..=16 => PixelDescriptor::RGBA16_SRGB,
            _ if is_float => PixelDescriptor::RGBAF32_LINEAR,
            _ => PixelDescriptor::RGBA16_SRGB,
        },
        tiff::ColorType::Palette(_) => PixelDescriptor::RGB8_SRGB,
        tiff::ColorType::CMYK(d) | tiff::ColorType::CMYKA(d) => match d {
            1..=8 => PixelDescriptor::RGBA8_SRGB,
            9..=16 if is_float => PixelDescriptor::RGBAF32_LINEAR,
            9..=16 => PixelDescriptor::RGBA16_SRGB,
            _ if is_float => PixelDescriptor::RGBAF32_LINEAR,
            _ => PixelDescriptor::RGBA16_SRGB,
        },
        tiff::ColorType::Multiband {
            bit_depth,
            num_samples,
        } => match (num_samples, bit_depth) {
            (1, 1..=8) => PixelDescriptor::GRAY8_SRGB,
            (1, _) if is_float => PixelDescriptor::GRAYF32_LINEAR,
            (1, _) => PixelDescriptor::GRAY16_SRGB,
            // 2 channels → GrayAlpha; mirror `descriptor_for` in decode.rs so a
            // Gray + `ExtraSamples` (float) image probes as GRAYAF32, matching
            // what `decode()` actually produces.
            (2, 1..=8) => PixelDescriptor::GRAYA8_SRGB,
            (2, _) if is_float => PixelDescriptor::GRAYAF32_LINEAR,
            (2, _) => PixelDescriptor::GRAYA16_SRGB,
            (3, 1..=8) => PixelDescriptor::RGB8_SRGB,
            (3, _) if is_float => PixelDescriptor::RGBF32_LINEAR,
            (3, _) => PixelDescriptor::RGB16_SRGB,
            (4, 1..=8) => PixelDescriptor::RGBA8_SRGB,
            (4, _) if is_float => PixelDescriptor::RGBAF32_LINEAR,
            (4, _) => PixelDescriptor::RGBA16_SRGB,
            (_, 1..=8) => PixelDescriptor::RGBA8_SRGB,
            _ if is_float => PixelDescriptor::RGBAF32_LINEAR,
            _ => PixelDescriptor::RGBA16_SRGB,
        },
        _ => PixelDescriptor::RGBA8_SRGB,
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use zencodec::decode::{Decode, DecodeJob, DecoderConfig};
    use zencodec::encode::{EncodeJob, Encoder, EncoderConfig};
    use zenpixels::PixelBuffer;

    /// Helper: encode via the zencodec trait flow.
    fn encode_pixels(slice: PixelSlice<'_>) -> EncodeOutput {
        let config = TiffEncoderCodecConfig::new();
        config.job().encoder().unwrap().encode(slice).unwrap()
    }

    /// Helper: decode via the zencodec trait flow.
    fn decode_bytes(data: &[u8]) -> DecodeOutput {
        let config = TiffDecoderCodecConfig::new();
        let job = config.job();
        let decoder = job.decoder(Cow::Borrowed(data), &[]).unwrap();
        decoder.decode().unwrap()
    }

    #[test]
    fn roundtrip_rgb8() {
        let w = 4u32;
        let h = 2u32;
        let pixels: Vec<u8> = (0..w * h * 3).map(|i| (i % 256) as u8).collect();
        let buf = PixelBuffer::from_vec(pixels.clone(), w, h, PixelDescriptor::RGB8_SRGB).unwrap();
        let slice = buf.as_slice();

        let encoded = encode_pixels(slice);
        assert_eq!(encoded.format(), ImageFormat::Tiff);
        assert!(!encoded.is_empty());

        let decoded = decode_bytes(encoded.data());
        assert_eq!(decoded.width(), w);
        assert_eq!(decoded.height(), h);
        assert_eq!(decoded.info().format, ImageFormat::Tiff);

        // Verify pixel data roundtrips
        let out_pixels = decoded.pixels();
        assert_eq!(out_pixels.contiguous_bytes().as_ref(), &pixels[..]);
    }

    #[test]
    fn roundtrip_gray8() {
        let w = 3u32;
        let h = 3u32;
        let pixels: Vec<u8> = (0..w * h).map(|i| (i * 28 % 256) as u8).collect();
        let buf = PixelBuffer::from_vec(pixels.clone(), w, h, PixelDescriptor::GRAY8_SRGB).unwrap();
        let slice = buf.as_slice();

        let encoded = encode_pixels(slice);
        let decoded = decode_bytes(encoded.data());
        assert_eq!(decoded.width(), w);
        assert_eq!(decoded.height(), h);
        assert_eq!(decoded.pixels().contiguous_bytes().as_ref(), &pixels[..]);
    }

    #[test]
    fn probe_via_trait() {
        // Encode, then probe the result
        let w = 2u32;
        let h = 2u32;
        let pixels = vec![255u8; (w * h * 4) as usize];
        let buf = PixelBuffer::from_vec(pixels.clone(), w, h, PixelDescriptor::RGBA8_SRGB).unwrap();
        let encoded = encode_pixels(buf.as_slice());

        let config = TiffDecoderCodecConfig::new();
        let job = config.job();
        let info = job.probe(encoded.data()).unwrap();
        assert_eq!(info.width, w);
        assert_eq!(info.height, h);
        assert_eq!(info.format, ImageFormat::Tiff);
        assert!(info.has_alpha);
    }

    #[test]
    fn output_info_via_trait() {
        let w = 2u32;
        let h = 2u32;
        let pixels = vec![128u8; (w * h * 3) as usize];
        let buf = PixelBuffer::from_vec(pixels, w, h, PixelDescriptor::RGB8_SRGB).unwrap();
        let encoded = encode_pixels(buf.as_slice());

        let config = TiffDecoderCodecConfig::new();
        let job = config.job();
        let output_info = job.output_info(encoded.data()).unwrap();
        assert_eq!(output_info.width, w);
        assert_eq!(output_info.height, h);
        assert!(!output_info.has_alpha);
    }

    #[test]
    fn animation_encode_rejected() {
        let config = TiffEncoderCodecConfig::new();
        let result = config.job().animation_frame_encoder();
        assert!(result.is_err());
    }

    #[test]
    fn streaming_decode_rejected() {
        let config = TiffDecoderCodecConfig::new();
        let job = config.job();
        let result = job.streaming_decoder(Cow::Borrowed(&[]), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn animation_decode_rejected() {
        let config = TiffDecoderCodecConfig::new();
        let job = config.job();
        let result = job.animation_frame_decoder(Cow::Borrowed(&[]), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn encoder_config_traits() {
        assert_eq!(TiffEncoderCodecConfig::format(), ImageFormat::Tiff);
        assert!(TiffEncoderCodecConfig::capabilities().lossless());
        assert!(!TiffEncoderCodecConfig::supported_descriptors().is_empty());
    }

    #[test]
    fn decoder_config_traits() {
        assert_eq!(TiffDecoderCodecConfig::formats(), &[ImageFormat::Tiff]);
        assert!(TiffDecoderCodecConfig::capabilities().cheap_probe());
        assert!(!TiffDecoderCodecConfig::supported_descriptors().is_empty());
    }

    #[test]
    fn lossless_always_true() {
        let config = TiffEncoderCodecConfig::new();
        assert_eq!(config.is_lossless(), Some(true));
    }

    #[test]
    fn source_encoding_is_lossless() {
        let w = 2u32;
        let h = 2u32;
        let pixels = vec![0u8; (w * h * 3) as usize];
        let buf = PixelBuffer::from_vec(pixels, w, h, PixelDescriptor::RGB8_SRGB).unwrap();
        let encoded = encode_pixels(buf.as_slice());
        let decoded = decode_bytes(encoded.data());
        let details = decoded.source_encoding_details().unwrap();
        assert!(details.is_lossless());
        assert_eq!(details.source_generic_quality(), None);
    }

    #[test]
    fn decode_policy_suppresses_metadata() {
        // Encode a simple image
        let w = 2u32;
        let h = 2u32;
        let pixels = vec![0u8; (w * h * 3) as usize];
        let buf = PixelBuffer::from_vec(pixels, w, h, PixelDescriptor::RGB8_SRGB).unwrap();
        let encoded = encode_pixels(buf.as_slice());

        // Decode with strict policy — metadata should be suppressed
        let config = TiffDecoderCodecConfig::new();
        let job = config.job().with_policy(DecodePolicy::strict());
        let info = job.probe(encoded.data()).unwrap();
        // ICC, EXIF, XMP should all be None with strict policy
        assert!(info.source_color.icc_profile.is_none());
        assert!(info.embedded_metadata.exif.is_none());
        assert!(info.embedded_metadata.xmp.is_none());
    }
}
