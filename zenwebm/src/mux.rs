//! The pull-output WebM writer.

use alloc::boxed::Box;
use alloc::vec::Vec;

use zencodec::JobResources;
use zencodec::encode::OutputBuffer;
use zencodec::source::NativeBound;
use zencodec::time::{FrameDuration, Rounding, TimeBase, Timestamp};
use zencodec_media::container::{
    BoundedMuxer, DynMuxer, MuxFinish, MuxHint, MuxOffer, MuxTiming, Muxer, PacketWrite,
    erase_bounded_muxer,
};
use zencodec_media::packet::{Packet, RandomAccess};
use zencodec_media::registry::{
    ContainerId, MediaError, MuxFactory, MuxNeeds, MuxPlan, TimingPolicy,
};
use zencodec_media::session::{ErasedError, ProtocolViolation, SessionError, UnsupportedMedia};
use zencodec_media::track::{CodecId, TrackInfo, TrackKind};

use crate::ebml::{self, id};
use crate::{WebmError, codec_id_for};

type Res<T> = Result<T, SessionError<WebmError>>;

fn err<T>(e: WebmError) -> Res<T> {
    Err(SessionError::Codec(e))
}

const NS_PER_SECOND: u64 = 1_000_000_000;
/// The WebM convention: millisecond ticks.
const DEFAULT_SCALE_NS: u32 = 1_000_000;
const DEFAULT_CLUSTER_SECONDS: u64 = 5;
const DEFAULT_CLUSTER_BYTES: usize = 8 << 20;

/// What the muxer had to do to fit the container. Nothing is quantized
/// without being counted here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct MuxReport {
    /// Packets written.
    pub packets: u64,
    /// Clusters written.
    pub clusters: u64,
    /// Timestamps, durations and padding rounded to the segment's ticks
    /// (or to nanoseconds, for padding and delays).
    pub quantized: u64,
    /// The largest rounding error, in nanoseconds.
    pub max_quantization_ns: u64,
    /// Packets whose random access was [`RandomAccess::Unknown`]: written
    /// without the keyframe flag, since Matroska has no "unknown".
    pub unknown_random_access: u64,
}

#[derive(Debug)]
struct MuxTrack {
    time_base: TimeBase,
    epoch: u32,
    video: bool,
    /// AV1: blocks drop a leading temporal delimiter (the Matroska AV1
    /// mapping says it SHOULD NOT be in a Block; readers prepend one).
    av1: bool,
    last_ticks: Option<i64>,
}

/// `b` without a leading AV1 temporal delimiter OBU (one with a size
/// field; a delimiter without one cannot be told apart from the rest).
fn without_temporal_delimiter(b: &[u8]) -> &[u8] {
    const OBU_TEMPORAL_DELIMITER: u8 = 2;
    let Some(&header) = b.first() else { return b };
    // A delimiter has no payload: forbidden bit clear, type 2, a size
    // field, size 0.
    if header & 0x80 != 0 || (header >> 3) & 0xF != OBU_TEMPORAL_DELIMITER || header & 0x2 == 0 {
        return b;
    }
    let mut i = 1 + usize::from(header & 0x4 != 0);
    let (mut size, mut shift) = (0usize, 0u32);
    loop {
        let Some(&byte) = b.get(i) else { return b };
        i += 1;
        size |= usize::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 56 {
            return b;
        }
    }
    if size == 0 { &b[i..] } else { b }
}

#[derive(Debug)]
struct OpenCluster {
    base: i64,
    body: Vec<u8>,
    blocks: u32,
}

/// WebM writer, live profile: an unknown-size Segment and one cluster at a
/// time. A cluster's bytes are pending once it closes, at the next video
/// random-access point or when it reaches the configured length or size.
/// No patches, no index.
#[derive(Debug)]
pub struct WebmMuxer {
    out: OutputBuffer,
    begun: bool,
    tracks: Vec<MuxTrack>,
    scale_ns: u32,
    segment: TimeBase,
    scale_override: Option<u32>,
    exact: bool,
    cluster_duration: FrameDuration,
    cluster_ticks: i64,
    max_cluster_bytes: usize,
    cluster: Option<OpenCluster>,
    report: MuxReport,
}

impl Default for WebmMuxer {
    fn default() -> Self {
        Self::new()
    }
}

impl WebmMuxer {
    /// A writer with clusters of up to 5 seconds or 8 MiB.
    pub fn new() -> Self {
        let segment = TimeBase::new(DEFAULT_SCALE_NS, NS_PER_SECOND as u32).expect("nonzero");
        Self {
            out: OutputBuffer::new(),
            begun: false,
            tracks: Vec::new(),
            scale_ns: DEFAULT_SCALE_NS,
            segment,
            scale_override: None,
            exact: false,
            cluster_duration: FrameDuration::new(DEFAULT_CLUSTER_SECONDS, 1).expect("nonzero"),
            cluster_ticks: 0,
            max_cluster_bytes: DEFAULT_CLUSTER_BYTES,
            cluster: None,
            report: MuxReport::default(),
        }
    }

    /// The segment tick length in nanoseconds (`TimestampScale`). Without
    /// it, the muxer picks the longest tick every track's clock divides
    /// exactly, or 1 ms when one does not divide whole nanoseconds.
    pub fn with_timestamp_scale_ns(mut self, ns: u32) -> Self {
        self.scale_override = Some(ns);
        self
    }

    /// Refuse a timestamp, duration or padding the segment's ticks cannot
    /// represent ([`WebmError::InexactTimestamp`]) instead of rounding it
    /// to the nearest tick and counting it in the report.
    pub fn with_exact_timestamps(mut self) -> Self {
        self.exact = true;
        self
    }

    /// Close a cluster once it spans this long (default 5 s).
    pub fn with_cluster_duration(mut self, d: FrameDuration) -> Self {
        self.cluster_duration = d;
        self
    }

    /// Close a cluster once its blocks reach this many bytes (default
    /// 8 MiB): the most a cluster holds before it can be sent.
    pub fn with_max_cluster_bytes(mut self, bytes: usize) -> Self {
        self.max_cluster_bytes = bytes;
        self
    }

    /// What was quantized and how much was written so far.
    pub fn report(&self) -> MuxReport {
        self.report
    }

    fn note_rounding(&mut self, error_ns: i128) {
        if error_ns != 0 {
            self.report.quantized += 1;
            let e = u64::try_from(error_ns.unsigned_abs()).unwrap_or(u64::MAX);
            self.report.max_quantization_ns = self.report.max_quantization_ns.max(e);
        }
    }

