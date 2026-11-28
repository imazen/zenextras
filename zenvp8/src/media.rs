//! `zencodec-media` sessions and factories for VP8 (`Vp8VideoDecoder`,
//! `Vp8DecodeFactory`, and behind `encoder`, `Vp8VideoEncoder` and
//! `Vp8EncodeFactory`).

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec;
use core::fmt;

#[cfg(feature = "encoder")]
use zencodec::ThreadingPolicy;
use zencodec::enough::Stop as _;
use zencodec::plane::{AnyPlane, PlanarLayout, PlaneEncoding, PlaneRef, PlaneSpec};
use zencodec::source::NativeBound;
use zencodec::storage::{OwnedStorage, StorageBudget};
#[cfg(feature = "encoder")]
use zencodec::time::TimeBase;
use zencodec::{JobResources, Pulse, SharedPulse};
use zencodec_media::frame::{
    ColorClaim, Frame, FrameGeometry, FrameInterpretation, FrameTiming, MapError, MappedPlanes,
    Picture, PlaneRole, Presentation,
};
use zencodec_media::packet::Packet;
#[cfg(feature = "encoder")]
use zencodec_media::packet::RandomAccess;
use zencodec_media::registry::{
    LocalVideoDecoder, MediaError, VideoDecodeFactory, VideoDecodeNeeds, VideoDecodePlan,
};
use zencodec_media::session::{
    CodecConfig, DynVideoDecoder, ProtocolViolation, Receive, SendStatus, SessionError,
    UnsupportedMedia, VideoDecoder, VideoDecoderShim, VideoStreamInfo, erase_video_decoder,
};
use zencodec_media::track::CodecId;
use zenpixels::sample::{ChromaSiting, ChromaSubsampling, SampleEncoding};
use zenpixels::{Cicp, SampleType};

use crate::{DecodeError, Vp8Decoder};

/// Errors produced by VP8 `zencodec-media` sessions.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Vp8SessionError {
    /// VP8 bitstream decode failed.
    Decode(DecodeError),
    /// Out of storage budget or fallible allocation failed.
    Allocation,
    /// Encoder settings invalid or unsupported.
    #[cfg(feature = "encoder")]
    Encode(crate::EncodeError),
    /// Input frame geometry, subsampling, depth, or range does not match encoder config.
    InputMismatch,
    /// Presentation timestamp was not strictly increasing.
    NonMonotonicTimestamp,
}

impl fmt::Display for Vp8SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(e) => write!(f, "VP8 decode: {e}"),
            Self::Allocation => f.write_str("VP8 plane storage budget exceeded"),
            #[cfg(feature = "encoder")]
            Self::Encode(e) => write!(f, "VP8 encode: {e}"),
            Self::InputMismatch => f.write_str("frame does not match VP8 encoder configuration"),
            Self::NonMonotonicTimestamp => {
                f.write_str("VP8 frame timestamp is not strictly increasing")
            }
        }
    }
}

impl core::error::Error for Vp8SessionError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Decode(e) => Some(e),
            #[cfg(feature = "encoder")]
            Self::Encode(e) => Some(e),
            _ => None,
        }
    }
}

type Res<T> = Result<T, SessionError<Vp8SessionError>>;

/// Accounted planar 8-bit 4:2:0 YCbCr picture backed by [`OwnedStorage`].
struct AccountedYuv420 {
    width: u32,
    height: u32,
    y: OwnedStorage<u8>,
    u: OwnedStorage<u8>,
    v: OwnedStorage<u8>,
}

struct MappedYuv420<'a> {
    p: &'a AccountedYuv420,
}

impl MappedPlanes for MappedYuv420<'_> {
    fn plane_count(&self) -> usize {
        3
    }

    fn plane(&self, index: usize) -> Option<AnyPlane<'_>> {
        let cw = self.p.width.div_ceil(2);
        let ch = self.p.height.div_ceil(2);
        let (slice, w, h) = match index {
            0 => (
                self.p.y.as_slice(),
                self.p.width as usize,
                self.p.height as usize,
            ),
            1 => (self.p.u.as_slice(), cw as usize, ch as usize),
            2 => (self.p.v.as_slice(), cw as usize, ch as usize),
            _ => return None,
        };
        let plane = PlaneRef::new(slice, w, h, w).ok()?;
        Some(AnyPlane::U8(plane))
    }
}

impl Picture for AccountedYuv420 {
    fn map(&self) -> Result<Box<dyn MappedPlanes + '_>, MapError> {
        Ok(Box::new(MappedYuv420 { p: self }))
    }
}

