//! Real codec adapters for `zencodec-media`'s session traits, built from the
//! qualified candidates in `zenextras/research/media-codecs`.
//!
//! * [`H264Decoder`] — oxideav-h264, fed AVCC→Annex-B converted MP4 packets
//!   plus a one-shot SPS/PPS config packet parsed from `avcC` codec-private.
//! * [`AacDecoder`] — oxideav-aac, fed per-AU ADTS headers synthesized from the
//!   MP4 `esds` AudioSpecificConfig (oxideav-aac has no raw-AU carrier path).
//! * [`Av1EncoderAdapter`] — zenrav1e through `zencodec_media::av1_encode`.
//! * [`OpusEncoderAdapter`] — ruopus, with a windowed-sinc resample to 48 kHz.
//!
//! Everything is bounded: decoders emit one `VideoFrame`/`AudioBlock` per call,
//! encoder adapters keep at most a one-second PCM tail plus encoder-side
//! lookahead queues.

pub mod resample;

use oxideav_core::Decoder as _;
use resample::StreamingResampler;
use zencodec_media::av1_encode::{self, EncodeReceive};
use zencodec_media::color::{ChromaLocation, Subsampling};
use zencodec_media::session::{
    AudioBlock, AudioDecoder, AudioEncoder, Pcm, PlaneBuf, PlaneData, VideoDecoder, VideoEncoder,
    VideoFrame,
};
use zencodec_media::time::{Rounding, TimeBase, Timestamp};
use zencodec_media::track::{AudioInfo, Codec, MediaError, MediaPacket, TrackKind, TrackSpec};
use zenpixels::descriptor::ChannelType;
use zenpixels::sample::SampleEncoding;
use zenpixels::Cicp;

fn core_err(e: oxideav_core::Error) -> MediaError {
    match e {
        oxideav_core::Error::NeedMore | oxideav_core::Error::Eof => {
            MediaError::Contract("codec signalled end-state out of turn")
        }
        other => MediaError::Format(Box::leak(format!("codec: {other}").into_boxed_str())),
    }
}

fn encode_err(e: av1_encode::EncodeError) -> MediaError {
    MediaError::Contract(Box::leak(format!("av1 encoder: {e}").into_boxed_str()))
}

/// Per-plane subsample shift, mirrored from `Subsampling::shifts` (private).
fn sub_shifts(s: Subsampling) -> (usize, usize) {
    match s {
        Subsampling::Yuv420 => (1, 1),
        Subsampling::Yuv422 => (1, 0),
        Subsampling::Yuv444 | Subsampling::Monochrome => (0, 0),
        _ => (0, 0),
    }
}

// ---------------------------------------------------------------------------
// H.264 MP4 → decoded frames (oxideav-h264)
// ---------------------------------------------------------------------------

/// Parsed `avcC` (AVCDecoderConfigurationRecord) — just what the decoder needs.
struct AvcC {
    nal_length_size: usize,
    /// SPS and PPS NALs, Annex-B framed, sent once ahead of the first AU.
    config_annexb: Vec<u8>,
}

fn parse_avcc(avcc: &[u8]) -> Result<AvcC, MediaError> {
    if avcc.len() < 7 {
        return Err(MediaError::Format("avcC truncated"));
    }
    let nal_length_size = (avcc[4] & 0x03) as usize + 1;
    let num_sps = (avcc[5] & 0x1F) as usize;
    let mut pos = 6usize;
    let mut cfg = Vec::new();
    let nals = |count: usize, pos: &mut usize, out: &mut Vec<u8>| -> Result<(), MediaError> {
        for _ in 0..count {
            if *pos + 2 > avcc.len() {
                return Err(MediaError::Format("avcC NAL length truncated"));
            }
            let n = u16::from_be_bytes([avcc[*pos], avcc[*pos + 1]]) as usize;
            *pos += 2;
            if *pos + n > avcc.len() {
                return Err(MediaError::Format("avcC NAL body truncated"));
            }
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&avcc[*pos..*pos + n]);
            *pos += n;
        }
        Ok(())
    };
    nals(num_sps, &mut pos, &mut cfg)?;
    if pos >= avcc.len() {
        return Err(MediaError::Format("avcC missing PPS count"));
    }
    let num_pps = avcc[pos] as usize;
    pos += 1;
    nals(num_pps, &mut pos, &mut cfg)?;
    if num_sps == 0 || num_pps == 0 {
        return Err(MediaError::Format("avcC without SPS/PPS"));
    }
    Ok(AvcC {
        nal_length_size,
        config_annexb: cfg,
    })
}

