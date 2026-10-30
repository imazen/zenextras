//! The pull-output WebM writer.

use alloc::vec::Vec;

use zencodec::encode::OutputBuffer;
use zencodec::time::{FrameDuration, Rounding, TimeBase, Timestamp};
use zencodec_media::container::{MuxFinish, MuxHint, MuxTiming, Muxer};
use zencodec_media::packet::{Packet, RandomAccess};
use zencodec_media::session::{ProtocolViolation, SessionError, UnsupportedMedia};
use zencodec_media::timeline::TrackTimeline;
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

    fn track_entry(&mut self, i: usize, t: &TrackInfo) -> Res<(Vec<u8>, bool)> {
        let (codec, webm) =
            codec_id_for(t.codec()).ok_or(SessionError::Codec(WebmError::UnsupportedCodec))?;
        let kind = match t.kind() {
            TrackKind::Video => 1,
            TrackKind::Audio => 2,
            _ => return err(WebmError::UnsupportedTrackKind),
        };
        #[allow(deprecated)] // Preserve the legacy scalar refusal for unknown timelines.
        let legacy_edit = t.edit_delay_ticks();
        if legacy_edit.is_some_and(|d| d != 0) {
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
            ebml::put(&mut e, id::CODEC_PRIVATE, p);
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
        if tracks
            .iter()
            .any(|track| matches!(track.timeline(), TrackTimeline::Edited(_)))
        {
            return Err(SessionError::Unsupported(
                UnsupportedMedia::PresentationTimeline,
            ));
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
        pulse.unwrap_or(&zencodec::NoPulse).check()?;
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
                Some((end - ticks).max(0) as u64)
            }
            None => None,
        };
        let random_access = packet.random_access();
        let payload = packet
            .take()
            .ok_or(SessionError::Protocol(ProtocolViolation::EmptyPacket))?;

        // Roll the cluster: at a video random-access point, at the length
        // or size bound, or when the offset leaves 16 bits.
        let roll = match &self.cluster {
            None => true,
            Some(c) => {
                let rel = ticks - c.base;
                if rel < i64::from(i16::MIN) {
                    return err(WebmError::OutOfOrder);
                }
                rel > i64::from(i16::MAX)
                    || rel >= self.cluster_ticks
                    || c.body.len() >= self.max_cluster_bytes
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
        if duration.is_none() && padding.is_none() && additions.is_empty() {
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
            if random_access != RandomAccess::Yes {
                // A Block with no ReferenceBlock is a keyframe; point at the
                // track's previous block.
                let back = prev.map_or(-1, |p| (p - ticks).min(-1));
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
        pulse.unwrap_or(&zencodec::NoPulse).check()?;
        if !self.begun {
            return Err(SessionError::Protocol(ProtocolViolation::NoBegin));
        }
        self.close_cluster();
        Ok(MuxFinish::new(self.out.into_remaining(), Vec::new()))
    }
}