/// Parse VP8 3-byte uncompressed data chunk tag: `(is_keyframe, show_frame)`.
fn vp8_frame_tag(bytes: &[u8]) -> Option<(bool, bool)> {
    if bytes.len() < 3 {
        return None;
    }
    let tag = u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[2]) << 16);
    let is_keyframe = (tag & 1) == 0;
    let show_frame = ((tag >> 4) & 1) != 0;
    Some((is_keyframe, show_frame))
}

/// Pure-Rust VP8 video decoder session implementing [`VideoDecoder`].
pub struct Vp8VideoDecoder {
    dec: Vp8Decoder,
    storage: StorageBudget,
    owned_pulse: Option<SharedPulse>,
    config: Option<Arc<CodecConfig>>,
    ended: bool,
    ready: VecDeque<Frame>,
    next_ordinal: u64,
    info: Option<VideoStreamInfo>,
    max_coded_extent: Option<(u32, u32)>,
    max_pixels: Option<u64>,
    max_input_bytes: Option<u64>,
    total_input_bytes: u64,
}

impl fmt::Debug for Vp8VideoDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vp8VideoDecoder")
            .field("configured", &self.config.is_some())
            .field("ended", &self.ended)
            .field("queued_frames", &self.ready.len())
            .finish_non_exhaustive()
    }
}

impl Vp8VideoDecoder {
    /// Construct a new VP8 decoder session with an unbounded storage budget.
    #[must_use]
    pub fn new() -> Self {
        Self::with_budget(StorageBudget::new(usize::MAX, usize::MAX), None)
    }

    /// Construct a VP8 decoder session accounting decoded output planes against `storage`.
    #[must_use]
    pub fn with_budget(storage: StorageBudget, max_mbs: Option<usize>) -> Self {
        let dec = match max_mbs {
            Some(m) => Vp8Decoder::with_max_macroblocks(m),
            None => Vp8Decoder::new(),
        };
        Self {
            dec,
            storage,
            owned_pulse: None,
            config: None,
            ended: false,
            ready: VecDeque::new(),
            next_ordinal: 0,
            info: None,
            max_coded_extent: None,
            max_pixels: None,
            max_input_bytes: None,
            total_input_bytes: 0,
        }
    }

    /// Attach an owned cancellation pulse polled on every session call.
    #[must_use]
    pub fn with_pulse(mut self, pulse: SharedPulse) -> Self {
        self.owned_pulse = Some(pulse);
        self
    }

    /// Cap maximum admitted coded `(width, height)`.
    #[must_use]
    pub const fn with_max_coded_extent(mut self, extent: (u32, u32)) -> Self {
        self.max_coded_extent = Some(extent);
        self
    }

    /// Cap maximum admitted frame pixel count.
    #[must_use]
    pub const fn with_max_pixels(mut self, max_pixels: Option<u64>) -> Self {
        self.max_pixels = max_pixels;
        self
    }

    /// Cap cumulative input payload bytes accepted across packets.
    #[must_use]
    pub const fn with_max_input_bytes(mut self, max_input_bytes: Option<u64>) -> Self {
        self.max_input_bytes = max_input_bytes;
        self
    }

    fn check_pulse(&self, pulse: Option<&dyn Pulse>) -> Res<()> {
        if let Some(p) = pulse {
            p.check()?;
        }
        if let Some(owned) = &self.owned_pulse {
            owned.check()?;
        }
        Ok(())
    }

    fn make_interpretation(
        &mut self,
        w: u32,
        h: u32,
        config: &CodecConfig,
    ) -> Res<Arc<FrameInterpretation>> {
        let enc = SampleEncoding::new(SampleType::U8, 8, 0)
            .map_err(|_| SessionError::Codec(Vp8SessionError::InputMismatch))?;
        let layout = PlanarLayout::new(
            vec![
                PlaneSpec::new(PlaneRole::Luma, PlaneEncoding::Code(enc)),
                PlaneSpec::new(PlaneRole::Cb, PlaneEncoding::Code(enc)),
                PlaneSpec::new(PlaneRole::Cr, PlaneEncoding::Code(enc)),
            ],
            ChromaSubsampling::Yuv420,
            ChromaSiting::Unknown,
        )
        .map_err(|_| SessionError::Codec(Vp8SessionError::InputMismatch))?;
        // RFC 6386 VP8 signals only limited-range BT.601 (0) or reserved (1);
        // container CICP overrides/refines unspecified primaries/transfer/matrix.
        let bitstream = Cicp::new(6, 6, 6, false);
        let container = config.video.and_then(|v| v.color);
        let resolved = container.unwrap_or(bitstream);
        let interp = FrameInterpretation::new(FrameGeometry::full(w, h), layout)
            .with_color(ColorClaim::new(Some(bitstream), container, Some(resolved)))
            .with_config_epoch(config.epoch);
        Ok(match &self.info {
            Some(i) if *i.interpretation == interp => Arc::clone(&i.interpretation),
            _ => {
                let a = Arc::new(interp);
                self.info = Some(VideoStreamInfo::new(Arc::clone(&a)));
                a
            }
        })
    }
}