    /// `t` in segment ticks: exact, or rounded and reported.
    fn ticks(&mut self, t: Timestamp) -> Res<i64> {
        let overflow = |_| SessionError::Codec(WebmError::TimestampOverflow);
        let floor = t.rescale(self.segment, Rounding::Floor).map_err(overflow)?;
        let ceil = t.rescale(self.segment, Rounding::Ceil).map_err(overflow)?;
        if floor == ceil {
            return Ok(floor.ticks());
        }
        if self.exact {
            return err(WebmError::InexactTimestamp);
        }
        let near = t
            .rescale(self.segment, Rounding::Nearest)
            .map_err(overflow)?;
        // Error in nanoseconds: ticks x scale against the exact instant.
        let exact_ns = i128::from(t.ticks()) * i128::from(t.base().numerator()) * 1_000_000_000
            / i128::from(t.base().denominator());
        self.note_rounding(i128::from(near.ticks()) * i128::from(self.scale_ns) - exact_ns);
        Ok(near.ticks())
    }

    /// A duration in whole nanoseconds: exact, or rounded and reported.
    fn nanos(&mut self, d: FrameDuration) -> Res<u64> {
        let num = u128::from(d.numerator()) * u128::from(NS_PER_SECOND);
        let den = u128::from(d.denominator());
        let floor = num / den;
        let rem = num % den;
        if rem != 0 {
            if self.exact {
                return err(WebmError::InexactTimestamp);
            }
            let up = rem * 2 >= den;
            self.note_rounding(if up { (den - rem) as i128 } else { rem as i128 });
        }
        let ns = floor + u128::from(rem * 2 >= den && rem != 0);
        u64::try_from(ns).map_err(|_| SessionError::Codec(WebmError::TimestampOverflow))
    }

    fn choose_scale(&self, tracks: &[TrackInfo]) -> Res<u32> {
        if let Some(s) = self.scale_override {
            return if s == 0 {
                err(WebmError::BadTimestampScale)
            } else {
                Ok(s)
            };
        }
        // Each clock's tick in nanoseconds, when whole; the scale is their
        // greatest common divisor.
        let mut scale: Option<u64> = None;
        for t in tracks {
            let tb = t.time_base();
            let ns = u64::from(tb.numerator()) * NS_PER_SECOND;
            if !ns.is_multiple_of(u64::from(tb.denominator())) {
                return Ok(DEFAULT_SCALE_NS);
            }
            let tick = ns / u64::from(tb.denominator());
            scale = Some(match scale {
                None => tick,
                Some(s) => gcd(s, tick),
            });
        }
        Ok(scale
            .and_then(|s| u32::try_from(s).ok())
            .unwrap_or(DEFAULT_SCALE_NS))
    }

    fn close_cluster(&mut self) {
        let Some(c) = self.cluster.take() else {
            return;
        };
        let mut ts = Vec::with_capacity(10);
        ebml::put_uint(&mut ts, id::TIMESTAMP, c.base as u64);
        let mut head = Vec::with_capacity(12);
        ebml::put_id(&mut head, id::CLUSTER);
        ebml::put_size(&mut head, (ts.len() + c.body.len()) as u64);
        self.out.push(&head);
        self.out.push(&ts);
        self.out.push(&c.body);
        self.report.clusters += 1;
    }

    /// Read-only duration conversion (does not mutate rounding report).
    fn nanos_ro(&self, d: FrameDuration) -> Res<u64> {
        let num = u128::from(d.numerator()) * u128::from(NS_PER_SECOND);
        let den = u128::from(d.denominator());
        let floor = num / den;
        let rem = num % den;
        if rem != 0 && self.exact {
            return err(WebmError::InexactTimestamp);
        }
        let ns = floor + u128::from(rem * 2 >= den && rem != 0);
        u64::try_from(ns).map_err(|_| SessionError::Codec(WebmError::TimestampOverflow))
    }