/// Convert one AVCC (length-prefixed) access unit into Annex B.
fn avcc_to_annexb(nal_len: usize, au: &[u8], out: &mut Vec<u8>) -> Result<(), MediaError> {
    let mut pos = 0usize;
    while pos + nal_len <= au.len() {
        let mut n = 0usize;
        for &b in &au[pos..pos + nal_len] {
            n = (n << 8) | b as usize;
        }
        pos += nal_len;
        if pos + n > au.len() {
            return Err(MediaError::Format("AVCC NAL overruns packet"));
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&au[pos..pos + n]);
        pos += n;
    }
    if pos != au.len() {
        return Err(MediaError::Format("AVCC packet tail underruns NAL length"));
    }
    Ok(())
}

/// H.264 decoder adapter. Packets arrive in decode order (MP4 stts order).
/// oxideav emits frames in presentation order but synthesizes output
/// timestamps from the DPB depth rather than propagating packet pts, so
/// submitted pts values are kept in a sorted multiset and the smallest is
/// popped per emitted frame — valid while output is strictly
/// presentation-ordered, which the conformance matrix verified.
pub struct H264Decoder {
    inner: oxideav_h264::h264_decoder::H264CodecDecoder,
    tb: TimeBase,
    avcc: AvcC,
    sent_config: bool,
    ended: bool,
    /// Multiset of submitted packet pts values not yet attributed to a frame.
    pending_pts: std::collections::BTreeMap<i64, u32>,
    /// MP4 edit-list media delay (ticks) — presentation starts at pts 0 after
    /// subtracting it, matching ffmpeg's elst application.
    edit_delay: i64,
}

impl H264Decoder {
    pub fn new(spec: &TrackSpec) -> Result<Self, MediaError> {
        if spec.kind != TrackKind::Video || spec.codec != Codec::H264 {
            return Err(MediaError::Contract(
                "H264Decoder needs an H.264 video track",
            ));
        }
        let avcc = spec
            .codec_private
            .as_deref()
            .ok_or(MediaError::Format("H.264 track without avcC"))
            .and_then(parse_avcc)?;
        Ok(Self {
            inner: oxideav_h264::h264_decoder::H264CodecDecoder::new(oxideav_core::CodecId::new(
                "h264",
            )),
            tb: spec.time_base,
            avcc,
            sent_config: false,
            ended: false,
            pending_pts: std::collections::BTreeMap::new(),
            edit_delay: spec.edit_delay_ticks.unwrap_or(0),
        })
    }

    fn send(&mut self, bytes: Vec<u8>, pts: i64, dts: Option<i64>) -> Result<(), MediaError> {
        let mut p = oxideav_core::Packet::new(
            0,
            oxideav_core::TimeBase::new(self.tb.numerator() as i64, self.tb.denominator() as i64),
            bytes,
        );
        p.pts = Some(pts);
        p.dts = dts;
        self.inner.send_packet(&p).map_err(core_err)
    }
}

impl VideoDecoder for H264Decoder {
    fn push_packet(&mut self, packet: &MediaPacket) -> Result<(), MediaError> {
        if self.ended {
            return Err(MediaError::Contract("packet pushed after end_input"));
        }
        let pts = packet.pts.ticks();
        let dts = packet.dts.map(|d| d.ticks());
        if !self.sent_config {
            self.send(self.avcc.config_annexb.clone(), pts, dts)?;
            self.sent_config = true;
        }
        let mut annexb = Vec::with_capacity(packet.data.len() + 8);
        avcc_to_annexb(self.avcc.nal_length_size, &packet.data, &mut annexb)?;
        self.send(annexb, pts, dts)?;
        *self.pending_pts.entry(pts).or_insert(0) += 1;
        Ok(())
    }

    fn next_frame(&mut self) -> Result<Option<VideoFrame>, MediaError> {
        loop {
            match self.inner.receive_frame() {
                Ok(oxideav_core::Frame::Video(vf)) => {
                    return self.convert(&vf).map(Some);
                }
                Ok(_) => continue,
                Err(oxideav_core::Error::NeedMore) | Err(oxideav_core::Error::Eof) => {
                    return Ok(None);
                }
                Err(e) => return Err(core_err(e)),
            }
        }
    }

