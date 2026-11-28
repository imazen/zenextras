//! `zencodec-media` video decoder and encoder sessions + shareable factories for VP9,
//! backed by `rusty_vp9 =0.3.0` and the `#![no_std]` header/superframe parser in [`crate::header`].

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use zencodec::encode::{Fidelity, LossyTarget};
use zencodec::enough::Stop as _;
use zencodec::plane::{AnyPlane, PlanarLayout, PlaneEncoding, PlaneRef, PlaneSpec};
use zencodec::source::NativeBound;
use zencodec::storage::{OwnedStorage, StorageBudget};
use zencodec::time::{TimeBase, Timestamp};
use zencodec::{JobResources, Pulse, SharedPulse, ThreadingPolicy};
use zencodec_media::frame::{
    ColorClaim, Frame, FrameGeometry, FrameInterpretation, FrameTiming, MapError, MappedPlanes,
    Picture, PlaneRole, Presentation,
};
use zencodec_media::packet::{Packet, RandomAccess};
use zencodec_media::registry::{
    LocalVideoDecoder, LocalVideoEncoder, MediaError, VideoDecodeFactory, VideoDecodeNeeds,
    VideoDecodePlan, VideoEncodeFactory, VideoEncodeNeeds, VideoEncodePlan,
};
use zencodec_media::session::{
    CodecConfig, DynVideoDecoder, DynVideoEncoder, ProtocolViolation, Receive, SendStatus,
    SessionError, UnsupportedMedia, VideoDecoder, VideoDecoderShim, VideoEncoder, VideoEncoderShim,
    VideoStreamInfo, erase_video_decoder, erase_video_encoder,
};
use zencodec_media::track::{CodecId, VideoTrackInfo};
use zenpixels::sample::{ChromaSiting, ChromaSubsampling, SampleEncoding};
use zenpixels::{Cicp, SampleType};

use crate::header::{
    Vp9CodecConfig, Vp9ColorSpace, Vp9Subsampling, parse_codec_private, parse_frame_header,
    split_superframe,
};

/// Errors produced by VP9 `zencodec-media` sessions.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Vp9SessionError {
    /// Underlying VP9 decoder/encoder error.
    Backend(String),
    /// Plane allocation exceeded the configured [`StorageBudget`].
    Allocation,
    /// Input frame dimensions, layout, depth, or range do not match encoder config.
    InputMismatch,
    /// Presentation timestamp was not strictly increasing.
    NonMonotonicTimestamp,
}

impl fmt::Display for Vp9SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(msg) => write!(f, "VP9 backend error: {msg}"),
            Self::Allocation => f.write_str("VP9 plane storage budget exceeded"),
            Self::InputMismatch => f.write_str("frame does not match VP9 encoder configuration"),
            Self::NonMonotonicTimestamp => {
                f.write_str("VP9 frame timestamp is not strictly increasing")
            }
        }
    }
}

impl core::error::Error for Vp9SessionError {}

type Res<T> = Result<T, SessionError<Vp9SessionError>>;

enum Vp9PictureStorage {
    U8 {
        planes: [OwnedStorage<u8>; 3],
        dims: [(usize, usize); 3],
        strides: [usize; 3],
    },
    U16 {
        planes: [OwnedStorage<u16>; 3],
        dims: [(usize, usize); 3],
        strides: [usize; 3],
    },
}

struct MappedVp9<'a> {
    storage: &'a Vp9PictureStorage,
}

impl MappedPlanes for MappedVp9<'_> {
    fn plane_count(&self) -> usize {
        3
    }

    fn plane(&self, index: usize) -> Option<AnyPlane<'_>> {
        if index >= 3 {
            return None;
        }
        match self.storage {
            Vp9PictureStorage::U8 {
                planes,
                dims,
                strides,
            } => {
                let (w, h) = dims[index];
                let p = PlaneRef::new(planes[index].as_slice(), w, h, strides[index]).ok()?;
                Some(AnyPlane::U8(p))
            }
            Vp9PictureStorage::U16 {
                planes,
                dims,
                strides,
            } => {
                let (w, h) = dims[index];
                let p = PlaneRef::new(planes[index].as_slice(), w, h, strides[index]).ok()?;
                Some(AnyPlane::U16(p))
            }
        }
    }
}

impl Picture for Vp9PictureStorage {
    fn map(&self) -> Result<Box<dyn MappedPlanes + '_>, MapError> {
        Ok(Box::new(MappedVp9 { storage: self }))
    }
}

#[derive(Clone, Copy)]
struct Vp9RefInfo {
    coded_size: (u32, u32),
    render_size: (u32, u32),
    color_space: Option<Vp9ColorSpace>,
    full_range: Option<bool>,
}

#[derive(Clone, Copy)]
struct PendingTiming {
    pts: Option<Timestamp>,
    duration_ticks: Option<u64>,
    render_size: Option<(u32, u32)>,
    color_space: Option<Vp9ColorSpace>,
    full_range: Option<bool>,
    keyframe: bool,
    show_existing: bool,
    ordinal: u64,
    input_offset: Option<u64>,
}