    /// Zero-allocation exact header validation and byte-length calculation
    /// matching [`Muxer::begin`] byte-for-byte.
    fn measure_header_bytes(&self, tracks: &[TrackInfo], exact: bool) -> Res<usize> {
        if exact {
            return Err(SessionError::Unsupported(UnsupportedMedia::ExactTiming));
        }
        if self.begun {
            return Err(SessionError::Protocol(ProtocolViolation::BeginTwice));
        }
        if tracks.is_empty() {
            return err(WebmError::NoTracks);
        }
        if tracks.len() > 126 {
            return err(WebmError::TooManyTracks);
        }
        let scale_ns = self.choose_scale(tracks)?;
        let _ = TimeBase::new(scale_ns, NS_PER_SECOND as u32)
            .map_err(|_| SessionError::Codec(WebmError::BadTimestampScale))?;
        let ovf = || SessionError::Unsupported(UnsupportedMedia::Container);
        let add = |a: usize, b: usize| a.checked_add(b).ok_or_else(ovf);
        let elem = |id: u32, len: usize| ebml::elem_len(id, len).ok_or_else(ovf);
        let mut entries_len = 0usize;
        let mut webm = true;
        for (i, t) in tracks.iter().enumerate() {
            let (codec, is_webm) =
                codec_id_for(t.codec()).ok_or(SessionError::Codec(WebmError::UnsupportedCodec))?;
            webm &= is_webm;
            let kind = match t.kind() {
                TrackKind::Video => 1u64,
                TrackKind::Audio => 2u64,
                _ => return err(WebmError::UnsupportedTrackKind),
            };
            if t.edit_delay_ticks().is_some_and(|d| d != 0) || !t.timing().edits.is_empty() {
                return err(WebmError::EditList);
            }
            let number = i as u64 + 1;
            let mut e_len = 0usize;
            e_len = add(e_len, ebml::uint_len(id::TRACK_NUMBER, number))?;
            e_len = add(e_len, ebml::uint_len(id::TRACK_UID, number))?;
            e_len = add(e_len, ebml::uint_len(id::TRACK_TYPE, kind))?;
            e_len = add(e_len, ebml::uint_len(id::FLAG_LACING, 0))?;
            e_len = add(e_len, elem(id::CODEC_ID, codec.len())?)?;
            if let Some(p) = t.codec_private() {
                if *t.codec() == CodecId::Vp9 {
                    let priv_len = normalize_webm_vp9_private_len(p)
                        .ok_or(SessionError::Unsupported(UnsupportedMedia::Codec))?;
                    if priv_len > 0 {
                        e_len = add(e_len, elem(id::CODEC_PRIVATE, priv_len)?)?;
                    }
                } else {
                    e_len = add(e_len, elem(id::CODEC_PRIVATE, p.len())?)?;
                }
            }
            if let Some(d) = t.codec_delay() {
                let ns = self.nanos_ro(d)?;
                e_len = add(e_len, ebml::uint_len(id::CODEC_DELAY, ns))?;
            }
            if let Some(d) = t.seek_preroll() {
                let ns = self.nanos_ro(d)?;
                e_len = add(e_len, ebml::uint_len(id::SEEK_PRE_ROLL, ns))?;
            }
            if let Some(v) = t.video_info() {
                let mut ve_len = add(
                    ebml::uint_len(id::PIXEL_WIDTH, u64::from(v.coded_width)),
                    ebml::uint_len(id::PIXEL_HEIGHT, u64::from(v.coded_height)),
                )?;
                if let Some((w, h)) = v.display {
                    ve_len = add(ve_len, ebml::uint_len(id::DISPLAY_WIDTH, u64::from(w)))?;
                    ve_len = add(ve_len, ebml::uint_len(id::DISPLAY_HEIGHT, u64::from(h)))?;
                }
                if let Some(c) = v.color {
                    let mut ce_len =
                        ebml::uint_len(id::MATRIX_COEFFICIENTS, u64::from(c.matrix_coefficients));
                    ce_len = add(
                        ce_len,
                        ebml::uint_len(id::RANGE, if c.full_range { 2 } else { 1 }),
                    )?;
                    ce_len = add(
                        ce_len,
                        ebml::uint_len(
                            id::TRANSFER_CHARACTERISTICS,
                            u64::from(c.transfer_characteristics),
                        ),
                    )?;
                    ce_len = add(
                        ce_len,
                        ebml::uint_len(id::PRIMARIES, u64::from(c.color_primaries)),
                    )?;
                    ve_len = add(ve_len, elem(id::COLOUR, ce_len)?)?;
                }
                e_len = add(e_len, elem(id::VIDEO, ve_len)?)?;
            }
            if let Some(a) = t.audio_info() {
                let ae_len = add(
                    ebml::float_len(id::SAMPLING_FREQUENCY),
                    ebml::uint_len(id::CHANNELS, u64::from(a.channels.count)),
                )?;
                e_len = add(e_len, elem(id::AUDIO, ae_len)?)?;
            }
            entries_len = add(entries_len, elem(id::TRACK_ENTRY, e_len)?)?;
        }
        let _ = self.nanos_ro(self.cluster_duration)?;
        let h_len = ebml::uint_len(id::EBML_VERSION, 1)
            + ebml::uint_len(id::EBML_READ_VERSION, 1)
            + ebml::uint_len(id::EBML_MAX_ID_LENGTH, 4)
            + ebml::uint_len(id::EBML_MAX_SIZE_LENGTH, 8)
            + elem(id::DOC_TYPE, if webm { 4 } else { 8 })?
            + ebml::uint_len(id::DOC_TYPE_VERSION, 4)
            + ebml::uint_len(id::DOC_TYPE_READ_VERSION, 2);
        let info_len = ebml::uint_len(id::TIMESTAMP_SCALE, u64::from(scale_ns))
            + elem(id::MUXING_APP, b"zenwebm".len())?
            + elem(id::WRITING_APP, b"zenwebm".len())?;
        let mut total = elem(id::EBML, h_len)?;
        total = add(total, ebml::id_len(id::SEGMENT))?;
        total = add(total, 8)?; // unknown size
        total = add(total, elem(id::INFO, info_len)?)?;
        total = add(total, elem(id::TRACKS, entries_len)?)?;
        Ok(total)
    }

    /// Validate `packet` against muxer state without mutating any cluster,
    /// rounding report, or output buffer.
    fn validate_packet(&self, packet: &Packet) -> Res<()> {
        if !self.begun {
            return Err(SessionError::Protocol(ProtocolViolation::NoBegin));
        }
        let index = packet.track() as usize;
        let Some(track) = self.tracks.get(index) else {
            return err(WebmError::NoSuchTrack);
        };
        if packet.config_epoch() != track.epoch {
            return err(WebmError::EpochChange);
        }
        let pts = packet
            .pts()
            .ok_or(SessionError::Protocol(ProtocolViolation::MissingTimestamp))?;
        if pts.base() != track.time_base {
            return Err(SessionError::Protocol(ProtocolViolation::ClockMismatch));
        }
        let has_padding = match packet.discard().map(|d| (d.start, d.end)) {
            Some((Some(_), Some(_))) => return err(WebmError::DiscardBothEnds),
            Some((Some(s), None)) => {
                let ns = self.nanos_ro(s)?;
                let _ = i64::try_from(ns)
                    .map_err(|_| SessionError::Codec(WebmError::TimestampOverflow))?;
                true
            }
            Some((None, Some(e))) => {
                let ns = self.nanos_ro(e)?;
                let _ = i64::try_from(ns)
                    .map_err(|_| SessionError::Codec(WebmError::TimestampOverflow))?;
                true
            }
            _ => false,
        };
        let overflow = |_| SessionError::Codec(WebmError::TimestampOverflow);
        let ticks_ro = |t: Timestamp| -> Res<i64> {
            let floor = t.rescale(self.segment, Rounding::Floor).map_err(overflow)?;
            let ceil = t.rescale(self.segment, Rounding::Ceil).map_err(overflow)?;
            if floor != ceil && self.exact {
                return err(WebmError::InexactTimestamp);
            }
            let near = if floor == ceil {
                floor
            } else {
                t.rescale(self.segment, Rounding::Nearest)
                    .map_err(overflow)?
            };
            Ok(near.ticks())
        };
        let ticks = ticks_ro(pts)?;
        if let Some(d) = packet.duration_ticks() {
            let d =
                i64::try_from(d).map_err(|_| SessionError::Codec(WebmError::TimestampOverflow))?;
            let end_ticks = pts
                .ticks()
                .checked_add(d)
                .ok_or(SessionError::Codec(WebmError::TimestampOverflow))?;
            let end = ticks_ro(Timestamp::new(end_ticks, pts.base()))?;
            let delta = i128::from(end) - i128::from(ticks);
            let _ = u64::try_from(delta.max(0))
                .map_err(|_| SessionError::Codec(WebmError::TimestampOverflow))?;
        }
        let needs_block_group =
            packet.duration_ticks().is_some() || has_padding || !packet.additions().is_empty();
        if needs_block_group
            && packet.random_access() != RandomAccess::Yes
            && let Some(prev) = track.last_ticks
        {
            let diff = i128::from(prev) - i128::from(ticks);
            let _ = i64::try_from(diff.min(-1))
                .map_err(|_| SessionError::Codec(WebmError::TimestampOverflow))?;
        }
        if packet.bytes().is_empty() {
            return Err(SessionError::Protocol(ProtocolViolation::EmptyPacket));
        }
        if let Some(c) = &self.cluster {
            let rel = i128::from(ticks) - i128::from(c.base);
            if rel < i128::from(i16::MIN) {
                return err(WebmError::OutOfOrder);
            }
            let est_block = BoundedWebmMuxer::estimate_block_bytes(packet);
            let will_roll = rel > i128::from(i16::MAX)
                || rel >= i128::from(self.cluster_ticks)
                || c.body.len() >= self.max_cluster_bytes
                || (c.blocks > 0
                    && c.body.len().saturating_add(est_block) > self.max_cluster_bytes)
                || (track.video && packet.random_access() == RandomAccess::Yes && c.blocks > 0);
            if will_roll && ticks < i64::from(i16::MIN) {
                return err(WebmError::NegativeTimestamp);
            }
        } else if ticks < i64::from(i16::MIN) {
            return err(WebmError::NegativeTimestamp);
        }
        Ok(())
    }