    fn end_input(&mut self) -> Result<(), MediaError> {
        self.ended = true;
        self.inner.flush().map_err(core_err)
    }

    fn reset(&mut self) -> Result<(), MediaError> {
        self.ended = false;
        self.sent_config = false;
        self.pending_pts.clear();
        oxideav_core::Decoder::reset(&mut self.inner).map_err(core_err)
    }
}

impl H264Decoder {
    /// Smallest outstanding submitted pts = the presentation pts of the next
    /// emitted frame (oxideav emits strictly in presentation order).
    fn pop_pending_pts(&mut self) -> Option<i64> {
        let (&k, _) = self.pending_pts.first_key_value()?;
        let v = self.pending_pts.get_mut(&k).unwrap();
        *v -= 1;
        if *v == 0 {
            self.pending_pts.remove(&k);
        }
        Some(k)
    }

    fn convert(&mut self, vf: &oxideav_core::VideoFrame) -> Result<VideoFrame, MediaError> {
        let sps = self.inner.active_sps().cloned();
        let (cl, ct, cr, cb, bits) = sps
            .as_ref()
            .map(|s| {
                let c = s
                    .frame_cropping
                    .clone()
                    .unwrap_or(oxideav_h264::sps::FrameCropping {
                        left: 0,
                        right: 0,
                        top: 0,
                        bottom: 0,
                    });
                (
                    c.left as usize,
                    c.top as usize,
                    c.right as usize,
                    c.bottom as usize,
                    8 + s.bit_depth_luma_minus8 as u8,
                )
            })
            .unwrap_or((0, 0, 0, 0, 8));
        let bps = if bits > 8 { 2usize } else { 1usize };
        let subsampling = match sps.as_ref().map(|s| s.chroma_format_idc) {
            Some(0) | None => Subsampling::Monochrome,
            Some(1) => Subsampling::Yuv420,
            Some(2) => Subsampling::Yuv422,
            Some(3) => Subsampling::Yuv444,
            Some(_) => return Err(MediaError::Unsupported("H.264 chroma_format_idc > 3")),
        };
        let (sx, sy) = sub_shifts(subsampling);

        let color = sps
            .as_ref()
            .and_then(|s| s.vui.as_ref())
            .and_then(|v| v.video_signal_type.as_ref())
            .map(|vst| {
                let (cp, tc, mc) = vst
                    .colour_description
                    .as_ref()
                    .map(|d| {
                        (
                            d.colour_primaries,
                            d.transfer_characteristics,
                            d.matrix_coefficients,
                        )
                    })
                    .unwrap_or((2, 2, 2));
                Cicp::new(cp, tc, mc, vst.video_full_range_flag)
            })
            .unwrap_or(Cicp::new(2, 2, 2, false));

        let location = sps
            .as_ref()
            .and_then(|s| s.vui.as_ref())
            .and_then(|v| v.chroma_loc_info.as_ref())
            .map(|c| match c.chroma_sample_loc_type_top_field {
                0 => ChromaLocation::Left,
                1 => ChromaLocation::Center,
                2 => ChromaLocation::TopLeft,
                _ => ChromaLocation::Unknown,
            })
            .unwrap_or(ChromaLocation::Left);

        let encoding = SampleEncoding::new(
            if bps == 1 {
                ChannelType::U8
            } else {
                ChannelType::U16
            },
            bits,
            0,
        )
        .map_err(|_| MediaError::Unsupported("H.264 sample encoding beyond U16"))?;

        // §7.4.2.1.1 CropUnitX/Y for 4:2:0 frame pictures = (2,2): crop offsets
        // are in 2-luma-pixel units; chroma planes divide by the subsample shift
        // again. Established empirically on oxideav output by the harness.
        let crop_plane = |p: &oxideav_core::VideoPlane, chroma: bool| -> PlaneBuf {
            let (ux, uy) = if chroma { (sx, sy) } else { (0, 0) };
            let unit_x = 2usize >> ux;
            let unit_y = 2usize >> uy;
            let rows = p.data.len().checked_div(p.stride).unwrap_or(0);
            let left = cl * unit_x;
            let top = ct * unit_y;
            let right = cr * unit_x;
            let bottom = cb * unit_y;
            let cw = p.stride / bps;
            let vw = cw.saturating_sub(left + right);
            let vh = rows.saturating_sub(top + bottom);
            match bits {
                8 => {
                    let mut v = Vec::with_capacity(vw * vh);
                    for r in 0..vh {
                        let base = (top + r) * p.stride + left;
                        v.extend_from_slice(&p.data[base..base + vw]);
                    }
                    PlaneBuf {
                        data: PlaneData::U8(v),
                        width: vw,
                        height: vh,
                        stride_samples: vw,
                    }
                }
                _ => {
                    let mut v = Vec::with_capacity(vw * vh);
                    for r in 0..vh {
                        let base = (top + r) * p.stride + left * 2;
                        for c in p.data[base..base + vw * 2].as_chunks::<2>().0 {
                            v.push(u16::from_le_bytes(*c));
                        }
                    }
                    PlaneBuf {
                        data: PlaneData::U16(v),
                        width: vw,
                        height: vh,
                        stride_samples: vw,
                    }
                }
            }
        };

        let planes = vf.image_planes();
        if planes.is_empty() {
            return Err(MediaError::Format("decoder emitted frame without planes"));
        }
        let y = crop_plane(&planes[0], false);
        let chroma = if subsampling == Subsampling::Monochrome || planes.len() < 3 {
            None
        } else {
            Some([crop_plane(&planes[1], true), crop_plane(&planes[2], true)])
        };
        Ok(VideoFrame {
            y,
            chroma,
            subsampling,
            location,
            color,
            encoding,
            pts: Timestamp::new(
                self.pop_pending_pts()
                    .unwrap_or_else(|| vf.pts.unwrap_or(0))
                    - self.edit_delay,
                self.tb,
            ),
            duration_ticks: None,
        })
    }
}