impl Default for Vp8VideoDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoDecoder for Vp8VideoDecoder {
    type Error = Vp8SessionError;

    fn configure(&mut self, config: &CodecConfig, pulse: Option<&dyn Pulse>) -> Res<()> {
        self.check_pulse(pulse)?;
        if config.codec != CodecId::Vp8 {
            return Err(SessionError::Unsupported(UnsupportedMedia::Codec));
        }
        self.config = Some(Arc::new(config.clone()));
        self.ended = false;
        Ok(())
    }

    fn send(&mut self, packet: &mut Packet, pulse: Option<&dyn Pulse>) -> Res<SendStatus> {
        self.check_pulse(pulse)?;
        let Some(config) = self.config.as_ref().map(Arc::clone) else {
            return Err(SessionError::Protocol(ProtocolViolation::NotConfigured));
        };
        if self.ended {
            return Err(SessionError::Protocol(ProtocolViolation::SendAfterEnd));
        }
        if !self.ready.is_empty() {
            return Ok(SendStatus::ReceivePending);
        }
        let bytes = packet.bytes();
        if bytes.is_empty() {
            return Err(SessionError::Protocol(ProtocolViolation::EmptyPacket));
        }
        let next_input_total = self.total_input_bytes.saturating_add(bytes.len() as u64);
        if let Some(cap) = self.max_input_bytes
            && next_input_total > cap
        {
            return Err(SessionError::Limit(
                zencodec_media::limits::MediaLimitExceeded::check(
                    zencodec::LimitKind::InputSize,
                    next_input_total,
                    cap,
                )
                .expect_err("input bytes exceeded"),
            ));
        }
        let (is_keyframe, _show) = vp8_frame_tag(bytes).ok_or(SessionError::Codec(
            Vp8SessionError::Decode(DecodeError::NotEnoughData),
        ))?;
        // Preflight keyframe coded dimensions BEFORE native decode/allocation, so an
        // overwide or invisible keyframe with equal macroblock count cannot allocate.
        if is_keyframe {
            if bytes.len() < 10 {
                return Err(SessionError::Codec(Vp8SessionError::Decode(
                    DecodeError::NotEnoughData,
                )));
            }
            if bytes[3..6] != [0x9d, 0x01, 0x2a] {
                return Err(SessionError::Codec(Vp8SessionError::Decode(
                    DecodeError::InvalidSignature,
                )));
            }
            let kw = u32::from(u16::from_le_bytes([bytes[6], bytes[7]]) & 0x3FFF);
            let kh = u32::from(u16::from_le_bytes([bytes[8], bytes[9]]) & 0x3FFF);
            if kw == 0 || kh == 0 {
                return Err(SessionError::Codec(Vp8SessionError::Decode(
                    DecodeError::InvalidDimensions,
                )));
            }
            if let Some((mw, mh)) = self.max_coded_extent
                && (kw > mw || kh > mh)
            {
                return Err(SessionError::Codec(Vp8SessionError::Decode(
                    DecodeError::TooLarge,
                )));
            }
            if let Some(mp) = self.max_pixels
                && u64::from(kw).saturating_mul(u64::from(kh)) > mp
            {
                return Err(SessionError::Codec(Vp8SessionError::Decode(
                    DecodeError::TooLarge,
                )));
            }
        }
        self.dec
            .decode(bytes)
            .map_err(|e| SessionError::Codec(Vp8SessionError::Decode(e)))?;
        self.total_input_bytes = next_input_total;

        let ordinal = packet.ordinal().unwrap_or(self.next_ordinal);
        self.next_ordinal = self.next_ordinal.max(ordinal.saturating_add(1));
        let pts = packet.pts();
        let dur = packet.duration_ticks();
        let input_offset = packet.input_offset();
        let _ = packet.take();

        while let Some(df) = self.dec.next_frame() {
            let w = u32::try_from(df.width).map_err(|_| {
                SessionError::Codec(Vp8SessionError::Decode(DecodeError::InvalidDimensions))
            })?;
            let h = u32::try_from(df.height).map_err(|_| {
                SessionError::Codec(Vp8SessionError::Decode(DecodeError::InvalidDimensions))
            })?;
            if let Some((mw, mh)) = self.max_coded_extent
                && (w > mw || h > mh)
            {
                return Err(SessionError::Codec(Vp8SessionError::Decode(
                    DecodeError::TooLarge,
                )));
            }
            let mut y = self
                .storage
                .bytes(df.y.len())
                .map_err(|_| SessionError::Codec(Vp8SessionError::Allocation))?;
            y.as_mut_slice().copy_from_slice(&df.y);
            let mut u = self
                .storage
                .bytes(df.u.len())
                .map_err(|_| SessionError::Codec(Vp8SessionError::Allocation))?;
            u.as_mut_slice().copy_from_slice(&df.u);
            let mut v = self
                .storage
                .bytes(df.v.len())
                .map_err(|_| SessionError::Codec(Vp8SessionError::Allocation))?;
            v.as_mut_slice().copy_from_slice(&df.v);

            let pic = AccountedYuv420 {
                width: w,
                height: h,
                y,
                u,
                v,
            };
            let interp = self.make_interpretation(w, h, &config)?;
            let mut timing = FrameTiming::default();
            timing.pts = pts;
            if let Some(d) = dur {
                timing = timing.with_duration_ticks(d);
            }
            timing = timing.with_packet(Some(ordinal), input_offset);
            let mut pres = Presentation::default();
            pres.keyframe = is_keyframe;
            self.ready
                .push_back(Frame::new(Arc::new(pic), interp, timing, pres));
        }

        Ok(SendStatus::Accepted)
    }