/// Pure-Rust VP9 video decoder session implementing [`VideoDecoder`],
/// combining `rusty_vp9::Vp9Decoder` with [`crate::header`] so `color_range`,
/// `color_space`, `bit_depth`, subsampling, and `render_size` are preserved faithfully.
pub struct Vp9VideoDecoder {
    dec: rusty_vp9::Vp9Decoder,
    config: Option<Arc<CodecConfig>>,
    budget: StorageBudget,
    max_width: Option<u32>,
    max_height: Option<u32>,
    max_pixels: Option<u64>,
    max_input_bytes: Option<u64>,
    total_input_bytes: u64,
    expected_private: Option<Vp9CodecConfig>,
    owned_pulse: Option<SharedPulse>,
    stream_color_space: Option<Vp9ColorSpace>,
    stream_full_range: Option<bool>,
    stream_render_size: Option<(u32, u32)>,
    ref_slots: [Option<Vp9RefInfo>; 8],
    pending_meta: VecDeque<PendingTiming>,
    ready: VecDeque<Frame>,
    stream_info: Option<VideoStreamInfo>,
    ended: bool,
    seen_keyframe: bool,
    next_ordinal: u64,
    synthetic_pts: i64,
}

impl fmt::Debug for Vp9VideoDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vp9VideoDecoder")
            .field("configured", &self.config.is_some())
            .field("ended", &self.ended)
            .field("ready", &self.ready.len())
            .finish_non_exhaustive()
    }
}

impl Vp9VideoDecoder {
    /// Construct a VP9 video decoder session backed by `budget`.
    #[must_use]
    pub fn new(budget: StorageBudget) -> Self {
        Self {
            dec: rusty_vp9::Vp9Decoder::new(),
            config: None,
            budget,
            max_width: None,
            max_height: None,
            max_pixels: None,
            max_input_bytes: None,
            total_input_bytes: 0,
            expected_private: None,
            owned_pulse: None,
            stream_color_space: None,
            stream_full_range: None,
            stream_render_size: None,
            ref_slots: [None; 8],
            pending_meta: VecDeque::new(),
            ready: VecDeque::new(),
            stream_info: None,
            ended: false,
            seen_keyframe: false,
            next_ordinal: 0,
            synthetic_pts: 0,
        }
    }

    /// Attach an owned cancellation pulse polled on every session call.
    #[must_use]
    pub fn with_pulse(mut self, pulse: SharedPulse) -> Self {
        self.owned_pulse = Some(pulse);
        self
    }

    /// Enforce maximum decoded dimensions / pixel count.
    #[must_use]
    pub fn with_dimension_limits(
        mut self,
        max_width: Option<u32>,
        max_height: Option<u32>,
        max_pixels: Option<u64>,
    ) -> Self {
        self.max_width = max_width;
        self.max_height = max_height;
        self.max_pixels = max_pixels;
        self
    }

    /// Cap cumulative input payload bytes accepted across packets.
    #[must_use]
    pub const fn with_max_input_bytes(mut self, max_input_bytes: Option<u64>) -> Self {
        self.max_input_bytes = max_input_bytes;
        self
    }