// ---------------------------------------------------------------------------
// AAC MP4 → decoded PCM (oxideav-aac, ADTS-wrapped)
// ---------------------------------------------------------------------------

/// AAC decoder adapter. MP4 carries raw access units plus an AudioSpecificConfig
/// in `codec_private`; oxideav-aac's decoder speaks ADTS or LOAS only, so each
/// AU is wrapped in a synthesized 7-byte ADTS header built from the ASC.
pub struct AacDecoder {
    inner: Box<dyn oxideav_core::Decoder>,
    asc: oxideav_aac::asc::AudioSpecificConfig,
    ended: bool,
    /// MP4 edit-list media delay in source samples — encoder priming. Audible
    /// media starts at `edit_delay`; `declared_end` (mdhd duration) caps the
    /// media span so container-declared tail padding is also trimmed. Emitted
    /// blocks run on the presentation timeline (media pos − edit_delay).
    edit_delay: i64,
    declared_end: Option<i64>,
    /// Absolute media index of the next decoded frame's first sample.
    media_pos: i64,
}

impl AacDecoder {
    pub fn new(spec: &TrackSpec) -> Result<Self, MediaError> {
        if spec.kind != TrackKind::Audio || spec.codec != Codec::Aac {
            return Err(MediaError::Contract("AacDecoder needs an AAC audio track"));
        }
        let asc = spec
            .codec_private
            .as_deref()
            .ok_or(MediaError::Format("AAC track without AudioSpecificConfig"))
            .and_then(|d| {
                oxideav_aac::asc::AudioSpecificConfig::parse(d)
                    .map(|(asc, _)| asc)
                    .map_err(|_| MediaError::Format("unparseable AudioSpecificConfig"))
            })?;
        let audio = spec
            .audio
            .ok_or(MediaError::Contract("audio track without AudioInfo"))?;
        let mut params = oxideav_core::CodecParameters::audio(oxideav_core::CodecId::new("aac"));
        params.sample_rate = Some(audio.sample_rate);
        params.channels = Some(audio.channels.max(1));
        let inner = oxideav_aac::codec_decoder::make_decoder(&params)
            .map_err(|_| MediaError::Unsupported("AAC decoder rejected track parameters"))?;
        Ok(Self {
            inner,
            asc,
            ended: false,
            edit_delay: spec.edit_delay_ticks.unwrap_or(0),
            declared_end: spec.declared_duration.map(|d| d as i64),
            media_pos: 0,
        })
    }

    fn src_rate(&self) -> u32 {
        self.asc.sample_rate.max(1)
    }