    fn receive(&mut self, pulse: Option<&dyn Pulse>) -> Res<Receive<Frame>> {
        self.check_pulse(pulse)?;
        if self.config.is_none() {
            return Err(SessionError::Protocol(ProtocolViolation::NotConfigured));
        }
        if let Some(frame) = self.ready.pop_front() {
            return Ok(Receive::Ready(frame));
        }
        if self.ended {
            Ok(Receive::EndOfStream)
        } else {
            Ok(Receive::NeedInput)
        }
    }

    fn end_input(&mut self, pulse: Option<&dyn Pulse>) -> Res<()> {
        self.check_pulse(pulse)?;
        self.ended = true;
        Ok(())
    }

    fn reset(&mut self, pulse: Option<&dyn Pulse>) -> Res<()> {
        self.check_pulse(pulse)?;
        self.dec.reset();
        self.ready.clear();
        self.ended = false;
        self.next_ordinal = 0;
        self.total_input_bytes = 0;
        Ok(())
    }

    fn stream_info(&self) -> Option<&VideoStreamInfo> {
        self.info.as_ref()
    }
}

/// Shareable [`VideoDecodeFactory`] for VP8 streams backed by [`Vp8VideoDecoder`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Vp8DecodeFactory;

impl Vp8DecodeFactory {
    /// Create a VP8 decode factory.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    fn open_concrete(
        &self,
        plan: &VideoDecodePlan,
        run: &JobResources,
    ) -> Result<Vp8VideoDecoder, MediaError> {
        run.check_stop()?;
        if plan.config.codec != CodecId::Vp8 {
            return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
        }
        let limits = run.limits();
        if limits.max_memory_bytes == Some(0) || limits.max_pixels == Some(0) {
            return Err(MediaError::Invalid(
                "VP8 decoder refused zero memory or pixel limit",
            ));
        }
        let (pw, ph) = plan.maximum_coded_extent;
        let mw = limits.max_width.map_or(pw, |l| l.min(pw));
        let mh = limits.max_height.map_or(ph, |l| l.min(ph));
        if mw == 0 || mh == 0 || limits.check_dimensions(mw, mh).is_err() {
            return Err(MediaError::Invalid(
                "VP8 coded extent exceeds JobResources dimension limits",
            ));
        }
        let max_mbs = (mw.div_ceil(16) as usize).saturating_mul(mh.div_ceil(16) as usize);
        let mut dec = Vp8VideoDecoder::with_budget(run.storage().clone(), Some(max_mbs.max(1)))
            .with_pulse(run.pulse().clone())
            .with_max_coded_extent((mw, mh))
            .with_max_pixels(limits.max_pixels)
            .with_max_input_bytes(limits.max_input_bytes);
        dec.configure(&plan.config, Some(run.pulse()))
            .map_err(MediaError::from)?;
        Ok(dec)
    }
}