    pub(crate) const fn with_expected_private(mut self, cfg: Vp9CodecConfig) -> Self {
        self.expected_private = Some(cfg);
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

    fn check_dims(&self, w: u32, h: u32) -> Res<()> {
        if w == 0 || h == 0 {
            return Err(SessionError::Codec(Vp9SessionError::Backend(String::from(
                "zero VP9 frame dimensions",
            ))));
        }
        if self.max_width.is_some_and(|m| w > m)
            || self.max_height.is_some_and(|m| h > m)
            || self
                .max_pixels
                .is_some_and(|m| u64::from(w).saturating_mul(u64::from(h)) > m)
        {
            return Err(SessionError::Codec(Vp9SessionError::Allocation));
        }
        Ok(())
    }

    fn resolved_color(
        &self,
        frame_cs: Option<Vp9ColorSpace>,
        frame_fr: Option<bool>,
        container: Option<Cicp>,
    ) -> ColorClaim {
        let cs_opt = frame_cs.or(self.stream_color_space);
        let fr_opt = frame_fr.or(self.stream_full_range);
        let bitstream = cs_opt.map(|cs| {
            let (p, t, m) = cs.cicp_triplet();
            let full = fr_opt.unwrap_or(cs == Vp9ColorSpace::Rgb);
            Cicp::new(p, t, m, full)
        });
        let resolved = match (bitstream, container) {
            (Some(b), Some(c)) => {
                let pick = |bv: u8, cv: u8| if bv == 2 { cv } else { bv };
                Some(Cicp::new(
                    pick(b.color_primaries, c.color_primaries),
                    pick(b.transfer_characteristics, c.transfer_characteristics),
                    pick(b.matrix_coefficients, c.matrix_coefficients),
                    b.full_range,
                ))
            }
            (Some(b), None) => Some(b),
            (None, Some(mut c)) => {
                if let Some(full) = fr_opt {
                    c.full_range = full;
                }
                Some(c)
            }
            (None, None) => fr_opt.map(|full| Cicp::new(2, 2, 2, full)),
        };
        ColorClaim::new(bitstream, container, resolved)
    }

    fn build_frame(&mut self, decoded: rusty_vp9::DecodedFrame, cfg: &CodecConfig) -> Res<Frame> {
        let w = decoded.width;
        let h = decoded.height;
        self.check_dims(w, h)?;

        let sub = match (decoded.subsampling_x, decoded.subsampling_y) {
            (1, 1) => ChromaSubsampling::Yuv420,
            (1, 0) => ChromaSubsampling::Yuv422,
            (0, 0) => ChromaSubsampling::Yuv444,
            _ => {
                return Err(SessionError::Codec(Vp9SessionError::Backend(String::from(
                    "unsupported VP9 subsampling",
                ))));
            }
        };
        let cw = if decoded.subsampling_x == 1 {
            w.div_ceil(2)
        } else {
            w
        };
        let ch = if decoded.subsampling_y == 1 {
            h.div_ceil(2)
        } else {
            h
        };
        let dims = [
            (w as usize, h as usize),
            (cw as usize, ch as usize),
            (cw as usize, ch as usize),
        ];

        if decoded.planes.len() < 3 || decoded.strides.len() < 3 {
            return Err(SessionError::Codec(Vp9SessionError::Backend(String::from(
                "VP9 decoder produced fewer than 3 planes",
            ))));
        }

        let bit_depth = decoded.bit_depth as u8;
        let storage = match bit_depth {
            8 => {
                let mut out_planes = Vec::with_capacity(3);
                let mut out_strides = [0usize; 3];
                for i in 0..3 {
                    let (pw, ph) = dims[i];
                    let mut buf = self
                        .budget
                        .bytes(pw.saturating_mul(ph))
                        .map_err(|_| SessionError::Codec(Vp9SessionError::Allocation))?;
                    let src = &decoded.planes[i];
                    let src_stride = decoded.strides[i];
                    let dst = buf.as_mut_slice();
                    for row in 0..ph {
                        let s0 = row * src_stride;
                        let d0 = row * pw;
                        if s0 + pw > src.len() {
                            return Err(SessionError::Codec(Vp9SessionError::Backend(
                                String::from("VP9 plane buffer truncated"),
                            )));
                        }
                        dst[d0..d0 + pw].copy_from_slice(&src[s0..s0 + pw]);
                    }
                    out_planes.push(buf);
                    out_strides[i] = pw;
                }
                let [y, u, v]: [OwnedStorage<u8>; 3] = out_planes
                    .try_into()
                    .map_err(|_| SessionError::Codec(Vp9SessionError::Allocation))?;
                Vp9PictureStorage::U8 {
                    planes: [y, u, v],
                    dims,
                    strides: out_strides,
                }
            }
            10 | 12 => {
                let mut out_planes = Vec::with_capacity(3);
                let mut out_strides = [0usize; 3];
                for i in 0..3 {
                    let (pw, ph) = dims[i];
                    let mut buf = self
                        .budget
                        .words(pw.saturating_mul(ph))
                        .map_err(|_| SessionError::Codec(Vp9SessionError::Allocation))?;
                    let src = &decoded.planes[i];
                    let src_stride = decoded.strides[i];
                    let row_bytes = pw * 2;
                    let dst = buf.as_mut_slice();
                    for row in 0..ph {
                        let s0 = row * src_stride;
                        let d0 = row * pw;
                        if s0 + row_bytes > src.len() {
                            return Err(SessionError::Codec(Vp9SessionError::Backend(
                                String::from("VP9 16-bit plane buffer truncated"),
                            )));
                        }
                        for (x, pair) in src[s0..s0 + row_bytes]
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .enumerate()
                        {
                            dst[d0 + x] = u16::from_le_bytes(*pair);
                        }
                    }
                    out_planes.push(buf);
                    out_strides[i] = pw;
                }
                let [y, u, v]: [OwnedStorage<u16>; 3] = out_planes
                    .try_into()
                    .map_err(|_| SessionError::Codec(Vp9SessionError::Allocation))?;
                Vp9PictureStorage::U16 {
                    planes: [y, u, v],
                    dims,
                    strides: out_strides,
                }
            }
            _ => {
                return Err(SessionError::Codec(Vp9SessionError::Backend(String::from(
                    "unsupported VP9 bit depth",
                ))));
            }
        };

        let meta = self.pending_meta.pop_front();
        let frame_cs = meta.and_then(|m| m.color_space);
        let frame_fr = meta.and_then(|m| m.full_range);
        let color = self.resolved_color(frame_cs, frame_fr, cfg.video.and_then(|v| v.color));
        let is_rgb = frame_cs.or(self.stream_color_space) == Some(Vp9ColorSpace::Rgb)
            || color.resolved.is_some_and(|c| c.matrix_coefficients == 0);
        if is_rgb && sub != ChromaSubsampling::Yuv444 {
            return Err(SessionError::Unsupported(UnsupportedMedia::Codec));
        }
        if let Some(exp) = self.expected_private {
            // Enforce profile-implied depth and subsampling even when TLV omits explicit fields.
            let profile_depth_ok = if exp.profile <= 1 {
                bit_depth == 8
            } else {
                matches!(bit_depth, 10 | 12)
            };
            let profile_sub_ok = if exp.profile == 0 || exp.profile == 2 {
                sub == ChromaSubsampling::Yuv420
            } else {
                matches!(sub, ChromaSubsampling::Yuv422 | ChromaSubsampling::Yuv444)
            };
            if !profile_depth_ok || !profile_sub_ok {
                return Err(SessionError::Unsupported(UnsupportedMedia::Codec));
            }
            if let Some(ebd) = exp.bit_depth
                && bit_depth != ebd
            {
                return Err(SessionError::Unsupported(UnsupportedMedia::Codec));
            }
            if let Some(esub) = exp.subsampling {
                let expected_sub = match esub {
                    Vp9Subsampling::Yuv420 => ChromaSubsampling::Yuv420,
                    Vp9Subsampling::Yuv422 => ChromaSubsampling::Yuv422,
                    Vp9Subsampling::Yuv444 => ChromaSubsampling::Yuv444,
                };
                if sub != expected_sub {
                    return Err(SessionError::Unsupported(UnsupportedMedia::Codec));
                }
            }
            if exp.cicp.is_some_and(|(_, _, m)| m == 0) != is_rgb
                && exp.cicp.is_some_and(|(_, _, m)| m != 2)
            {
                return Err(SessionError::Unsupported(UnsupportedMedia::Codec));
            }
            if let Some(c) = cfg.video.and_then(|v| v.color)
                && c.matrix_coefficients != 2
                && (c.matrix_coefficients == 0) != is_rgb
            {
                return Err(SessionError::Unsupported(UnsupportedMedia::Codec));
            }
        }
        let enc = match bit_depth {
            8 => SampleEncoding::new(SampleType::U8, 8, 0),
            10 => SampleEncoding::new(SampleType::U16, 10, 0),
            _ => SampleEncoding::new(SampleType::U16, 12, 0),
        }
        .map_err(|_| SessionError::Codec(Vp9SessionError::InputMismatch))?;
        let roles = if is_rgb {
            // VP9 CS_RGB encodes planes in G, B, R order (VP9 spec §6.2.2).
            [PlaneRole::Green, PlaneRole::Blue, PlaneRole::Red]
        } else {
            [PlaneRole::Luma, PlaneRole::Cb, PlaneRole::Cr]
        };
        let specs = vec![
            PlaneSpec::new(roles[0], PlaneEncoding::Code(enc)),
            PlaneSpec::new(roles[1], PlaneEncoding::Code(enc)),
            PlaneSpec::new(roles[2], PlaneEncoding::Code(enc)),
        ];
        // VP9 bitstream carries no chroma sample siting flag; report Unknown truthfully.
        let layout = PlanarLayout::new(specs, sub, ChromaSiting::Unknown)
            .map_err(|_| SessionError::Codec(Vp9SessionError::Allocation))?;

        let mut geom = FrameGeometry::full(w, h);
        if let Some(rs) = meta.and_then(|m| m.render_size)
            && rs != (w, h)
        {
            geom.render = Some(rs);
        }
        let interp = Arc::new(
            FrameInterpretation::new(geom, layout)
                .with_color(color)
                .with_config_epoch(cfg.epoch),
        );
        self.stream_info = Some(VideoStreamInfo::new(Arc::clone(&interp)));

        let pts = meta
            .and_then(|m| m.pts)
            .or_else(|| decoded.pts.map(|t| Timestamp::new(t, cfg.time_base)));
        let mut timing = FrameTiming::default();
        timing.pts = pts;
        if let Some(d) = meta.and_then(|m| m.duration_ticks) {
            timing = timing.with_duration_ticks(d);
        }
        if let Some(m) = meta {
            timing = timing.with_packet(Some(m.ordinal), m.input_offset);
        }
        let mut pres = Presentation::default();
        pres.keyframe = meta.is_some_and(|m| m.keyframe);
        pres.show_existing = meta.is_some_and(|m| m.show_existing);
        Ok(Frame::new(Arc::new(storage), interp, timing, pres))
    }

    fn drain_decoder(&mut self, cfg: &CodecConfig, pulse: Option<&dyn Pulse>) -> Res<()> {
        loop {
            self.check_pulse(pulse)?;
            match self.dec.next_frame() {
                Ok(decoded) => {
                    let frame = self.build_frame(decoded, cfg)?;
                    self.ready.push_back(frame);
                }
                Err(rusty_vp9::Error::Again | rusty_vp9::Error::Eof) => break,
                Err(e) => {
                    return Err(SessionError::Codec(Vp9SessionError::Backend(
                        alloc::format!("{e}"),
                    )));
                }
            }
        }
        Ok(())
    }
}

impl VideoDecoder for Vp9VideoDecoder {
    type Error = Vp9SessionError;