    /// Wrap one raw access unit in an ADTS fixed header derived from the ASC.
    fn adts_wrap(&self, au: &[u8]) -> Result<Vec<u8>, MediaError> {
        if self.asc.aot == 0 || self.asc.aot > 4 {
            return Err(MediaError::Unsupported(
                "AAC object type is not ADTS-expressible",
            ));
        }
        if self.asc.channel_configuration > 7 {
            return Err(MediaError::Unsupported(
                "AAC channel config beyond ADTS field",
            ));
        }
        let h = oxideav_aac::adts::AdtsHeader {
            mpeg_version_mpeg2: false,
            protection_absent: true,
            profile: self.asc.aot - 1,
            sampling_frequency_index: self.asc.sampling_frequency_index,
            channel_configuration: self.asc.channel_configuration,
            aac_frame_length: (au.len() + 7) as u16,
            adts_buffer_fullness: 0x7FF,
            number_of_raw_data_blocks_in_frame: 1,
        };
        let head = h
            .write()
            .map_err(|_| MediaError::Format("synthesized ADTS header invalid"))?;
        let mut v = Vec::with_capacity(au.len() + 7);
        v.extend_from_slice(&head);
        v.extend_from_slice(au);
        Ok(v)
    }

    fn src_pts_tb(&self) -> TimeBase {
        TimeBase::new(1, self.src_rate()).unwrap_or_else(|_| TimeBase::new(1, 1).unwrap())
    }
}

impl AudioDecoder for AacDecoder {
    fn push_packet(&mut self, packet: &MediaPacket) -> Result<(), MediaError> {
        if self.ended {
            return Err(MediaError::Contract("packet pushed after end_input"));
        }
        let adts = self.adts_wrap(&packet.data)?;
        let rate = self.src_rate();
        let mut p = oxideav_core::Packet::new(0, oxideav_core::TimeBase::new(1, rate as i64), adts);
        // MediaPacket pts is in track ticks; rescale onto the decoder's
        // 1/rate clock so the propagated frame pts stays exact.
        let pts = packet
            .pts
            .rescale(self.src_pts_tb(), Rounding::Nearest)
            .map_err(|_| MediaError::Format("AAC pts out of range"))?;
        p.pts = Some(pts.ticks());
        p.dts = packet.dts.map(|d| d.ticks());
        self.inner.send_packet(&p).map_err(core_err)
    }

    fn next_block(&mut self) -> Result<Option<AudioBlock>, MediaError> {
        loop {
            match self.inner.receive_frame() {
                Ok(oxideav_core::Frame::Audio(f)) => {
                    let plane = f
                        .data
                        .first()
                        .ok_or(MediaError::Format("AAC frame no plane"))?;
                    let ch = (plane.len() / 2)
                        .checked_div(f.samples.max(1) as usize)
                        .unwrap_or(0)
                        .max(1) as u16;
                    // Emit samples in [edit_delay, declared_end) on the media
                    // timeline — priming at the head, container padding at the
                    // tail. Emitted pts is the presentation timeline.
                    let total = f.samples as usize;
                    let m_start = self.media_pos;
                    self.media_pos += total as i64;
                    let lo = m_start.max(self.edit_delay);
                    let hi = (m_start + total as i64).min(self.declared_end.unwrap_or(i64::MAX));
                    let emit = (hi - lo).max(0) as usize;
                    if emit == 0 {
                        continue;
                    }
                    let skip = (lo - m_start) as usize;
                    let pcm: Vec<i16> = plane[skip * ch as usize * 2..][..emit * ch as usize * 2]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| i16::from_le_bytes(*c))
                        .collect();
                    return Ok(Some(AudioBlock {
                        pts: Timestamp::new(lo - self.edit_delay, self.src_pts_tb()),
                        frames: emit,
                        channels: ch,
                        sample_rate: self.src_rate(),
                        pcm: Pcm::S16(pcm),
                    }));
                }
                Ok(_) => continue,
                Err(oxideav_core::Error::NeedMore) | Err(oxideav_core::Error::Eof) => {
                    return Ok(None);
                }
                Err(e) => return Err(core_err(e)),
            }
        }
    }

    fn end_input(&mut self) -> Result<(), MediaError> {
        self.ended = true;
        self.inner.flush().map_err(core_err)
    }

    fn reset(&mut self) -> Result<(), MediaError> {
        self.ended = false;
        self.media_pos = 0;
        self.inner.reset().map_err(core_err)
    }
}