    fn track_entry(&mut self, i: usize, t: &TrackInfo) -> Res<(Vec<u8>, bool)> {
        let (codec, webm) =
            codec_id_for(t.codec()).ok_or(SessionError::Codec(WebmError::UnsupportedCodec))?;
        let kind = match t.kind() {
            TrackKind::Video => 1,
            TrackKind::Audio => 2,
            _ => return err(WebmError::UnsupportedTrackKind),
        };
        if t.edit_delay_ticks().is_some_and(|d| d != 0) || !t.timing().edits.is_empty() {
            return err(WebmError::EditList);
        }
        let mut e = Vec::new();
        let number = i as u64 + 1;
        ebml::put_uint(&mut e, id::TRACK_NUMBER, number);
        ebml::put_uint(&mut e, id::TRACK_UID, number);
        ebml::put_uint(&mut e, id::TRACK_TYPE, kind);
        ebml::put_uint(&mut e, id::FLAG_LACING, 0);
        ebml::put(&mut e, id::CODEC_ID, codec.as_bytes());
        if let Some(p) = t.codec_private() {
            if *t.codec() == CodecId::Vp9 {
                let webm_priv = normalize_webm_vp9_private(p)
                    .ok_or(SessionError::Unsupported(UnsupportedMedia::Codec))?;
                if !webm_priv.is_empty() {
                    ebml::put(&mut e, id::CODEC_PRIVATE, &webm_priv);
                }
            } else {
                ebml::put(&mut e, id::CODEC_PRIVATE, p);
            }
        }
        if let Some(d) = t.codec_delay() {
            let ns = self.nanos(d)?;
            ebml::put_uint(&mut e, id::CODEC_DELAY, ns);
        }
        if let Some(d) = t.seek_preroll() {
            let ns = self.nanos(d)?;
            ebml::put_uint(&mut e, id::SEEK_PRE_ROLL, ns);
        }
        if let Some(v) = t.video_info() {
            let mut ve = Vec::new();
            ebml::put_uint(&mut ve, id::PIXEL_WIDTH, u64::from(v.coded_width));
            ebml::put_uint(&mut ve, id::PIXEL_HEIGHT, u64::from(v.coded_height));
            if let Some((w, h)) = v.display {
                ebml::put_uint(&mut ve, id::DISPLAY_WIDTH, u64::from(w));
                ebml::put_uint(&mut ve, id::DISPLAY_HEIGHT, u64::from(h));
            }
            if let Some(c) = v.color {
                let mut ce = Vec::new();
                ebml::put_uint(
                    &mut ce,
                    id::MATRIX_COEFFICIENTS,
                    u64::from(c.matrix_coefficients),
                );
                ebml::put_uint(&mut ce, id::RANGE, if c.full_range { 2 } else { 1 });
                ebml::put_uint(
                    &mut ce,
                    id::TRANSFER_CHARACTERISTICS,
                    u64::from(c.transfer_characteristics),
                );
                ebml::put_uint(&mut ce, id::PRIMARIES, u64::from(c.color_primaries));
                ebml::put(&mut ve, id::COLOUR, &ce);
            }
            ebml::put(&mut e, id::VIDEO, &ve);
        }
        if let Some(a) = t.audio_info() {
            let mut ae = Vec::new();
            ebml::put_float(&mut ae, id::SAMPLING_FREQUENCY, f64::from(a.sample_rate));
            ebml::put_uint(&mut ae, id::CHANNELS, u64::from(a.channels.count));
            ebml::put(&mut e, id::AUDIO, &ae);
        }
        Ok((e, webm))
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl Muxer for WebmMuxer {
    type Error = WebmError;

    fn begin(&mut self, tracks: &[TrackInfo], _hint: MuxHint) -> Res<MuxTiming> {
        if self.begun {
            return Err(SessionError::Protocol(ProtocolViolation::BeginTwice));
        }
        if tracks.is_empty() {
            return err(WebmError::NoTracks);
        }
        if tracks.len() > 126 {
            return err(WebmError::TooManyTracks);
        }
        let scale_ns = self.choose_scale(tracks)?;
        let segment = TimeBase::new(scale_ns, NS_PER_SECOND as u32)
            .map_err(|_| SessionError::Codec(WebmError::BadTimestampScale))?;
        self.scale_ns = scale_ns;
        self.segment = segment;
        let mut entries = Vec::new();
        let mut webm = true;
        for (i, t) in tracks.iter().enumerate() {
            let (e, w) = self.track_entry(i, t)?;
            webm &= w;
            ebml::put(&mut entries, id::TRACK_ENTRY, &e);
        }
        // The cluster bound in segment ticks, at most what a block's 16-bit
        // offset reaches.
        let cluster = self.nanos(self.cluster_duration)? / u64::from(scale_ns);
        self.cluster_ticks = i64::try_from(cluster)
            .unwrap_or(i64::MAX)
            .clamp(1, i64::from(i16::MAX));

        let mut head = Vec::new();
        let mut h = Vec::new();
        ebml::put_uint(&mut h, id::EBML_VERSION, 1);
        ebml::put_uint(&mut h, id::EBML_READ_VERSION, 1);
        ebml::put_uint(&mut h, id::EBML_MAX_ID_LENGTH, 4);
        ebml::put_uint(&mut h, id::EBML_MAX_SIZE_LENGTH, 8);
        ebml::put(
            &mut h,
            id::DOC_TYPE,
            if webm { b"webm" } else { b"matroska" },
        );
        ebml::put_uint(&mut h, id::DOC_TYPE_VERSION, 4);
        ebml::put_uint(&mut h, id::DOC_TYPE_READ_VERSION, 2);
        ebml::put(&mut head, id::EBML, &h);
        ebml::put_id(&mut head, id::SEGMENT);
        ebml::put_unknown_size(&mut head);
        let mut info = Vec::new();
        ebml::put_uint(&mut info, id::TIMESTAMP_SCALE, u64::from(scale_ns));
        ebml::put(&mut info, id::MUXING_APP, b"zenwebm");
        ebml::put(&mut info, id::WRITING_APP, b"zenwebm");
        ebml::put(&mut head, id::INFO, &info);
        ebml::put(&mut head, id::TRACKS, &entries);
        self.out.push(&head);

        self.tracks = tracks
            .iter()
            .map(|t| MuxTrack {
                time_base: t.time_base(),
                epoch: t.config_epoch(),
                video: t.kind() == TrackKind::Video,
                av1: *t.codec() == CodecId::Av1,
                last_ticks: None,
            })
            .collect();
        self.begun = true;
        Ok(MuxTiming::streaming())
    }

    fn write_packet(&mut self, mut packet: Packet, pulse: Option<&dyn zencodec::Pulse>) -> Res<()> {
        if let Some(pulse) = pulse {
            pulse.check()?;
        }
        if !self.begun {
            return Err(SessionError::Protocol(ProtocolViolation::NoBegin));
        }
        let index = packet.track() as usize;
        let Some(track) = self.tracks.get(index) else {
            return err(WebmError::NoSuchTrack);
        };
        if packet.config_epoch() != track.epoch {
            return err(WebmError::EpochChange);
        }
        let pts = packet
            .pts()
            .ok_or(SessionError::Protocol(ProtocolViolation::MissingTimestamp))?;
        if pts.base() != track.time_base {
            return Err(SessionError::Protocol(ProtocolViolation::ClockMismatch));
        }
        let video = track.video;
        let av1 = track.av1;
        let prev = track.last_ticks;
        let discard = packet.discard();
        let padding = match discard.map(|d| (d.start, d.end)) {
            Some((Some(_), Some(_))) => return err(WebmError::DiscardBothEnds),
            Some((Some(s), None)) => Some(
                -i64::try_from(self.nanos(s)?)
                    .map_err(|_| SessionError::Codec(WebmError::TimestampOverflow))?,
            ),
            Some((None, Some(e))) => Some(
                i64::try_from(self.nanos(e)?)
                    .map_err(|_| SessionError::Codec(WebmError::TimestampOverflow))?,
            ),
            _ => None,
        };
        let ticks = self.ticks(pts)?;
        let duration = match packet.duration_ticks() {
            Some(d) => {
                let d = i64::try_from(d)
                    .map_err(|_| SessionError::Codec(WebmError::TimestampOverflow))?;
                let end = self.ticks(Timestamp::new(
                    pts.ticks()
                        .checked_add(d)
                        .ok_or(SessionError::Codec(WebmError::TimestampOverflow))?,
                    pts.base(),
                ))?;
                let delta = i128::from(end) - i128::from(ticks);
                let dur = u64::try_from(delta.max(0))
                    .map_err(|_| SessionError::Codec(WebmError::TimestampOverflow))?;
                Some(dur)
            }
            None => None,
        };
        let random_access = packet.random_access();
        let needs_block_group =
            duration.is_some() || padding.is_some() || !packet.additions().is_empty();
        let reference_block = if needs_block_group && random_access != RandomAccess::Yes {
            let back = match prev {
                Some(p) => {
                    let diff = i128::from(p) - i128::from(ticks);
                    i64::try_from(diff.min(-1))
                        .map_err(|_| SessionError::Codec(WebmError::TimestampOverflow))?
                }
                None => -1,
            };
            Some(back)
        } else {
            None
        };
        let est_block = BoundedWebmMuxer::estimate_block_bytes(&packet);
        if est_block > self.max_cluster_bytes {
            return Err(SessionError::Unsupported(UnsupportedMedia::Container));
        }
        let payload = packet
            .take()
            .ok_or(SessionError::Protocol(ProtocolViolation::EmptyPacket))?;

        // Roll the cluster: at a video random-access point, at the length
        // or size bound (including the incoming block's framing), or when the offset leaves 16 bits.
        let roll = match &self.cluster {
            None => true,
            Some(c) => {
                let rel = i128::from(ticks) - i128::from(c.base);
                if rel < i128::from(i16::MIN) {
                    return err(WebmError::OutOfOrder);
                }
                rel > i128::from(i16::MAX)
                    || rel >= i128::from(self.cluster_ticks)
                    || c.body.len() >= self.max_cluster_bytes
                    || (c.blocks > 0
                        && c.body.len().saturating_add(est_block) > self.max_cluster_bytes)
                    || (video && random_access == RandomAccess::Yes && c.blocks > 0)
            }
        };
        if roll {
            self.close_cluster();
            if ticks < i64::from(i16::MIN) {
                return err(WebmError::NegativeTimestamp);
            }
            // Cluster timestamps are unsigned; earlier packets reach back
            // through the signed block offset.
            self.cluster = Some(OpenCluster {
                base: ticks.max(0),
                body: Vec::new(),
                blocks: 0,
            });
        }
        let c = self.cluster.as_mut().expect("opened above");
        let rel = (ticks - c.base) as i16;
        let body = if av1 {
            without_temporal_delimiter(payload.as_bytes())
        } else {
            payload.as_bytes()
        };
        let mut block = Vec::with_capacity(body.len() + 4);
        block.push(0x80 | (index as u8 + 1));
        block.extend_from_slice(&rel.to_be_bytes());
        let additions = packet.additions();
        if !needs_block_group {
            block.push(if random_access == RandomAccess::Yes {
                0x80
            } else {
                0
            });
            block.extend_from_slice(body);
            ebml::put(&mut c.body, id::SIMPLE_BLOCK, &block);
        } else {
            block.push(0);
            block.extend_from_slice(body);
            let mut g = Vec::with_capacity(block.len() + 32);
            ebml::put(&mut g, id::BLOCK, &block);
            if let Some(d) = duration {
                ebml::put_uint(&mut g, id::BLOCK_DURATION, d);
            }
            if let Some(back) = reference_block {
                // A Block with no ReferenceBlock is a keyframe; point at the
                // track's previous block.
                ebml::put_int(&mut g, id::REFERENCE_BLOCK, back);
            }
            if let Some(p) = padding {
                ebml::put_int(&mut g, id::DISCARD_PADDING, p);
            }
            if !additions.is_empty() {
                let mut adds = Vec::new();
                for a in additions {
                    let mut more = Vec::new();
                    ebml::put_uint(&mut more, id::BLOCK_ADD_ID, u64::from(a.id()));
                    ebml::put(&mut more, id::BLOCK_ADDITIONAL, a.payload().as_bytes());
                    ebml::put(&mut adds, id::BLOCK_MORE, &more);
                }
                ebml::put(&mut g, id::BLOCK_ADDITIONS, &adds);
            }
            ebml::put(&mut c.body, id::BLOCK_GROUP, &g);
        }
        c.blocks += 1;
        if random_access == RandomAccess::Unknown {
            self.report.unknown_random_access += 1;
        }
        self.report.packets += 1;
        self.tracks[index].last_ticks = Some(ticks);
        Ok(())
    }

    fn pending_output(&self) -> &[u8] {
        self.out.pending()
    }

    fn consume_output(&mut self, n: usize) {
        self.out.consume(n);
    }

    fn take_output(&mut self, into: &mut Vec<u8>) {
        self.out.take_into(into);
    }

    fn finish(mut self, pulse: Option<&dyn zencodec::Pulse>) -> Res<MuxFinish> {
        if let Some(pulse) = pulse {
            pulse.check()?;
        }
        if !self.begun {
            return Err(SessionError::Protocol(ProtocolViolation::NoBegin));
        }
        self.close_cluster();
        Ok(MuxFinish::new(self.out.into_remaining(), Vec::new()))
    }
}

const fn is_valid_vp9_level(level: u8) -> bool {
    matches!(
        level,
        0 | 10 | 11 | 20 | 21 | 30 | 31 | 40 | 41 | 50 | 51 | 52 | 60 | 61 | 62
    )
}

const fn is_valid_vp9_profile_combo(
    profile: u8,
    bit_depth: Option<u8>,
    chroma: Option<u8>,
    is_rgb: bool,
) -> bool {
    if profile > 3 {
        return false;
    }
    if is_rgb && (profile == 0 || profile == 2 || !matches!(chroma, Some(3))) {
        return false;
    }
    if let Some(bd) = bit_depth {
        let depth_ok = if profile <= 1 {
            bd == 8
        } else {
            matches!(bd, 10 | 12)
        };
        if !depth_ok {
            return false;
        }
    }
    if let Some(c) = chroma {
        let chroma_ok = if profile == 0 || profile == 2 {
            c <= 1
        } else {
            matches!(c, 2 | 3)
        };
        if !chroma_ok {
            return false;
        }
    }
    true
}

/// Convert ISOBMFF v1 `vpcC` (12 bytes starting with `[1, 0, 0, 0]` and 0 init bytes)
/// or validate WebM VP9 CodecPrivate feature-TLV `(id, len, value)` entries.
fn normalize_webm_vp9_private(raw: &[u8]) -> Option<Vec<u8>> {
    // 1. Canonical 12-byte ISOBMFF v1 `vpcC` -> WebM VP9 feature TLV.
    if raw.len() == 12 && raw[0] == 1 && raw[1..4] == [0, 0, 0] && raw[10..12] == [0, 0] {
        let profile = raw[4];
        let level = raw[5];
        let bit_depth = raw[6] >> 4;
        let chroma = (raw[6] >> 1) & 0x07;
        let is_rgb = raw[9] == 0;
        if !is_valid_vp9_level(level)
            || !is_valid_vp9_profile_combo(profile, Some(bit_depth), Some(chroma), is_rgb)
        {
            return None;
        }
        let mut out = Vec::with_capacity(12);
        // Feature ID 1: Profile (1 byte)
        out.extend_from_slice(&[1, 1, profile]);
        // Feature ID 2: Level (1 byte, omit if 0 / unknown)
        if level != 0 {
            out.extend_from_slice(&[2, 1, level]);
        }
        // Feature ID 3: Bit Depth (1 byte)
        out.extend_from_slice(&[3, 1, bit_depth]);
        // Feature ID 4: Chroma Subsampling (1 byte, preserving 0 vs 1 siting)
        out.extend_from_slice(&[4, 1, chroma]);
        return Some(out);
    }
    // 2. Existing WebM VP9 feature-TLV sequence: validate every `(id, len, val)` entry
    //    and enforce full VP9 profile/level/depth/chroma table consistency.
    let mut pos = 0usize;
    let mut profile = None;
    let mut bit_depth = None;
    let mut chroma = None;
    while pos < raw.len() {
        if pos + 2 > raw.len() {
            return None;
        }
        let id = raw[pos];
        let len = usize::from(raw[pos + 1]);
        pos += 2;
        if len != 1 || pos + len > raw.len() {
            return None;
        }
        let val = raw[pos];
        pos += 1;
        match id {
            1 if val <= 3 => profile = Some(val),
            2 if val != 0 && is_valid_vp9_level(val) => {}
            3 if matches!(val, 8 | 10 | 12) => bit_depth = Some(val),
            4 if val <= 3 => chroma = Some(val),
            _ => return None,
        }
    }
    let p = profile?;
    if !is_valid_vp9_profile_combo(p, bit_depth, chroma, false) {
        return None;
    }
    Some(raw.to_vec())
}

/// Zero-allocation exact serialized byte length of [`normalize_webm_vp9_private`].
fn normalize_webm_vp9_private_len(raw: &[u8]) -> Option<usize> {
    if raw.len() == 12 && raw[0] == 1 && raw[1..4] == [0, 0, 0] && raw[10..12] == [0, 0] {
        let profile = raw[4];
        let level = raw[5];
        let bit_depth = raw[6] >> 4;
        let chroma = (raw[6] >> 1) & 0x07;
        let is_rgb = raw[9] == 0;
        if !is_valid_vp9_level(level)
            || !is_valid_vp9_profile_combo(profile, Some(bit_depth), Some(chroma), is_rgb)
        {
            return None;
        }
        return Some(if level != 0 { 12 } else { 9 });
    }
    let mut pos = 0usize;
    let mut profile = None;
    let mut bit_depth = None;
    let mut chroma = None;
    while pos < raw.len() {
        if pos + 2 > raw.len() {
            return None;
        }
        let id = raw[pos];
        let len = usize::from(raw[pos + 1]);
        pos += 2;
        if len != 1 || pos + len > raw.len() {
            return None;
        }
        let val = raw[pos];
        pos += 1;
        match id {
            1 if val <= 3 => profile = Some(val),
            2 if val != 0 && is_valid_vp9_level(val) => {}
            3 if matches!(val, 8 | 10 | 12) => bit_depth = Some(val),
            4 if val <= 3 => chroma = Some(val),
            _ => return None,
        }
    }
    let p = profile?;
    if !is_valid_vp9_profile_combo(p, bit_depth, chroma, false) {
        return None;
    }
    Some(raw.len())
}

/// [`WebmMuxer`] wrapped with output-credit backpressure implementing
/// [`DynMuxer`] and [`BoundedMuxer`].
#[derive(Debug)]
pub struct BoundedWebmMuxer {
    inner: WebmMuxer,
    offer: MuxOffer,
    max_pending: usize,
}

impl BoundedWebmMuxer {
    /// Wrap `inner` with an admitted [`MuxOffer`] and pending-byte cap.
    #[must_use]
    pub fn new(inner: WebmMuxer, offer: MuxOffer, max_pending: usize) -> Self {
        let cap = max_pending.max(1);
        let cluster_cap = cap.saturating_sub(24).max(1);
        let eff_cluster_cap = inner.max_cluster_bytes.min(cluster_cap);
        let inner = inner.with_max_cluster_bytes(eff_cluster_cap);
        Self {
            inner,
            offer,
            max_pending: cap,
        }
    }

    fn preflight_begin(
        &self,
        tracks: &[TrackInfo],
        _hint: MuxHint,
        exact: bool,
    ) -> Result<(), SessionError<ErasedError>> {
        // Zero-allocation exact header validation & sizing before allocating buffers.
        let header_len = self
            .inner
            .measure_header_bytes(tracks, exact)
            .map_err(erase_webm_err)?;
        if header_len > self.max_pending {
            return Err(SessionError::Unsupported(UnsupportedMedia::Container));
        }
        Ok(())
    }

    fn estimate_block_bytes(pkt: &Packet) -> usize {
        let payload_len = pkt.bytes().len();
        let has_padding = matches!(
            pkt.discard().map(|d| (d.start, d.end)),
            Some((Some(_), None)) | Some((None, Some(_)))
        );
        if pkt.duration_ticks().is_none() && !has_padding && pkt.additions().is_empty() {
            // SimpleBlock: 1B ID + <=8B size VINT + 4B track/rel-ts/flags = <=13B overhead (16B bound).
            return payload_len.saturating_add(16);
        }
        let additions_len: usize = pkt.additions().iter().fold(0usize, |acc, a| {
            acc.saturating_add(a.payload().as_bytes().len())
                .saturating_add(32)
        });
        let add_wrap = if pkt.additions().is_empty() { 0 } else { 16 };
        // Covers BlockGroup header, Block header, track/rel-ts/flags, optional BlockDuration,
        // ReferenceBlock, DiscardPadding, and BlockAdditions EBML framing.
        payload_len
            .saturating_add(additions_len)
            .saturating_add(add_wrap)
            .saturating_add(80)
    }
}

fn erase_webm_err(e: SessionError<WebmError>) -> SessionError<ErasedError> {
    e.map_codec(|c| ErasedError::new(Box::new(c)))
}

impl DynMuxer for BoundedWebmMuxer {
    fn begin_exact_timing(
        &mut self,
        tracks: &[TrackInfo],
        hint: MuxHint,
    ) -> Result<MuxTiming, SessionError<ErasedError>> {
        self.preflight_begin(tracks, hint, true)?;
        self.inner
            .begin_exact_timing(tracks, hint)
            .map_err(erase_webm_err)
    }

    fn begin(
        &mut self,
        tracks: &[TrackInfo],
        hint: MuxHint,
    ) -> Result<MuxTiming, SessionError<ErasedError>> {
        self.preflight_begin(tracks, hint, false)?;
        self.inner.begin(tracks, hint).map_err(erase_webm_err)
    }

    fn write_packet(
        &mut self,
        packet: Packet,
        pulse: Option<&dyn zencodec::Pulse>,
    ) -> Result<(), SessionError<ErasedError>> {
        let mut slot = Some(packet);
        match self.try_write_packet(&mut slot, pulse)? {
            PacketWrite::Accepted => Ok(()),
            _ => Err(SessionError::Unsupported(UnsupportedMedia::Container)),
        }
    }

    fn pending_output(&self) -> &[u8] {
        self.inner.pending_output()
    }

    fn consume_output(&mut self, n: usize) {
        self.inner.consume_output(n);
    }

    fn take_output(&mut self, into: &mut Vec<u8>) {
        self.inner.take_output(into);
    }

    fn finish(
        self: Box<Self>,
        pulse: Option<&dyn zencodec::Pulse>,
    ) -> Result<MuxFinish, SessionError<ErasedError>> {
        self.inner.finish(pulse).map_err(erase_webm_err)
    }

    fn as_bounded(&self) -> Option<&dyn BoundedMuxer> {
        Some(self)
    }

    fn as_bounded_mut(&mut self) -> Option<&mut dyn BoundedMuxer> {
        Some(self)
    }
}

impl BoundedMuxer for BoundedWebmMuxer {
    fn admitted_offer(&self) -> &MuxOffer {
        &self.offer
    }

    fn try_write_packet(
        &mut self,
        packet: &mut Option<Packet>,
        pulse: Option<&dyn zencodec::Pulse>,
    ) -> Result<PacketWrite, SessionError<ErasedError>> {
        if let Some(p) = pulse {
            p.check()?;
        }
        let Some(pkt_ref) = packet.as_ref() else {
            return Err(SessionError::Protocol(ProtocolViolation::EmptyPacket));
        };
        // Validate packet BEFORE potentially rolling or closing an open cluster.
        self.inner
            .validate_packet(pkt_ref)
            .map_err(erase_webm_err)?;
        const CLUSTER_HEADER_MAX: usize = 24;
        let pending = self.inner.pending_output().len();
        let block_est = Self::estimate_block_bytes(pkt_ref);
        let single_cluster_est = block_est.saturating_add(CLUSTER_HEADER_MAX);
        if single_cluster_est > self.max_pending || block_est > self.inner.max_cluster_bytes {
            // Even in its own fresh cluster, this packet exceeds max_pending or max_cluster_bytes.
            return Err(SessionError::Unsupported(UnsupportedMedia::Container));
        }
        let open_body = self.inner.cluster.as_ref().map_or(0, |c| c.body.len());
        let open_flush = if open_body > 0 {
            open_body.saturating_add(CLUSTER_HEADER_MAX)
        } else {
            0
        };
        let combined_body = open_body.saturating_add(block_est);
        let combined_cluster = combined_body.saturating_add(CLUSTER_HEADER_MAX);
        // Worst case if `write_packet` itself triggers a cluster roll (e.g. keyframe/duration)
        // AND the caller immediately calls `finish()` without draining:
        // pending + flushed open cluster + new cluster containing this packet.
        let worst_after_accept = if open_body > 0 {
            pending
                .saturating_add(open_flush)
                .saturating_add(single_cluster_est)
        } else {
            pending.saturating_add(single_cluster_est)
        };
        // If appending to the open cluster would exceed `max_pending`, `max_cluster_bytes`,
        // or the worst-case post-accept finish budget, roll the open cluster cleanly first.
        if open_body > 0
            && (combined_cluster > self.max_pending
                || combined_body > self.inner.max_cluster_bytes
                || worst_after_accept > self.max_pending)
        {
            if pending.saturating_add(open_flush) > self.max_pending {
                if pending > 0 {
                    return Ok(PacketWrite::DrainOutput);
                }
                return Err(SessionError::Unsupported(UnsupportedMedia::Container));
            }
            self.inner.close_cluster();
            if !self.inner.pending_output().is_empty() {
                return Ok(PacketWrite::DrainOutput);
            }
        } else if worst_after_accept > self.max_pending && pending > 0 {
            return Ok(PacketWrite::DrainOutput);
        }
        let pkt = packet.take().expect("checked Some above");
        self.inner
            .write_packet(pkt, pulse)
            .map_err(erase_webm_err)?;
        Ok(PacketWrite::Accepted)
    }
}

/// Shareable [`MuxFactory`] for WebM / Matroska containers backed by [`WebmMuxer`].
#[derive(Clone, Copy, Debug, Default)]
pub struct WebmMuxFactory {
    max_cluster_bytes: Option<usize>,
}

impl WebmMuxFactory {
    /// Create a WebM muxer factory with default cluster sizing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_cluster_bytes: None,
        }
    }