impl VideoDecodeFactory for Vp8DecodeFactory {
    fn quote(
        &self,
        config: &CodecConfig,
        need: &VideoDecodeNeeds,
        run: &JobResources,
    ) -> Result<VideoDecodePlan, MediaError> {
        run.check_stop()?;
        if config.codec != CodecId::Vp8 {
            return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
        }
        if run.limits().max_memory_bytes == Some(0) {
            return Err(MediaError::Invalid("VP8 decoder refused zero memory limit"));
        }
        let (w, h) = need.maximum_coded_extent;
        if w == 0 || h == 0 || w > 0x3FFF || h > 0x3FFF {
            return Err(MediaError::Invalid(
                "VP8 coded extent must be within 1..=16383",
            ));
        }
        if run.limits().check_dimensions(w, h).is_err() {
            return Err(MediaError::Invalid(
                "VP8 coded extent exceeds JobResources dimension limits",
            ));
        }
        // VP8 maintains 4 reference picture buffers internally (New/Last/Golden/AltRef).
        if need.max_held_pictures < 4 {
            return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
        }
        let expected_enc = SampleEncoding::new(SampleType::U8, 8, 0).expect("valid U8");
        if !need.accepted_layouts.is_empty()
            && !need.accepted_layouts.iter().any(|l| {
                l.subsampling() == ChromaSubsampling::Yuv420
                    && l.siting() == ChromaSiting::Unknown
                    && l.planes().len() == 3
                    && l.planes().iter().map(|p| p.role()).eq([
                        PlaneRole::Luma,
                        PlaneRole::Cb,
                        PlaneRole::Cr,
                    ])
                    && l.planes()
                        .iter()
                        .all(|p| p.encoding() == PlaneEncoding::Code(expected_enc))
            })
        {
            return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
        }
        // 4 reference FrameBufs + working buffers: ~8x raw 4:2:0 frame bytes.
        let padded_w = u64::from(w.div_ceil(16) * 16 + 64);
        let padded_h = u64::from(h.div_ceil(16) * 16 + 64);
        let one_frame = padded_w.saturating_mul(padded_h).saturating_mul(3) / 2;
        let workspace = one_frame.saturating_mul(6).saturating_add(256 << 10);
        let mut plan = VideoDecodePlan::new(
            config.clone(),
            need.maximum_coded_extent,
            NativeBound::UpperBound(workspace),
            4,
        );
        plan.is_send = true;
        Ok(plan)
    }

    fn open(
        &self,
        plan: &VideoDecodePlan,
        run: &JobResources,
    ) -> Result<Box<LocalVideoDecoder>, MediaError> {
        let dec = self.open_concrete(plan, run)?;
        Ok(Box::new(VideoDecoderShim(dec)))
    }

    fn open_send(
        &self,
        plan: &VideoDecodePlan,
        run: &JobResources,
    ) -> Result<Box<DynVideoDecoder>, MediaError> {
        let dec = self.open_concrete(plan, run)?;
        Ok(erase_video_decoder(dec))
    }
}

#[cfg(feature = "encoder")]
pub use encode::{Vp8EncodeFactory, Vp8VideoEncoder};

#[cfg(feature = "encoder")]
mod encode {
    use super::*;
    use zencodec::encode::{Fidelity, LossyTarget};
    use zencodec_media::registry::{
        LocalVideoEncoder, VideoEncodeFactory, VideoEncodeNeeds, VideoEncodePlan,
    };
    use zencodec_media::session::{
        DynVideoEncoder, VideoEncoder, VideoEncoderShim, erase_video_encoder,
    };
    use zencodec_media::track::VideoTrackInfo;

    use crate::{EncoderConfig, Vp8Encoder};

    /// Pure-Rust VP8 video encoder session implementing [`VideoEncoder`].
    pub struct Vp8VideoEncoder {
        enc: Vp8Encoder,
        width: u32,
        height: u32,
        clock: TimeBase,
        output_cfg: CodecConfig,
        owned_pulse: Option<SharedPulse>,
        ready: VecDeque<Packet>,
        last_pts: Option<i64>,
        ended: bool,
    }