// ---------------------------------------------------------------------------
// Frames → AV1 packets (zenrav1e via zencodec_media::av1_encode)
// ---------------------------------------------------------------------------

/// AV1 encoder adapter. Constructed lazily on the first frame so the actual
/// decoded geometry/depth/chroma configure the encoder, not the container's
/// (possibly stale) declarations.
pub struct Av1EncoderAdapter {
    inner: Option<av1_encode::Av1Encoder>,
    tb: TimeBase,
    out_track: u32,
    ordinal: u64,
    speed: u8,
    quantizer: usize,
    queue: usize,
    ended: bool,
}

impl Av1EncoderAdapter {
    /// `speed` is the zenrav1e speed preset (0..=10); `quantizer` is the base
    /// quantizer (0..=255). `queue` bounds in-flight frames.
    pub fn new(
        spec: &TrackSpec,
        out_track: u32,
        speed: u8,
        quantizer: usize,
        queue: usize,
    ) -> Result<Self, MediaError> {
        if spec.kind != TrackKind::Video {
            return Err(MediaError::Contract(
                "Av1EncoderAdapter needs a video track",
            ));
        }
        Ok(Self {
            inner: None,
            tb: spec.time_base,
            out_track,
            ordinal: 0,
            speed,
            quantizer,
            queue,
            ended: false,
        })
    }
}

impl VideoEncoder for Av1EncoderAdapter {
    fn push_frame(&mut self, frame: VideoFrame) -> Result<(), MediaError> {
        if self.ended {
            return Err(MediaError::Contract("frame pushed after end_input"));
        }
        if self.inner.is_none() {
            let mut cfg = zenrav1e::EncoderConfig::with_speed_preset(self.speed);
            cfg.width = frame.y.width;
            cfg.height = frame.y.height;
            cfg.bit_depth = frame.encoding.code_bits() as usize;
            cfg.chroma_sampling = match frame.subsampling {
                Subsampling::Monochrome => zenrav1e::prelude::ChromaSampling::Cs400,
                Subsampling::Yuv420 => zenrav1e::prelude::ChromaSampling::Cs420,
                Subsampling::Yuv422 => zenrav1e::prelude::ChromaSampling::Cs422,
                Subsampling::Yuv444 => zenrav1e::prelude::ChromaSampling::Cs444,
                _ => return Err(MediaError::Unsupported("decoder chroma subsampling")),
            };
            cfg.chroma_sample_position = match frame.location {
                ChromaLocation::Left => zenrav1e::prelude::ChromaSamplePosition::Vertical,
                ChromaLocation::TopLeft => zenrav1e::prelude::ChromaSamplePosition::Colocated,
                _ => zenrav1e::prelude::ChromaSamplePosition::Unknown,
            };
            cfg.pixel_range = if frame.color.full_range {
                zenrav1e::prelude::PixelRange::Full
            } else {
                zenrav1e::prelude::PixelRange::Limited
            };
            let (cp, tc, mc) = (
                frame.color.color_primaries,
                frame.color.transfer_characteristics,
                frame.color.matrix_coefficients,
            );
            if cp != 2 || tc != 2 || mc != 2 {
                cfg.color_description = Some(zenrav1e::color::ColorDescription {
                    color_primaries: num_traits::FromPrimitive::from_u8(cp).unwrap_or_default(),
                    transfer_characteristics: num_traits::FromPrimitive::from_u8(tc)
                        .unwrap_or_default(),
                    matrix_coefficients: num_traits::FromPrimitive::from_u8(mc).unwrap_or_default(),
                });
            }
            cfg.quantizer = self.quantizer;
            cfg.still_picture = false;
            cfg.enable_timing_info = false;
            self.inner =
                Some(av1_encode::Av1Encoder::new(cfg, self.tb, self.queue, 1).map_err(encode_err)?);
        }
        let ts = frame.pts;
        let enc = self.inner.as_mut().unwrap();
        enc.submit(frame.yuv_view()?, ts).map_err(encode_err)?;
        Ok(())
    }