    fn configure(&mut self, config: &CodecConfig, pulse: Option<&dyn Pulse>) -> Res<()> {
        self.check_pulse(pulse)?;
        if config.codec != CodecId::Vp9 {
            return Err(SessionError::Unsupported(UnsupportedMedia::Codec));
        }
        self.config = Some(Arc::new(config.clone()));
        Ok(())
    }

    fn send(&mut self, packet: &mut Packet, pulse: Option<&dyn Pulse>) -> Res<SendStatus> {
        self.check_pulse(pulse)?;
        let Some(cfg) = self.config.clone() else {
            return Err(SessionError::Protocol(ProtocolViolation::NotConfigured));
        };
        if self.ended {
            return Err(SessionError::Protocol(ProtocolViolation::SendAfterEnd));
        }
        if !self.ready.is_empty() {
            return Ok(SendStatus::ReceivePending);
        }
        if packet.config_epoch() != cfg.epoch {
            return Err(SessionError::Protocol(ProtocolViolation::EpochMismatch));
        }
        if let Some(pts) = packet.pts()
            && pts.base() != cfg.time_base
        {
            return Err(SessionError::Protocol(ProtocolViolation::ClockMismatch));
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

        let subframes = split_superframe(bytes);
        let ordinal = packet.ordinal().unwrap_or(self.next_ordinal);
        let input_offset = packet.input_offset();
        let pkt_pts = packet.pts();
        let pkt_dur = packet.duration_ticks();

        // Preflight every subframe BEFORE passing the packet to `rusty_vp9`, updating a
        // tentative copy of the 8-slot reference table and recording presentation metadata
        // ONLY for subframes that actually display (`show_frame || show_existing_frame`).
        let mut next_refs = self.ref_slots;
        let mut next_cs = self.stream_color_space;
        let mut next_fr = self.stream_full_range;
        let mut next_rs = self.stream_render_size;
        let mut seen_kf = self.seen_keyframe;
        let mut shown_metas: Vec<PendingTiming> = Vec::new();
        for sub in subframes {
            let hdr = parse_frame_header(sub).ok_or_else(|| {
                SessionError::Codec(Vp9SessionError::Backend(String::from(
                    "malformed VP9 uncompressed frame header",
                )))
            })?;
            if hdr.key_frame {
                seen_kf = true;
            }
            if hdr.show_existing_frame {
                let idx = usize::from(hdr.frame_to_show_map_idx.unwrap_or(0));
                let Some(info) = next_refs.get(idx).and_then(|s| *s) else {
                    return Err(SessionError::Codec(Vp9SessionError::Backend(String::from(
                        "VP9 show_existing_frame references uninitialized slot",
                    ))));
                };
                self.check_dims(info.coded_size.0, info.coded_size.1)?;
                shown_metas.push(PendingTiming {
                    pts: pkt_pts,
                    duration_ticks: pkt_dur,
                    render_size: Some(info.render_size),
                    color_space: info.color_space.or(next_cs),
                    full_range: info.full_range.or(next_fr),
                    keyframe: false,
                    show_existing: true,
                    ordinal,
                    input_offset,
                });
                continue;
            }
            if let Some(cs) = hdr.color_space {
                next_cs = Some(cs);
            }
            if let Some(fr) = hdr.full_range {
                next_fr = Some(fr);
            }
            let coded = hdr.coded_size.or_else(|| {
                hdr.size_from_ref
                    .and_then(|idx| next_refs.get(usize::from(idx)).and_then(|s| *s))
                    .map(|info| info.coded_size)
            });
            let Some((w, h)) = coded else {
                return Err(SessionError::Codec(Vp9SessionError::Backend(String::from(
                    "VP9 inter frame references uninitialized size slot",
                ))));
            };
            self.check_dims(w, h)?;
            let render = hdr.render_size.unwrap_or((w, h));
            next_rs = Some(render);
            let slot_info = Vp9RefInfo {
                coded_size: (w, h),
                render_size: render,
                color_space: next_cs,
                full_range: next_fr,
            };
            for (slot_idx, slot) in next_refs.iter_mut().enumerate() {
                if (hdr.refresh_frame_flags & (1u8 << slot_idx)) != 0 {
                    *slot = Some(slot_info);
                }
            }
            if hdr.show_frame {
                shown_metas.push(PendingTiming {
                    pts: pkt_pts,
                    duration_ticks: pkt_dur,
                    render_size: Some(render),
                    color_space: next_cs,
                    full_range: next_fr,
                    keyframe: hdr.key_frame,
                    show_existing: false,
                    ordinal,
                    input_offset,
                });
            }
        }
        if !seen_kf {
            return Err(SessionError::Codec(Vp9SessionError::Backend(String::from(
                "VP9 stream must begin with a keyframe",
            ))));
        }

        let raw_pts = pkt_pts.map(|t| t.ticks()).unwrap_or_else(|| {
            let v = self.synthetic_pts;
            self.synthetic_pts += 1;
            v
        });
        let payload = packet
            .take()
            .ok_or(SessionError::Protocol(ProtocolViolation::EmptyPacket))?;
        self.dec
            .push(payload.as_bytes(), Some(raw_pts))
            .map_err(|e| SessionError::Codec(Vp9SessionError::Backend(alloc::format!("{e}"))))?;

        // Commit state updates after `rusty_vp9` accepts the packet.
        self.total_input_bytes = next_input_total;
        self.seen_keyframe = seen_kf;
        self.ref_slots = next_refs;
        self.stream_color_space = next_cs;
        self.stream_full_range = next_fr;
        self.stream_render_size = next_rs;
        self.next_ordinal = ordinal.saturating_add(1);
        for meta in shown_metas {
            self.pending_meta.push_back(meta);
        }
        self.drain_decoder(&cfg, pulse)?;
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
        if let Some(cfg) = self.config.clone() {
            self.dec.flush();
            self.drain_decoder(&cfg, pulse)?;
        }
        self.ended = true;
        Ok(())
    }

    fn reset(&mut self, pulse: Option<&dyn Pulse>) -> Res<()> {
        self.check_pulse(pulse)?;
        self.dec = rusty_vp9::Vp9Decoder::new();
        self.ref_slots = [None; 8];
        self.stream_color_space = None;
        self.stream_full_range = None;
        self.stream_render_size = None;
        self.pending_meta.clear();
        self.ready.clear();
        self.ended = false;
        self.seen_keyframe = false;
        self.next_ordinal = 0;
        self.synthetic_pts = 0;
        self.total_input_bytes = 0;
        Ok(())
    }

    fn stream_info(&self) -> Option<&VideoStreamInfo> {
        self.stream_info.as_ref()
    }
}

fn vp9_possible_layouts(
    cfg: Vp9CodecConfig,
    container_color: Option<Cicp>,
) -> Option<Vec<PlanarLayout>> {
    let depths: &[u8] = match (cfg.profile, cfg.bit_depth) {
        (0 | 1, None | Some(8)) => &[8],
        (2 | 3, None) => &[10, 12],
        (2 | 3, Some(10)) => &[10],
        (2 | 3, Some(12)) => &[12],
        _ => return None,
    };
    let subs: &[ChromaSubsampling] = match (cfg.profile, cfg.subsampling) {
        (0 | 2, None | Some(Vp9Subsampling::Yuv420)) => &[ChromaSubsampling::Yuv420],
        (1 | 3, None) => &[ChromaSubsampling::Yuv422, ChromaSubsampling::Yuv444],
        (1 | 3, Some(Vp9Subsampling::Yuv422)) => &[ChromaSubsampling::Yuv422],
        (1 | 3, Some(Vp9Subsampling::Yuv444)) => &[ChromaSubsampling::Yuv444],
        _ => return None,
    };
    let priv_rgb = cfg.cicp.and_then(|(_, _, m)| (m != 2).then_some(m == 0));
    let cont_rgb = container_color
        .and_then(|c| (c.matrix_coefficients != 2).then_some(c.matrix_coefficients == 0));
    if let (Some(a), Some(b)) = (priv_rgb, cont_rgb)
        && a != b
    {
        return None;
    }
    let definite_rgb = priv_rgb.or(cont_rgb);

    let mut out = Vec::new();
    for &bits in depths {
        let st = if bits == 8 {
            SampleType::U8
        } else {
            SampleType::U16
        };
        let enc = SampleEncoding::new(st, bits, 0).ok()?;
        for &sub in subs {
            let rgb_modes: &[bool] = if sub == ChromaSubsampling::Yuv444 {
                match definite_rgb {
                    Some(true) => &[true],
                    Some(false) => &[false],
                    None => &[false, true],
                }
            } else {
                if definite_rgb == Some(true) {
                    return None;
                }
                &[false]
            };
            for &is_rgb in rgb_modes {
                let roles = if is_rgb {
                    [PlaneRole::Green, PlaneRole::Blue, PlaneRole::Red]
                } else {
                    [PlaneRole::Luma, PlaneRole::Cb, PlaneRole::Cr]
                };
                let layout = PlanarLayout::new(
                    roles
                        .into_iter()
                        .map(|r| PlaneSpec::new(r, PlaneEncoding::Code(enc)))
                        .collect(),
                    sub,
                    ChromaSiting::Unknown,
                )
                .ok()?;
                out.push(layout);
            }
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Shareable [`VideoDecodeFactory`] for VP9 streams backed by [`Vp9VideoDecoder`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Vp9DecodeFactory;

impl Vp9DecodeFactory {
    /// Create a VP9 decode factory.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    fn open_concrete(
        &self,
        plan: &VideoDecodePlan,
        run: &JobResources,
    ) -> Result<Vp9VideoDecoder, MediaError> {
        run.check_stop()?;
        if plan.config.codec != CodecId::Vp9 {
            return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
        }
        let limits = run.limits();
        if limits.max_memory_bytes == Some(0) || limits.max_pixels == Some(0) {
            return Err(MediaError::Invalid(
                "VP9 decoder refused zero memory or pixel limit",
            ));
        }
        let (max_w, max_h) = plan.maximum_coded_extent;
        let bound_w = limits.max_width.map_or(max_w, |l| l.min(max_w));
        let bound_h = limits.max_height.map_or(max_h, |l| l.min(max_h));
        if bound_w == 0 || bound_h == 0 || limits.check_dimensions(bound_w, bound_h).is_err() {
            return Err(MediaError::Invalid(
                "VP9 coded extent exceeds JobResources dimension limits",
            ));
        }
        let mut dec = Vp9VideoDecoder::new(run.storage().clone())
            .with_pulse(run.pulse().clone())
            .with_dimension_limits(Some(bound_w), Some(bound_h), limits.max_pixels)
            .with_max_input_bytes(limits.max_input_bytes);
        if let Some(priv_bytes) = plan.config.private.as_deref()
            && let Some(parsed) = parse_codec_private(priv_bytes)
        {
            dec = dec.with_expected_private(parsed);
        }
        dec.configure(&plan.config, Some(run.pulse()))
            .map_err(MediaError::from)?;
        Ok(dec)
    }
}

impl VideoDecodeFactory for Vp9DecodeFactory {
    fn quote(
        &self,
        config: &CodecConfig,
        need: &VideoDecodeNeeds,
        run: &JobResources,
    ) -> Result<VideoDecodePlan, MediaError> {
        run.check_stop()?;
        if config.codec != CodecId::Vp9 {
            return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
        }
        if run.limits().max_memory_bytes == Some(0) {
            return Err(MediaError::Invalid("VP9 decoder refused zero memory limit"));
        }
        let (w, h) = need.maximum_coded_extent;
        if w == 0 || h == 0 || w > 65536 || h > 65536 {
            return Err(MediaError::Invalid(
                "VP9 coded extent must be within 1..=65536",
            ));
        }
        if run.limits().check_dimensions(w, h).is_err() {
            return Err(MediaError::Invalid(
                "VP9 coded extent exceeds JobResources dimension limits",
            ));
        }
        if need.max_held_pictures < 8 {
            return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
        }
        let parsed_private = match config.private.as_deref() {
            Some(raw) => {
                let parsed = parse_codec_private(raw)
                    .ok_or(MediaError::Unsupported(UnsupportedMedia::Codec))?;
                Some(parsed)
            }
            None => None,
        };
        if !need.accepted_layouts.is_empty() {
            // Every layout producible under the pinned private config (+ container color) must
            // be in `need.accepted_layouts`; otherwise refuse rather than risk a role/layout mismatch.
            let Some(cfg) = parsed_private else {
                return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
            };
            let Some(required) = vp9_possible_layouts(cfg, config.video.and_then(|v| v.color))
            else {
                return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
            };
            if !required
                .iter()
                .all(|req| need.accepted_layouts.contains(req))
            {
                return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
            }
        }
        let mut plan = VideoDecodePlan::new(
            config.clone(),
            need.maximum_coded_extent,
            NativeBound::Unknown,
            8,
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

/// Pure-Rust VP9 video encoder session implementing [`VideoEncoder`],
/// backed by `rusty_vp9::Vp9Encoder` configured in FIFO single-pass mode
/// (`lag = 0`, `two_pass = false`) for deterministic 1:1 PTS/duration mapping.
pub struct Vp9VideoEncoder {
    enc: rusty_vp9::Vp9Encoder,
    width: u32,
    height: u32,
    clock: TimeBase,
    output_cfg: CodecConfig,
    owned_pulse: Option<SharedPulse>,
    ready: VecDeque<Packet>,
    last_pts: Option<i64>,
    ended: bool,
}

impl fmt::Debug for Vp9VideoEncoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vp9VideoEncoder")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

impl Vp9VideoEncoder {
    /// Construct a VP9 encoder session for 8-bit limited-range YUV 4:2:0 frames.
    pub fn new(
        width: u32,
        height: u32,
        qindex: u32,
        speed: u32,
        keyframe_interval: u32,
        clock: TimeBase,
        color: Option<Cicp>,
    ) -> Result<Self, Vp9SessionError> {
        if width == 0 || height == 0 || width > 65536 || height > 65536 {
            return Err(Vp9SessionError::InputMismatch);
        }
        let mut enc = rusty_vp9::Vp9Encoder::default();
        let cfg = rusty_vp9::Vp9EncoderConfig {
            qindex: Some(qindex.clamp(1, 255)),
            speed: Some(speed.min(6)),
            keyframe_interval: (keyframe_interval > 0).then_some(keyframe_interval),
            lag: Some(0),
            two_pass: false,
            ..Default::default()
        };
        enc.configure(&cfg)
            .map_err(|e| Vp9SessionError::Backend(alloc::format!("{e}")))?;
        let mut vinfo = VideoTrackInfo::new(width, height);
        if let Some(c) = color {
            vinfo = vinfo.with_color(c);
        }
        let (p, t, m) = color
            .map(|c| {
                (
                    c.color_primaries,
                    c.transfer_characteristics,
                    c.matrix_coefficients,
                )
            })
            .unwrap_or((2, 2, 2));
        let vpcc = [
            1u8,
            0,
            0,
            0,
            0, // profile 0
            0, // level undefined (VP ISOBMFF binding)
            (8 << 4) | (Vp9Subsampling::Yuv420.vpcc_code() << 1),
            p,
            t,
            m,
            0,
            0,
        ];
        let output_cfg = CodecConfig::new(CodecId::Vp9, clock)
            .with_video(vinfo)
            .with_private(Arc::from(vpcc.as_slice()));
        Ok(Self {
            enc,
            width,
            height,
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

impl VideoEncoder for Vp9VideoEncoder {
    type Error = Vp9SessionError;

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
            return Err(SessionError::Codec(Vp9SessionError::InputMismatch));
        }
        if interp.color.resolved.is_some_and(|c| c.full_range) {
            return Err(SessionError::Codec(Vp9SessionError::InputMismatch));
        }
        let pts = frame
            .timing()
            .pts
            .ok_or(SessionError::Protocol(ProtocolViolation::MissingTimestamp))?;
        if pts.base() != self.clock {
            return Err(SessionError::Protocol(ProtocolViolation::ClockMismatch));
        }
        if self.last_pts.is_some_and(|last| pts.ticks() <= last) {
            return Err(SessionError::Codec(Vp9SessionError::NonMonotonicTimestamp));
        }

        let mapped = frame
            .picture()
            .map()
            .map_err(|_| SessionError::Codec(Vp9SessionError::InputMismatch))?;
        let (Some(AnyPlane::U8(yp)), Some(AnyPlane::U8(up)), Some(AnyPlane::U8(vp))) =
            (mapped.plane(0), mapped.plane(1), mapped.plane(2))
        else {
            return Err(SessionError::Codec(Vp9SessionError::InputMismatch));
        };
        if yp.width() != self.width as usize
            || yp.height() != self.height as usize
            || up.width() != self.width.div_ceil(2) as usize
            || up.height() != self.height.div_ceil(2) as usize
            || vp.width() != up.width()
            || vp.height() != up.height()
        {
            return Err(SessionError::Codec(Vp9SessionError::InputMismatch));
        }

        self.enc
            .push_frame(
                [
                    yp.strided_samples(),
                    up.strided_samples(),
                    vp.strided_samples(),
                ],
                [yp.stride(), up.stride(), vp.stride()],
                self.width,
                self.height,
            )
            .map_err(|e| SessionError::Codec(Vp9SessionError::Backend(alloc::format!("{e}"))))?;

        match self.enc.next_packet() {
            Ok(encoded) => {
                if encoded.keyframe
                    && let Some(hdr) = parse_frame_header(&encoded.data)
                    && let Some(vpcc) = hdr.to_vpcc_v1(0)
                {
                    let cfg = self
                        .output_cfg
                        .clone()
                        .with_private(Arc::from(vpcc.as_slice()));
                    self.output_cfg = cfg;
                }
                let mut pkt = Packet::from_vec(encoded.data)
                    .map_err(|_| SessionError::Protocol(ProtocolViolation::EmptyPacket))?
                    .with_track(0)
                    .with_pts(pts)
                    .with_dts(pts)
                    .with_random_access(if encoded.keyframe {
                        RandomAccess::Yes
                    } else {
                        RandomAccess::No
                    });
                if let Some(d) = frame.timing().duration_ticks {
                    pkt = pkt.with_duration_ticks(d);
                }
                self.ready.push_back(pkt);
            }
            Err(rusty_vp9::Error::Again) => {}
            Err(e) => {
                return Err(SessionError::Codec(Vp9SessionError::Backend(
                    alloc::format!("{e}"),
                )));
            }
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
        self.enc.flush();
        self.ended = true;
        Ok(())
    }

    fn output_config(&self) -> Option<&CodecConfig> {
        Some(&self.output_cfg)
    }
}

/// Shareable [`VideoEncodeFactory`] for VP9 streams backed by [`Vp9VideoEncoder`].
#[derive(Clone, Copy, Debug)]
pub struct Vp9EncodeFactory {
    default_qindex: u32,
    speed: u32,
    keyframe_interval: u32,
}

impl Default for Vp9EncodeFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl Vp9EncodeFactory {
    /// Create a VP9 encode factory (default `qindex = 64`, `speed = 3`, keyframe interval 120).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            default_qindex: 64,
            speed: 3,
            keyframe_interval: 120,
        }
    }

    /// Set default VP9 base quantizer index (`1..=255`).
    #[must_use]
    pub const fn with_qindex(mut self, qindex: u32) -> Self {
        self.default_qindex = qindex;
        self
    }

    /// Set VP9 encoder speed preset (`0` slowest/best .. `6` fastest).
    #[must_use]
    pub const fn with_speed(mut self, speed: u32) -> Self {
        self.speed = speed;
        self
    }

    /// Set maximum keyframe interval in frames (`0` = first frame only).
    #[must_use]
    pub const fn with_keyframe_interval(mut self, interval: u32) -> Self {
        self.keyframe_interval = interval;
        self
    }

    fn resolve_qindex(
        &self,
        interp: &FrameInterpretation,
        fidelity: Option<&Fidelity>,
    ) -> Result<(u32, u32, u32), MediaError> {
        let geom = interp.geometry;
        if geom.coded_width == 0
            || geom.coded_height == 0
            || geom.coded_width > 65536
            || geom.coded_height > 65536
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
            None => self.default_qindex.clamp(1, 255),
            Some(Fidelity::Lossless) => {
                return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
            }
            Some(Fidelity::Lossy(LossyTarget::CodecSpecificQuality(q))) => {
                if !q.is_finite() {
                    return Err(MediaError::Invalid("VP9 quality must be finite"));
                }
                let clamped = q.clamp(0.0, 100.0);
                (((100.0 - clamped) * 2.54 + 1.0) as u32).clamp(1, 255)
            }
            Some(_) => return Err(MediaError::Unsupported(UnsupportedMedia::Codec)),
        };
        Ok((geom.coded_width, geom.coded_height, qindex))
    }

    fn open_concrete(
        &self,
        plan: &VideoEncodePlan,
        run: &JobResources,
    ) -> Result<Vp9VideoEncoder, MediaError> {
        run.check_stop()?;
        if plan.codec != CodecId::Vp9 {
            return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
        }
        let (w, h, qindex) = self.resolve_qindex(&plan.interpretation, plan.fidelity.as_ref())?;
        let color = plan.interpretation.color.resolved;
        let enc = Vp9VideoEncoder::new(
            w,
            h,
            qindex,
            self.speed,
            self.keyframe_interval,
            plan.clock,
            color,
        )
        .map_err(MediaError::backend)?
        .with_pulse(run.pulse().clone());
        Ok(enc)
    }
}

impl VideoEncodeFactory for Vp9EncodeFactory {
    fn quote(
        &self,
        need: &VideoEncodeNeeds,
        run: &JobResources,
    ) -> Result<VideoEncodePlan, MediaError> {
        run.check_stop()?;
        if need.codec != CodecId::Vp9 {
            return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
        }
        if need.max_in_flight_frames == 0 {
            return Err(MediaError::Unsupported(UnsupportedMedia::Codec));
        }
        if run.limits().max_memory_bytes == Some(0) {
            return Err(MediaError::Invalid("VP9 encoder refused zero memory limit"));
        }
        let geom = need.interpretation.geometry;
        if run
            .limits()
            .check_dimensions(geom.coded_width, geom.coded_height)
            .is_err()
        {
            return Err(MediaError::Invalid(
                "VP9 encode dimensions exceed JobResources limits",
            ));
        }
        let _ = self.resolve_qindex(&need.interpretation, need.fidelity.as_ref())?;
        let _ = ThreadingPolicy::Sequential;
        let mut plan = VideoEncodePlan::new(
            CodecId::Vp9,
            Arc::clone(&need.interpretation),
            need.clock,
            NativeBound::Unknown,
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

/// Register [`Vp9DecodeFactory`] and [`Vp9EncodeFactory`] in a [`zencodec::CodecSet`]
/// under `org.imazen.vp9.rusty_vp9`.
pub fn register_vp9_codec_set(
    set: zencodec::CodecSet,
) -> Result<zencodec::CodecSet, zencodec::RegistrationError> {
    use zencodec::{NamedFactory, RegistrationId};
    use zencodec_media::registry::{VideoDecodeRole, VideoEncodeRole};

    let id = RegistrationId::new("org.imazen.vp9.rusty_vp9").expect("valid registration id");
    let set = set.with_role::<VideoDecodeRole>(
        [CodecId::Vp9],
        NamedFactory::new(
            id.clone(),
            env!("CARGO_PKG_VERSION"),
            Arc::new(Vp9DecodeFactory::new()) as Arc<dyn VideoDecodeFactory>,
        ),
    )?;
    set.with_role::<VideoEncodeRole>(
        [CodecId::Vp9],
        NamedFactory::new(
            id,
            env!("CARGO_PKG_VERSION"),
            Arc::new(Vp9EncodeFactory::new()) as Arc<dyn VideoEncodeFactory>,
        ),
    )
}