    impl fmt::Debug for Vp8VideoEncoder {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("Vp8VideoEncoder")
                .field("width", &self.width)
                .field("height", &self.height)
                .field("ended", &self.ended)
                .finish_non_exhaustive()
        }
    }

    impl Vp8VideoEncoder {
        /// Construct a VP8 encoder session for `cfg` on `clock`.
        pub fn new(
            cfg: EncoderConfig,
            clock: TimeBase,
            color: Option<Cicp>,
        ) -> Result<Self, Vp8SessionError> {
            let enc = Vp8Encoder::new(cfg).map_err(Vp8SessionError::Encode)?;
            let w = u32::try_from(cfg.width).map_err(|_| Vp8SessionError::InputMismatch)?;
            let h = u32::try_from(cfg.height).map_err(|_| Vp8SessionError::InputMismatch)?;
            let mut vinfo = VideoTrackInfo::new(w, h);
            if let Some(c) = color {
                vinfo = vinfo.with_color(c);
            }
            let output_cfg = CodecConfig::new(CodecId::Vp8, clock).with_video(vinfo);
            Ok(Self {
                enc,
                width: w,
                height: h,
                clock,
                output_cfg,
                owned_pulse: None,
                ready: VecDeque::new(),
                last_pts: None,
                ended: false,
            })
        }

        /// Attach an owned cancellation pulse polled on every session call.
        #[must_use]
        pub fn with_pulse(mut self, pulse: SharedPulse) -> Self {
            self.owned_pulse = Some(pulse);
            self
        }

        fn check_pulse(&self, pulse: Option<&dyn Pulse>) -> Res<()> {
            if let Some(p) = pulse {
                p.check()?;
            }
            if let Some(owned) = &self.owned_pulse {
                owned.check()?;
            }
            Ok(())
        }
    }

    impl VideoEncoder for Vp8VideoEncoder {
        type Error = Vp8SessionError;

        fn send(&mut self, frame: &Frame, pulse: Option<&dyn Pulse>) -> Res<SendStatus> {
            self.check_pulse(pulse)?;
            if self.ended {
                return Err(SessionError::Protocol(ProtocolViolation::SendAfterEnd));
            }
            if !self.ready.is_empty() {
                return Ok(SendStatus::ReceivePending);
            }
            let interp = frame.interpretation();
            let geom = interp.geometry;
            if geom.coded_width != self.width
                || geom.coded_height != self.height
                || geom.crop.x != 0
                || geom.crop.y != 0
                || geom.crop.width != self.width
                || geom.crop.height != self.height
                || interp.layout.subsampling() != ChromaSubsampling::Yuv420
                || interp.layout.planes().len() != 3
            {
                return Err(SessionError::Codec(Vp8SessionError::InputMismatch));
            }
            if interp.color.resolved.is_some_and(|c| c.full_range) {
                // RFC 6386 VP8 encodes limited-range YCbCr only; refuse full-range honestly.
                return Err(SessionError::Codec(Vp8SessionError::InputMismatch));
            }
            let pts = frame
                .timing()
                .pts
                .ok_or(SessionError::Protocol(ProtocolViolation::MissingTimestamp))?;
            if pts.base() != self.clock {
                return Err(SessionError::Protocol(ProtocolViolation::ClockMismatch));
            }
            if self.last_pts.is_some_and(|last| pts.ticks() <= last) {
                return Err(SessionError::Codec(Vp8SessionError::NonMonotonicTimestamp));
            }

            let mapped = frame
                .picture()
                .map()
                .map_err(|_| SessionError::Codec(Vp8SessionError::InputMismatch))?;
            let (Some(AnyPlane::U8(yp)), Some(AnyPlane::U8(up)), Some(AnyPlane::U8(vp))) =
                (mapped.plane(0), mapped.plane(1), mapped.plane(2))
            else {
                return Err(SessionError::Codec(Vp8SessionError::InputMismatch));
            };
            if yp.width() != self.width as usize
                || yp.height() != self.height as usize
                || up.width() != self.width.div_ceil(2) as usize
                || up.height() != self.height.div_ceil(2) as usize
                || vp.width() != up.width()
                || vp.height() != up.height()
                || up.stride() != vp.stride()
            {
                return Err(SessionError::Codec(Vp8SessionError::InputMismatch));
            }

            self.enc
                .push_frame(
                    yp.strided_samples(),
                    up.strided_samples(),
                    vp.strided_samples(),
                    yp.stride(),
                    up.stride(),
                )
                .map_err(|e| SessionError::Codec(Vp8SessionError::Encode(e)))?;

            if let Some(bytes) = self.enc.pull_packet() {
                let is_kf = vp8_frame_tag(&bytes).is_some_and(|(kf, _)| kf);
                let mut pkt = Packet::from_vec(bytes)
                    .map_err(|_| SessionError::Protocol(ProtocolViolation::EmptyPacket))?
                    .with_track(0)
                    .with_pts(pts)
                    .with_dts(pts)
                    .with_random_access(if is_kf {
                        RandomAccess::Yes
                    } else {
                        RandomAccess::No
                    });
                if let Some(d) = frame.timing().duration_ticks {
                    pkt = pkt.with_duration_ticks(d);
                }
                self.ready.push_back(pkt);
            }
            self.last_pts = Some(pts.ticks());
            Ok(SendStatus::Accepted)
        }

        fn receive(&mut self, pulse: Option<&dyn Pulse>) -> Res<Receive<Packet>> {
            self.check_pulse(pulse)?;
            if let Some(pkt) = self.ready.pop_front() {
                return Ok(Receive::Ready(pkt));
            }
            if self.ended {
                Ok(Receive::EndOfStream)
            } else {
                Ok(Receive::NeedInput)
            }
        }

        fn end_input(&mut self, pulse: Option<&dyn Pulse>) -> Res<()> {
            self.check_pulse(pulse)?;
            self.ended = true;
            Ok(())
        }

        fn output_config(&self) -> Option<&CodecConfig> {
            Some(&self.output_cfg)
        }
    }

    /// Shareable [`VideoEncodeFactory`] for VP8 streams backed by [`Vp8VideoEncoder`].
    #[derive(Clone, Copy, Debug)]
    pub struct Vp8EncodeFactory {
        default_qindex: i32,
        keyframe_interval: usize,
    }

    impl Default for Vp8EncodeFactory {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Vp8EncodeFactory {
        /// Create a VP8 encode factory (default `qindex = 30`, keyframe interval 120).
        #[must_use]
        pub const fn new() -> Self {
            Self {
                default_qindex: 30,
                keyframe_interval: 120,
            }
        }

        /// Set default VP8 base quantizer index (`0..=127`).
        #[must_use]
        pub const fn with_qindex(mut self, qindex: i32) -> Self {
            self.default_qindex = qindex;
            self
        }

        /// Set keyframe interval in frames (`0` = first frame only).
        #[must_use]
        pub const fn with_keyframe_interval(mut self, interval: usize) -> Self {
            self.keyframe_interval = interval;
            self
        }

        fn build_config(
            &self,
            interp: &FrameInterpretation,
            fidelity: Option<&Fidelity>,
        ) -> Result<EncoderConfig, MediaError> {
            let geom = interp.geometry;
            if geom.coded_width == 0
                || geom.coded_height == 0
                || geom.coded_width > 0x3FFF
                || geom.coded_height > 0x3FFF
                || geom.crop.x != 0
                || geom.crop.y != 0
                || geom.crop.width != geom.coded_width
                || geom.crop.height != geom.coded_height
                || interp.layout.subsampling() != ChromaSubsampling::Yuv420
                || interp.layout.planes().len() != 3
            {
                return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
            }
            if !interp.layout.planes().iter().map(|p| p.role()).eq([
                PlaneRole::Luma,
                PlaneRole::Cb,
                PlaneRole::Cr,
            ]) {
                return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
            }
            let expected_enc = SampleEncoding::new(SampleType::U8, 8, 0).expect("valid U8");
            for plane in interp.layout.planes() {
                if plane.encoding() != PlaneEncoding::Code(expected_enc) {
                    return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
                }
            }
            if interp.color.resolved.is_some_and(|c| c.full_range) {
                return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
            }
            let qindex = match fidelity {
                None => self.default_qindex,
                Some(Fidelity::Lossless) => {
                    // VP8 has no mathematically lossless mode.
                    return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
                }
                Some(Fidelity::Lossy(LossyTarget::CodecSpecificQuality(q))) => {
                    if !q.is_finite() {
                        return Err(MediaError::Invalid("VP8 quality must be finite"));
                    }
                    let clamped = q.clamp(0.0, 100.0);
                    ((100.0 - clamped) * 1.27 + 0.5) as i32
                }
                Some(_) => return Err(MediaError::Unsupported(UnsupportedMedia::Codec)),
            };
            let cfg = EncoderConfig {
                width: geom.coded_width as usize,
                height: geom.coded_height as usize,
                qindex: qindex.clamp(0, 127),
                keyframe_interval: self.keyframe_interval,
            };
            cfg.validate()
                .map_err(|_| MediaError::Unsupported(UnsupportedMedia::Codec))?;
            Ok(cfg)
        }

        fn open_concrete(
            &self,
            plan: &VideoEncodePlan,
            run: &JobResources,
        ) -> Result<Vp8VideoEncoder, MediaError> {
            run.check_stop()?;
            if plan.codec != CodecId::Vp8 {
                return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
            }
            let cfg = self.build_config(&plan.interpretation, plan.fidelity.as_ref())?;
            let color = plan.interpretation.color.resolved;
            let enc = Vp8VideoEncoder::new(cfg, plan.clock, color)
                .map_err(MediaError::backend)?
                .with_pulse(run.pulse().clone());
            Ok(enc)
        }
    }

    impl VideoEncodeFactory for Vp8EncodeFactory {
        fn quote(
            &self,
            need: &VideoEncodeNeeds,
            run: &JobResources,
        ) -> Result<VideoEncodePlan, MediaError> {
            run.check_stop()?;
            if need.codec != CodecId::Vp8 {
                return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
            }
            if need.max_in_flight_frames == 0 {
                return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
            }
            if run.limits().max_memory_bytes == Some(0) {
                return Err(MediaError::Invalid("VP8 encoder refused zero memory limit"));
            }
            let geom = need.interpretation.geometry;
            if run
                .limits()
                .check_dimensions(geom.coded_width, geom.coded_height)
                .is_err()
            {
                return Err(MediaError::Invalid(
                    "VP8 encode dimensions exceed JobResources limits",
                ));
            }
            let _cfg = self.build_config(&need.interpretation, need.fidelity.as_ref())?;
            let _ = ThreadingPolicy::Sequential;
            let padded_w = u64::from(geom.coded_width.div_ceil(16) * 16 + 64);
            let padded_h = u64::from(geom.coded_height.div_ceil(16) * 16 + 64);
            let workspace = padded_w
                .saturating_mul(padded_h)
                .saturating_mul(6)
                .saturating_add(512 << 10);
            let mut plan = VideoEncodePlan::new(
                CodecId::Vp8,
                Arc::clone(&need.interpretation),
                need.clock,
                NativeBound::UpperBound(workspace),
                0,
            )
            .with_fidelity(need.fidelity);
            plan.is_send = true;
            Ok(plan)
        }

        fn open(
            &self,
            plan: &VideoEncodePlan,
            run: &JobResources,
        ) -> Result<Box<LocalVideoEncoder>, MediaError> {
            let enc = self.open_concrete(plan, run)?;
            Ok(Box::new(VideoEncoderShim(enc)))
        }

        fn open_send(
            &self,
            plan: &VideoEncodePlan,
            run: &JobResources,
        ) -> Result<Box<DynVideoEncoder>, MediaError> {
            let enc = self.open_concrete(plan, run)?;
            Ok(erase_video_encoder(enc))
        }
    }
}