    fn next_packet(&mut self) -> Result<Option<MediaPacket>, MediaError> {
        let Some(enc) = self.inner.as_mut() else {
            return Ok(None);
        };
        match enc.receive() {
            Ok(EncodeReceive::Packet(p)) => {
                let pkt = MediaPacket {
                    track: self.out_track,
                    ordinal: self.ordinal,
                    config_epoch: 0,
                    pts: p.timestamp(),
                    dts: None,
                    duration_ticks: None,
                    keyframe: p.keyframe(),
                    discard_padding_ns: None,
                    data: p.into_data(),
                };
                self.ordinal += 1;
                Ok(Some(pkt))
            }
            Ok(_) => Ok(None),
            Err(e) => Err(encode_err(e)),
        }
    }

    fn end_input(&mut self) -> Result<(), MediaError> {
        self.ended = true;
        if let Some(enc) = self.inner.as_mut() {
            enc.end_input().map_err(encode_err)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PCM → Opus packets (ruopus, resampled to 48 kHz)
// ---------------------------------------------------------------------------

/// Opus output runs at 48 kHz on a 1/48000 clock — ticks are samples.
const OPUS_RATE: u32 = 48_000;
/// Opus frame size used for every emitted packet (20 ms).
const OPUS_FRAME: usize = 960;
/// Encoder pending-PCM bound: one second per channel.
const PENDING_LIMIT: usize = OPUS_RATE as usize;

pub struct OpusEncoderAdapter {
    enc: ruopus::OpusEncoder,
    resampler: Option<StreamingResampler>,
    channels: usize,
    /// Interleaved 48 kHz f32 awaiting a full 960-sample frame.
    pending: Vec<f32>,
    /// Absolute output-sample index of `pending[0]` (per channel).
    pending_base: u64,
    ended: bool,
    flushed: bool,
    /// pts of the first produced output sample, in 1/48000 ticks.
    pts_offset: i64,
    seen_first: bool,
    out_track: u32,
    ordinal: u64,
}

impl OpusEncoderAdapter {
    pub fn new(spec: &TrackSpec, out_track: u32) -> Result<Self, MediaError> {
        if spec.kind != TrackKind::Audio {
            return Err(MediaError::Contract(
                "OpusEncoderAdapter needs an audio track",
            ));
        }
        let audio = spec
            .audio
            .ok_or(MediaError::Contract("audio track without AudioInfo"))?;
        if !(1..=2).contains(&audio.channels) {
            return Err(MediaError::Unsupported("ruopus encodes mono/stereo only"));
        }
        let resampler = StreamingResampler::needed(audio.sample_rate, OPUS_RATE).then(|| {
            StreamingResampler::new(audio.sample_rate, OPUS_RATE, audio.channels as usize)
        });
        let mut enc = ruopus::OpusEncoder::new(audio.channels as usize);
        // Without a target bitrate ruopus fills `max_bytes` every packet
        // (CELT byte-budget semantics); 64 kbps VBR is the sensible default.
        enc.set_bitrate(Some(64_000));
        Ok(Self {
            enc,
            resampler,
            channels: audio.channels as usize,
            pending: Vec::new(),
            pending_base: 0,
            ended: false,
            flushed: false,
            pts_offset: 0,
            seen_first: false,
            out_track,
            ordinal: 0,
        })
    }

    /// OpusHead bytes for `TrackSpec.codec_private` — ruopus has ~zero encoder
    /// lookahead (impulse probe), so `pre_skip` is honestly 0.
    pub fn codec_private(channels: u16) -> Vec<u8> {
        let mut v = Vec::with_capacity(19);
        v.extend_from_slice(b"OpusHead");
        v.push(1);
        v.push(channels as u8);
        v.extend_from_slice(&0u16.to_le_bytes());
        v.extend_from_slice(&OPUS_RATE.to_le_bytes());
        v.extend_from_slice(&0i16.to_le_bytes());
        v.push(0);
        v
    }
}

impl AudioEncoder for OpusEncoderAdapter {
    fn push_block(&mut self, block: AudioBlock) -> Result<(), MediaError> {
        if self.ended {
            return Err(MediaError::Contract("block pushed after end_input"));
        }
        if block.channels as usize != self.channels {
            return Err(MediaError::Format("channel count changed mid-stream"));
        }
        let f32s: Vec<f32> = match &block.pcm {
            Pcm::S16(v) => v.iter().map(|&s| s as f32 / 32768.0).collect(),
            Pcm::F32(v) => v.clone(),
        };
        if !self.seen_first {
            let scaled = block
                .pts
                .rescale(
                    TimeBase::new(1, OPUS_RATE).map_err(|_| MediaError::Format("opus tb"))?,
                    Rounding::Nearest,
                )
                .map_err(|_| MediaError::Format("audio pts out of range"))?;
            self.pts_offset = scaled.ticks();
            self.seen_first = true;
        }
        if let Some(rs) = self.resampler.as_mut() {
            rs.push(&f32s, &mut self.pending);
        } else {
            self.pending.extend_from_slice(&f32s);
        }
        if self.pending.len() / self.channels > PENDING_LIMIT {
            return Err(MediaError::Limit("opus encoder PCM queue exceeded 1s"));
        }
        Ok(())
    }

    fn next_packet(&mut self) -> Result<Option<MediaPacket>, MediaError> {
        let avail = self.pending.len() / self.channels;
        if avail == 0 || (!self.flushed && avail < OPUS_FRAME) {
            return Ok(None);
        }
        let take = avail.min(OPUS_FRAME);
        let mut frame = Vec::with_capacity(OPUS_FRAME * self.channels);
        frame.extend_from_slice(&self.pending[..take * self.channels]);
        let pad = OPUS_FRAME - take;
        if pad > 0 {
            frame.resize(OPUS_FRAME * self.channels, 0.0);
        }
        let data = self
            .enc
            .encode_auto(&frame, 1275)
            .map_err(|_| MediaError::Contract("opus encode failed"))?;
        self.pending.drain(..take * self.channels);
        let pts_ticks = self.pts_offset + self.pending_base as i64;
        self.pending_base += take as u64;
        let pkt = MediaPacket {
            track: self.out_track,
            ordinal: self.ordinal,
            config_epoch: 0,
            data,
            pts: Timestamp::new(
                pts_ticks,
                TimeBase::new(1, OPUS_RATE).map_err(|_| MediaError::Format("opus tb"))?,
            ),
            dts: None,
            duration_ticks: Some(OPUS_FRAME as u32),
            keyframe: true,
            discard_padding_ns: (pad > 0)
                .then(|| ((pad as i64) * 1_000_000_000 + OPUS_RATE as i64 - 1) / OPUS_RATE as i64),
        };
        self.ordinal += 1;
        Ok(Some(pkt))
    }

    fn end_input(&mut self) -> Result<(), MediaError> {
        self.ended = true;
        if let Some(rs) = self.resampler.as_mut() {
            rs.finish(&mut self.pending);
        }
        self.flushed = true;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Output specs for the first route
// ---------------------------------------------------------------------------

/// Build output `TrackSpec`s for the H.264/AAC MP4 → AV1/Opus WebM route, in
/// the same order as the input specs (WebM track numbers follow this order).
pub fn route_output_specs(src: &[TrackSpec]) -> Result<Vec<TrackSpec>, MediaError> {
    let mut out = Vec::new();
    for t in src {
        match t.kind {
            TrackKind::Video => {
                let vi = t
                    .video
                    .ok_or(MediaError::Contract("video track without VideoInfo"))?;
                out.push(TrackSpec {
                    index: out.len() as u32,
                    kind: TrackKind::Video,
                    codec: Codec::Av1,
                    codec_private: None,
                    time_base: t.time_base,
                    video: Some(vi),
                    audio: None,
                    codec_delay_ns: 0,
                    seek_preroll_ns: 0,
                    config_epoch: 0,
                    declared_packets: None,
                    declared_duration: None,
                    edit_delay_ticks: None,
                });
            }
            TrackKind::Audio => {
                let ai = t
                    .audio
                    .ok_or(MediaError::Contract("audio track without AudioInfo"))?;
                out.push(TrackSpec {
                    index: out.len() as u32,
                    kind: TrackKind::Audio,
                    codec: Codec::Opus,
                    codec_private: Some(OpusEncoderAdapter::codec_private(ai.channels)),
                    time_base: TimeBase::new(1, OPUS_RATE)
                        .map_err(|_| MediaError::Format("opus tb"))?,
                    video: None,
                    audio: Some(AudioInfo {
                        sample_rate: OPUS_RATE,
                        channels: ai.channels,
                    }),
                    codec_delay_ns: 0,
                    seek_preroll_ns: 80_000_000,
                    config_epoch: 0,
                    declared_packets: None,
                    declared_duration: None,
                    edit_delay_ticks: None,
                });
            }
            TrackKind::Other(_) => {}
        }
    }
    Ok(out)
}