    /// Cap maximum cluster payload bytes before flushing a cluster.
    #[must_use]
    pub const fn with_max_cluster_bytes(mut self, bytes: usize) -> Self {
        self.max_cluster_bytes = Some(bytes);
        self
    }

    fn make_muxer(&self) -> WebmMuxer {
        let mut m = WebmMuxer::new();
        if let Some(bytes) = self.max_cluster_bytes {
            m = m.with_max_cluster_bytes(bytes);
        }
        m
    }
}

impl MuxFactory for WebmMuxFactory {
    fn quote(&self, need: &MuxNeeds, run: &JobResources) -> Result<MuxPlan, MediaError> {
        run.check_stop()?;
        if need.tracks.is_empty() {
            return Err(MediaError::Invalid(
                "WebM muxer requires at least one track",
            ));
        }
        let default_pending: u64 = 16 << 20;
        let ceiling = match run.limits().max_memory_bytes {
            Some(0) => {
                return Err(MediaError::Invalid(
                    "JobResources max_memory_bytes is too small for WebM header",
                ));
            }
            Some(m) => m.min(default_pending),
            None => default_pending,
        };
        let dry = self.make_muxer();
        let exact = matches!(need.timing, TimingPolicy::Exact);
        let header_len = dry
            .measure_header_bytes(&need.tracks, exact)
            .map_err(|e| match e {
                SessionError::Unsupported(u) => MediaError::Unsupported(u),
                SessionError::Codec(WebmError::UnsupportedCodec) => {
                    MediaError::Unsupported(UnsupportedMedia::Codec)
                }
                SessionError::Codec(
                    WebmError::EditList | WebmError::MixedClocks | WebmError::UnsupportedTrackKind,
                ) => MediaError::Unsupported(UnsupportedMedia::Container),
                other => MediaError::from(other),
            })? as u64;
        if header_len > ceiling {
            return Err(MediaError::Invalid(
                "WebM header exceeds admitted pending-output budget",
            ));
        }
        let timing = MuxTiming::streaming();
        let max_pending = match run.limits().max_memory_bytes {
            Some(m) => (m / 4).max(header_len).min(ceiling),
            None => default_pending,
        };
        let offer = MuxOffer::new(
            timing,
            NativeBound::UpperBound(max_pending),
            NativeBound::Unknown,
            NativeBound::UpperBound(max_pending),
        );
        let mut plan = MuxPlan::new(
            ContainerId::Webm,
            need.tracks.clone(),
            need.destination,
            need.timing,
            offer,
        );
        plan.supports_bounded_writes = true;
        Ok(plan)
    }

    fn open(
        &self,
        plan: &MuxPlan,
        run: &JobResources,
    ) -> Result<Box<dyn DynMuxer + Send>, MediaError> {
        run.check_stop()?;
        if plan.container != ContainerId::Webm {
            return Err(MediaError::Unsupported(UnsupportedMedia::Container));
        }
        let max_pending = match plan.offer.maximum_pending {
            NativeBound::Exact(n) | NativeBound::UpperBound(n) => {
                usize::try_from(n).unwrap_or(16 << 20)
            }
            _ => 16 << 20,
        };
        Ok(erase_bounded_muxer(BoundedWebmMuxer::new(
            self.make_muxer(),
            plan.offer,
            max_pending,
        )))
    }
}