/// Register [`Vp8DecodeFactory`] (and [`Vp8EncodeFactory`] when feature `encoder` is enabled)
/// in a [`zencodec::CodecSet`] under `org.imazen.vp8.zenvp8`.
pub fn register_vp8_codec_set(
    set: zencodec::CodecSet,
) -> Result<zencodec::CodecSet, zencodec::RegistrationError> {
    use zencodec::{NamedFactory, RegistrationId};
    use zencodec_media::registry::VideoDecodeRole;

    let id = RegistrationId::new("org.imazen.vp8.zenvp8").expect("valid registration id");
    let set = set.with_role::<VideoDecodeRole>(
        [CodecId::Vp8],
        NamedFactory::new(
            id.clone(),
            env!("CARGO_PKG_VERSION"),
            Arc::new(Vp8DecodeFactory::new()) as Arc<dyn VideoDecodeFactory>,
        ),
    )?;
    #[cfg(feature = "encoder")]
    let set = {
        use zencodec_media::registry::{VideoEncodeFactory, VideoEncodeRole};
        set.with_role::<VideoEncodeRole>(
            [CodecId::Vp8],
            NamedFactory::new(
                id,
                env!("CARGO_PKG_VERSION"),
                Arc::new(Vp8EncodeFactory::new()) as Arc<dyn VideoEncodeFactory>,
            ),
        )?
    };
    Ok(set)
}
