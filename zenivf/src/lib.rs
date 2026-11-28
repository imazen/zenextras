//! IVF, libvpx's minimal raw-stream container, as a sans-I/O
//! [`Demuxer`] and a pull-output [`Muxer`] for `zencodec-media`.
//!
//! An IVF file is a 32-byte header (`DKIF`, version, header length, codec
//! fourcc, width, height, time base, frame count) and then packets, each a
//! 12-byte header (payload length, signed 64-bit presentation timestamp) and
//! the payload. It carries one video track and nothing else: no colour, no
//! audio, no index, no random-access flags, no loop count, no duration for
//! the last packet. VP8, VP9 and AV1 test vectors use it.
//!
//! - [`IvfDemuxer`] reads it from any `zencodec-io` driver. Packets carry
//!   [`RandomAccess::Unknown`]: IVF records no random-access points, and a
//!   codec keyframe flag is not one. A frame count of 0 or all ones means
//!   unknown; a declared count must match the packets. Seeking by time scans
//!   the packet headers with range requests (only
//!   [`SeekScope::Any`]; a random-access-scoped seek is unsupported, because
//!   the container cannot know).
//! - [`IvfMuxer`] writes the header at `begin` and each packet as it comes,
//!   with an unknown frame count; [`IvfMuxer::with_count_patch`] writes the
//!   real count at `finish` as a patch, for destinations that can seek back.
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use zencodec::JobResources;
use zencodec::encode::OutputBuffer;
use zencodec::source::{NativeBound, SourceAccess};
use zencodec::time::{TimeBase, Timestamp};
use zencodec_media::container::{
    AccessPattern, BoundedMuxer, DemuxEvent, DemuxProgress, Demuxer, DynDemuxer, DynMuxer, Input,
    InputRequest, MuxFinish, MuxHint, MuxOffer, MuxTiming, Muxer, PacketWrite, Patch, SeekMode,
    SeekOffer, SeekResult, SeekScope, SeekStatus, SeekTarget, SeekableDemuxer, erase_bounded_muxer,
    erase_seekable_demuxer,
};
use zencodec_media::limits::MediaLimits;
use zencodec_media::packet::{Packet, RandomAccess};
use zencodec_media::registry::{
    ContainerId, ContainerProbe, DemuxFactory, DemuxNeeds, DemuxOffer, MediaError, MuxFactory,
    MuxNeeds, MuxPlan, ProbeLimits, TimingPolicy,
};
use zencodec_media::session::{ErasedError, ProtocolViolation, SessionError, UnsupportedMedia};
use zencodec_media::track::{CodecId, CodecTag, TrackInfo, TrackKind, VideoTrackInfo};

/// The IVF signature.
pub const SIGNATURE: &[u8; 4] = b"DKIF";
/// The IVF header length this crate writes (and the minimum it reads).
pub const HEADER_LEN: usize = 32;
/// The length of a packet header.
pub const PACKET_HEADER_LEN: usize = 12;
const COUNT_OFFSET: u64 = 24;

/// Malformed or unrepresentable IVF.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum IvfError {
    /// The file does not start with `DKIF`.
    BadSignature,
    /// A version other than 0.
    UnsupportedVersion(u16),
    /// The header length field is below 32.
    ShortHeader,
    /// Zero width or height.
    ZeroDimensions,
    /// Zero time-base numerator or denominator.
    ZeroTimeBase,
    /// The file ends inside a header or a packet.
    Truncated,
    /// A packet with no payload.
    EmptyPacket,
    /// The packets disagree with the declared frame count.
    CountMismatch {
        /// The count in the header.
        declared: u32,
        /// Packets found.
        found: u64,
    },
    /// No packet satisfies the seek.
    NoSuchFrame,
    /// A seek before the header was read.
    HeadersFirst,
    /// IVF has one track: index 0.
    NoSuchTrack,
    /// IVF carries exactly one video track.
    OneVideoTrack,
    /// The codec has no IVF fourcc.
    UnsupportedCodec,
    /// Dimensions over 65535.
    DimensionsTooLarge,
    /// More packets than a 32-bit count can record.
    TooManyPackets,
}

impl fmt::Display for IvfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadSignature => f.write_str("not an IVF file (no DKIF signature)"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported IVF version {v}"),
            Self::ShortHeader => f.write_str("IVF header length is shorter than 32 bytes"),
            Self::ZeroDimensions => f.write_str("IVF dimensions are zero"),
            Self::ZeroTimeBase => f.write_str("IVF time base is zero"),
            Self::Truncated => f.write_str("IVF file ends inside a header or packet"),
            Self::EmptyPacket => f.write_str("IVF packet has no payload"),
            Self::CountMismatch { declared, found } => {
                write!(f, "IVF header declares {declared} frames, file has {found}")
            }
            Self::NoSuchFrame => f.write_str("no IVF packet satisfies the seek"),
            Self::HeadersFirst => f.write_str("seek before the IVF header was read"),
            Self::NoSuchTrack => f.write_str("IVF has one track, index 0"),
            Self::OneVideoTrack => f.write_str("IVF carries exactly one video track"),
            Self::UnsupportedCodec => f.write_str("codec has no IVF fourcc"),
            Self::DimensionsTooLarge => f.write_str("IVF dimensions are limited to 65535"),
            Self::TooManyPackets => f.write_str("more packets than IVF's 32-bit count"),
        }
    }
}

impl core::error::Error for IvfError {}

/// The codec an IVF fourcc names.
pub fn codec_from_fourcc(fourcc: [u8; 4]) -> CodecId {
    match &fourcc {
        b"AV01" => CodecId::Av1,
        b"VP90" => CodecId::Vp9,
        b"VP80" => CodecId::Vp8,
        b"H264" | b"AVC1" => CodecId::H264,
        b"HEVC" | b"H265" => CodecId::Hevc,
        _ => CodecId::Other(CodecTag::new(&alloc::string::String::from_utf8_lossy(
            &fourcc,
        ))),
    }
}

/// The IVF fourcc for a codec, if it has one.
pub fn fourcc_for(codec: &CodecId) -> Option<[u8; 4]> {
    Some(match codec {
        CodecId::Av1 => *b"AV01",
        CodecId::Vp9 => *b"VP90",
        CodecId::Vp8 => *b"VP80",
        CodecId::H264 => *b"H264",
        CodecId::Hevc => *b"HEVC",
        CodecId::Other(tag) => {
            let b = tag.as_str().as_bytes();
            if b.len() != 4 {
                return None;
            }
            [b[0], b[1], b[2], b[3]]
        }
        _ => return None,
    })
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn i64_at(b: &[u8], i: usize) -> i64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[i..i + 8]);
    i64::from_le_bytes(a)
}

fn codec(e: IvfError) -> SessionError<IvfError> {
    SessionError::Codec(e)
}

#[derive(Clone, Copy, Debug)]
struct IndexEntry {
    offset: u64,
    pts: i64,
    ordinal: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Header,
    Packets,
    Scanning,
    Done,
}

/// Sans-I/O IVF reader. Sequential; a seek scans packet headers with range
/// requests and keeps the index for later seeks.
#[derive(Debug)]
pub struct IvfDemuxer {
    limits: MediaLimits,
    state: State,
    tracks: Vec<TrackInfo>,
    requests: Vec<InputRequest>,
    time_base: Option<TimeBase>,
    header_len: u64,
    pos: u64,
    ordinal: u64,
    declared: Option<u32>,
    index: Vec<IndexEntry>,
    index_complete: bool,
    scan_pos: u64,
    scan_ordinal: u64,
    pending_seek: Option<SeekTarget>,
}

impl IvfDemuxer {
    /// A reader enforcing `limits` (packet size, header size, index size).
    pub fn new(limits: MediaLimits) -> Self {
        Self {
            limits,
            state: State::Header,
            tracks: Vec::new(),
            requests: Vec::new(),
            time_base: None,
            header_len: HEADER_LEN as u64,
            pos: 0,
            ordinal: 0,
            declared: None,
            index: Vec::new(),
            index_complete: false,
            scan_pos: 0,
            scan_ordinal: 0,
            pending_seek: None,
        }
    }

    /// The frame count the header declares, when it declares one.
    pub fn declared_frames(&self) -> Option<u32> {
        self.declared
    }

    fn need(&mut self, r: InputRequest) -> Result<DemuxProgress, SessionError<IvfError>> {
        self.requests.push(r);
        Ok(DemuxProgress::need(0))
    }

    fn note(&mut self, entry: IndexEntry) -> Result<(), SessionError<IvfError>> {
        if !self.index_complete && self.index.len() as u64 == entry.ordinal {
            self.limits
                .check_table_entries(self.index.len() as u64 + 1)
                .map_err(SessionError::Limit)?;
            self.index.push(entry);
        }
        Ok(())
    }

    fn pick(&self, target: &SeekTarget) -> Result<IndexEntry, SessionError<IvfError>> {
        let tb = self.time_base.ok_or(codec(IvfError::HeadersFirst))?;
        let t = target.time;
        let value = |e: &IndexEntry| {
            i128::from(e.pts) * i128::from(tb.numerator()) * i128::from(t.base().denominator())
        };
        let want =
            i128::from(t.ticks()) * i128::from(t.base().numerator()) * i128::from(tb.denominator());
        let all = self.index.iter();
        let chosen = match target.mode {
            SeekMode::Nearest => all.min_by(|a, b| {
                (value(a) - want)
                    .abs()
                    .cmp(&(value(b) - want).abs())
                    .then(a.pts.cmp(&b.pts))
            }),
            SeekMode::AtOrBefore => all.filter(|e| value(e) <= want).max_by_key(|e| e.pts),
            SeekMode::AtOrAfter => all.filter(|e| value(e) >= want).min_by_key(|e| e.pts),
            _ => return Err(SessionError::Unsupported(UnsupportedMedia::Seek)),
        };
        chosen.copied().ok_or(codec(IvfError::NoSuchFrame))
    }

    fn position(&mut self, e: IndexEntry) -> SeekResult {
        self.pos = e.offset;
        self.ordinal = e.ordinal;
        self.state = State::Packets;
        SeekResult::new(
            Timestamp::new(e.pts, self.time_base.expect("header read")),
            e.ordinal,
            RandomAccess::Unknown,
            0,
        )
    }

    fn finish_scan(&mut self, consumed: usize) -> Result<DemuxProgress, SessionError<IvfError>> {
        self.index_complete = true;
        let target = self.pending_seek.take().expect("scanning implies a target");
        let entry = self.pick(&target)?;
        let result = self.position(entry);
        Ok(DemuxProgress::new(consumed, DemuxEvent::Seeked(result)))
    }

    fn header(&mut self, input: Input<'_>) -> Result<DemuxProgress, SessionError<IvfError>> {
        if input.offset != 0 {
            return self.need(InputRequest::at_least(0, HEADER_LEN));
        }
        let b = input.bytes;
        if b.len() < HEADER_LEN {
            if input.reaches_end() {
                return Err(codec(IvfError::Truncated));
            }
            return self.need(InputRequest::at_least(0, HEADER_LEN));
        }
        if &b[0..4] != SIGNATURE {
            return Err(codec(IvfError::BadSignature));
        }
        let version = u16_at(b, 4);
        if version != 0 {
            return Err(codec(IvfError::UnsupportedVersion(version)));
        }
        let header_len = u16_at(b, 6);
        if usize::from(header_len) < HEADER_LEN {
            return Err(codec(IvfError::ShortHeader));
        }
        self.limits
            .check_header_bytes(u64::from(header_len))
            .map_err(SessionError::Limit)?;
        let fourcc = [b[8], b[9], b[10], b[11]];
        let (w, h) = (u32::from(u16_at(b, 12)), u32::from(u16_at(b, 14)));
        if w == 0 || h == 0 {
            return Err(codec(IvfError::ZeroDimensions));
        }
        // Rate (ticks per second) then scale (seconds per tick numerator).
        let (rate, scale) = (u32_at(b, 16), u32_at(b, 20));
        let tb = TimeBase::new(scale, rate).map_err(|_| codec(IvfError::ZeroTimeBase))?;
        let frames = u32_at(b, 24);
        // FFmpeg writes all ones when it cannot seek back to patch the count;
        // other writers write zero.
        self.declared = (frames != 0 && frames != u32::MAX).then_some(frames);
        let mut track =
            TrackInfo::video(0, codec_from_fourcc(fourcc), tb, VideoTrackInfo::new(w, h));
        if let Some(n) = self.declared {
            track = track.with_declared_packets(u64::from(n));
        }
        let track = track.build().map_err(|_| codec(IvfError::ZeroDimensions))?;
        self.tracks = vec![track];
        self.time_base = Some(tb);
        self.header_len = u64::from(header_len);
        self.pos = self.header_len;
        self.scan_pos = self.header_len;
        self.state = State::Packets;
        // Extension bytes beyond 32 are skipped by asking for packets at
        // `header_len`.
        Ok(DemuxProgress::new(HEADER_LEN, DemuxEvent::Headers))
    }

    fn packet(&mut self, input: Input<'_>) -> Result<DemuxProgress, SessionError<IvfError>> {
        if input.offset != self.pos {
            return self.need(InputRequest::at_least(self.pos, PACKET_HEADER_LEN));
        }
        let b = input.bytes;
        if b.is_empty() && input.reaches_end() {
            self.state = State::Done;
            self.index_complete = true;
            if let Some(declared) = self.declared
                && u64::from(declared) != self.ordinal
            {
                return Err(codec(IvfError::CountMismatch {
                    declared,
                    found: self.ordinal,
                }));
            }
            return Ok(DemuxProgress::new(0, DemuxEvent::Done));
        }
        if b.len() < PACKET_HEADER_LEN {
            if input.reaches_end() {
                return Err(codec(IvfError::Truncated));
            }
            return self.need(InputRequest::at_least(self.pos, PACKET_HEADER_LEN));
        }
        if let Some(declared) = self.declared
            && self.ordinal >= u64::from(declared)
        {
            return Err(codec(IvfError::CountMismatch {
                declared,
                found: self.ordinal + 1,
            }));
        }
        let len = u32_at(b, 0) as usize;
        if len == 0 {
            return Err(codec(IvfError::EmptyPacket));
        }
        self.limits
            .check_packet_bytes(len as u64)
            .map_err(SessionError::Limit)?;
        let total = PACKET_HEADER_LEN + len;
        if b.len() < total {
            if input.reaches_end() {
                return Err(codec(IvfError::Truncated));
            }
            return self.need(InputRequest::at_least(self.pos, total));
        }
        let pts = i64_at(b, 4);
        let tb = self.time_base.expect("header read");
        let mut data = Vec::new();
        data.try_reserve_exact(len)
            .map_err(|_| SessionError::Limit(packet_alloc_limit(len)))?;
        data.extend_from_slice(&b[PACKET_HEADER_LEN..total]);
        let packet = Packet::from_vec(data)
            .map_err(|_| codec(IvfError::EmptyPacket))?
            .with_track(0)
            .with_pts(Timestamp::new(pts, tb))
            .with_ordinal(self.ordinal)
            .with_input_offset(self.pos + PACKET_HEADER_LEN as u64);
        self.note(IndexEntry {
            offset: self.pos,
            pts,
            ordinal: self.ordinal,
        })?;
        self.pos += total as u64;
        if self.scan_ordinal <= self.ordinal {
            self.scan_pos = self.pos;
            self.scan_ordinal = self.ordinal + 1;
        }
        self.ordinal += 1;
        Ok(DemuxProgress::new(total, DemuxEvent::Packet(packet)))
    }

    fn scan(&mut self, input: Input<'_>) -> Result<DemuxProgress, SessionError<IvfError>> {
        let Some(total) = input.total_len else {
            return self.need(InputRequest::Len);
        };
        if self.scan_pos >= total {
            return self.finish_scan(0);
        }
        if input.offset != self.scan_pos || input.bytes.len() < PACKET_HEADER_LEN {
            if input.offset == self.scan_pos && input.reaches_end() {
                return Err(codec(IvfError::Truncated));
            }
            return self.need(InputRequest::range(self.scan_pos, PACKET_HEADER_LEN as u64));
        }
        let b = input.bytes;
        let len = u64::from(u32_at(b, 0));
        if len == 0 {
            return Err(codec(IvfError::EmptyPacket));
        }
        let entry = IndexEntry {
            offset: self.scan_pos,
            pts: i64_at(b, 4),
            ordinal: self.scan_ordinal,
        };
        self.note(entry)?;
        self.scan_pos += PACKET_HEADER_LEN as u64 + len;
        self.scan_ordinal += 1;
        if self.scan_pos > total {
            return Err(codec(IvfError::Truncated));
        }
        if self.scan_pos == total {
            return self.finish_scan(PACKET_HEADER_LEN);
        }
        self.requests
            .push(InputRequest::range(self.scan_pos, PACKET_HEADER_LEN as u64));
        Ok(DemuxProgress::need(PACKET_HEADER_LEN))
    }
}

fn packet_alloc_limit(len: usize) -> zencodec_media::limits::MediaLimitExceeded {
    zencodec_media::limits::MediaLimitExceeded::check(zencodec::LimitKind::Memory, len as u64, 0)
        .expect_err("a zero limit refuses any allocation")
}

impl Demuxer for IvfDemuxer {
    type Error = IvfError;

    fn step(
        &mut self,
        input: Input<'_>,
        pulse: Option<&dyn zencodec::Pulse>,
    ) -> Result<DemuxProgress, SessionError<IvfError>> {
        if let Some(pulse) = pulse {
            pulse.check()?;
        }
        self.requests.clear();
        match self.state {
            State::Header => self.header(input),
            State::Packets => self.packet(input),
            State::Scanning => self.scan(input),
            State::Done => Ok(DemuxProgress::new(0, DemuxEvent::Done)),
        }
    }

    fn requested(&self) -> &[InputRequest] {
        &self.requests
    }

    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    fn access_pattern(&self) -> AccessPattern {
        if self.state == State::Scanning {
            AccessPattern::RandomAccess
        } else {
            AccessPattern::Sequential
        }
    }

    fn seek(&mut self, target: &SeekTarget) -> Result<SeekStatus, SessionError<IvfError>> {
        if self.state == State::Header {
            return Err(codec(IvfError::HeadersFirst));
        }
        if target.track != 0 {
            return Err(codec(IvfError::NoSuchTrack));
        }
        if target.scope != SeekScope::Any {
            // IVF records no random-access points.
            return Err(SessionError::Unsupported(UnsupportedMedia::Seek));
        }
        if self.index_complete {
            let e = self.pick(target)?;
            return Ok(SeekStatus::Positioned(self.position(e)));
        }
        self.pending_seek = Some(*target);
        self.state = State::Scanning;
        Ok(SeekStatus::Scanning)
    }
}

impl SeekableDemuxer for IvfDemuxer {
    fn seek_offer(&self, target: &SeekTarget) -> Result<SeekOffer, SessionError<IvfError>> {
        if self.state == State::Header {
            return Err(codec(IvfError::HeadersFirst));
        }
        if target.track != 0 {
            return Err(codec(IvfError::NoSuchTrack));
        }
        if target.scope != SeekScope::Any {
            return Err(SessionError::Unsupported(UnsupportedMedia::Seek));
        }
        if self.index_complete {
            let e = self.pick(target)?;
            let landing = SeekResult::new(
                Timestamp::new(e.pts, self.time_base.expect("header read")),
                e.ordinal,
                RandomAccess::Unknown,
                0,
            );
            return Ok(
                SeekOffer::new(AccessPattern::RandomAccess, NativeBound::Exact(0))
                    .with_landing_estimate(landing)
                    .with_requires_index(false),
            );
        }
        Ok(
            SeekOffer::new(AccessPattern::RandomAccess, NativeBound::Unknown)
                .with_requires_index(true),
        )
    }

    fn begin_seek(
        &mut self,
        target: &SeekTarget,
        pulse: Option<&dyn zencodec::Pulse>,
    ) -> Result<SeekStatus, SessionError<IvfError>> {
        if let Some(p) = pulse {
            p.check()?;
        }
        self.seek(target)
    }
}

/// Shareable [`DemuxFactory`] for IVF containers backed by [`IvfDemuxer`].
#[derive(Clone, Debug)]
pub struct IvfDemuxFactory {
    limits: MediaLimits,
}

impl Default for IvfDemuxFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl IvfDemuxFactory {
    /// Create a factory using [`MediaLimits::DEFAULT`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            limits: MediaLimits::DEFAULT,
        }
    }

    /// Create a factory with custom [`MediaLimits`].
    #[must_use]
    pub const fn with_limits(limits: MediaLimits) -> Self {
        Self { limits }
    }

    fn effective_limits(&self, run: &JobResources) -> MediaLimits {
        let mut limits = self.limits;
        if let Some(mem) = run.limits().max_memory_bytes {
            limits.max_header_bytes = limits.max_header_bytes.min(mem);
            let pkt = usize::try_from(mem).unwrap_or(usize::MAX);
            if pkt < limits.max_packet_bytes {
                limits.max_packet_bytes = pkt;
            }
        }
        if let Some(max_in) = run.limits().max_input_bytes {
            let pkt = usize::try_from(max_in).unwrap_or(usize::MAX);
            if pkt < limits.max_packet_bytes {
                limits.max_packet_bytes = pkt;
            }
        }
        limits
    }
}

impl DemuxFactory for IvfDemuxFactory {
    fn probe(&self, input: Input<'_>, caps: &ProbeLimits) -> Result<ContainerProbe, MediaError> {
        if input.offset != 0 || caps.max_boxes == 0 {
            return Ok(ContainerProbe::NoMatch);
        }
        let max_bytes = caps.max_probe_bytes;
        if max_bytes < HEADER_LEN {
            return Ok(ContainerProbe::NoMatch);
        }
        let b = &input.bytes[..input.bytes.len().min(max_bytes)];
        if b.len() < 4 {
            return if input.reaches_end() {
                Ok(ContainerProbe::NoMatch)
            } else {
                Ok(ContainerProbe::Need {
                    min_bytes: HEADER_LEN,
                })
            };
        }
        if &b[..4] != SIGNATURE {
            return Ok(ContainerProbe::NoMatch);
        }
        if b.len() < HEADER_LEN {
            return if input.reaches_end() {
                Ok(ContainerProbe::NoMatch)
            } else {
                Ok(ContainerProbe::Need {
                    min_bytes: HEADER_LEN,
                })
            };
        }
        let version = u16_at(b, 4);
        let header_len = usize::from(u16_at(b, 6));
        let w = u16_at(b, 12);
        let h = u16_at(b, 14);
        if version == 0 && header_len >= HEADER_LEN && w > 0 && h > 0 {
            Ok(ContainerProbe::Confirmed {
                container: ContainerId::Ivf,
                confidence: 95,
            })
        } else {
            Ok(ContainerProbe::NoMatch)
        }
    }

    fn quote(
        &self,
        need: &DemuxNeeds,
        source: &SourceAccess,
        run: &JobResources,
    ) -> Result<DemuxOffer, MediaError> {
        run.check_stop()?;
        if run.limits().max_memory_bytes == Some(0) {
            return Err(MediaError::Invalid("IVF demuxer refused zero memory limit"));
        }
        if need.immutable_replay_required && !source.immutable {
            return Err(MediaError::Unsupported(UnsupportedMedia::Seek));
        }
        let access = if need.seek.is_some() {
            if source.access_pattern != AccessPattern::RandomAccess {
                return Err(MediaError::Unsupported(UnsupportedMedia::Seek));
            }
            AccessPattern::RandomAccess
        } else {
            AccessPattern::Sequential
        };
        let mut offer =
            DemuxOffer::new(ContainerId::Ivf, access).with_workspace(NativeBound::Unknown);
        if let Some(target) = need.seek {
            if target.scope != SeekScope::Any {
                return Err(MediaError::Unsupported(UnsupportedMedia::Seek));
            }
            offer = offer.with_seek_offer(
                SeekOffer::new(AccessPattern::RandomAccess, NativeBound::Unknown)
                    .with_requires_index(true),
            );
        }
        Ok(offer)
    }

    fn open(
        &self,
        offer: &DemuxOffer,
        run: &JobResources,
    ) -> Result<Box<dyn DynDemuxer + Send>, MediaError> {
        run.check_stop()?;
        if offer.container != ContainerId::Ivf {
            return Err(MediaError::Unsupported(UnsupportedMedia::Container));
        }
        Ok(erase_seekable_demuxer(IvfDemuxer::new(
            self.effective_limits(run),
        )))
    }
}

/// IVF writer. The header goes out at `begin`, each packet as it is
/// written; nothing waits for `finish`.
#[derive(Debug, Default)]
pub struct IvfMuxer {
    out: OutputBuffer,
    begun: bool,
    count_patch: bool,
    patching: bool,
    time_base: Option<TimeBase>,
    count: u64,
}

impl IvfMuxer {
    /// A writer that leaves the frame count unknown (0), so its output never
    /// needs patching.
    pub fn new() -> Self {
        Self::default()
    }

    /// Write the real frame count at `finish`, as a patch over the header,
    /// when the destination can seek back. A forward-only destination still
    /// gets the unknown-count layout.
    pub fn with_count_patch(mut self) -> Self {
        self.count_patch = true;
        self
    }
}

impl Muxer for IvfMuxer {
    type Error = IvfError;

    fn begin(
        &mut self,
        tracks: &[TrackInfo],
        hint: MuxHint,
    ) -> Result<MuxTiming, SessionError<IvfError>> {
        if self.begun {
            return Err(SessionError::Protocol(ProtocolViolation::BeginTwice));
        }
        let [track] = tracks else {
            return Err(codec(IvfError::OneVideoTrack));
        };
        if !track.timing().is_empty() {
            // IVF has no edit list, codec delay, or seek pre-roll carrier.
            return Err(SessionError::Unsupported(UnsupportedMedia::Container));
        }
        let Some(video) = track
            .video_info()
            .filter(|_| track.kind() == TrackKind::Video)
        else {
            return Err(codec(IvfError::OneVideoTrack));
        };
        let fourcc = fourcc_for(track.codec()).ok_or(codec(IvfError::UnsupportedCodec))?;
        let (Ok(w), Ok(h)) = (
            u16::try_from(video.coded_width),
            u16::try_from(video.coded_height),
        ) else {
            return Err(codec(IvfError::DimensionsTooLarge));
        };
        let tb = track.time_base();
        let mut header = [0u8; HEADER_LEN];
        header[..4].copy_from_slice(SIGNATURE);
        header[6..8].copy_from_slice(&(HEADER_LEN as u16).to_le_bytes());
        header[8..12].copy_from_slice(&fourcc);
        header[12..14].copy_from_slice(&w.to_le_bytes());
        header[14..16].copy_from_slice(&h.to_le_bytes());
        header[16..20].copy_from_slice(&tb.denominator().to_le_bytes());
        header[20..24].copy_from_slice(&tb.numerator().to_le_bytes());
        self.out.push(&header);
        self.time_base = Some(tb);
        self.patching = self.count_patch && !hint.forward_only;
        self.begun = true;
        Ok(if self.patching {
            MuxTiming::streaming_with_patches()
        } else {
            MuxTiming::streaming()
        })
    }

    fn write_packet(
        &mut self,
        mut packet: Packet,
        pulse: Option<&dyn zencodec::Pulse>,
    ) -> Result<(), SessionError<IvfError>> {
        if let Some(pulse) = pulse {
            pulse.check()?;
        }
        if !self.begun {
            return Err(SessionError::Protocol(ProtocolViolation::NoBegin));
        }
        if packet.track() != 0 {
            return Err(codec(IvfError::NoSuchTrack));
        }
        let pts = packet
            .pts()
            .ok_or(SessionError::Protocol(ProtocolViolation::MissingTimestamp))?;
        if Some(pts.base()) != self.time_base {
            return Err(SessionError::Protocol(ProtocolViolation::ClockMismatch));
        }
        if self.patching && self.count >= u64::from(u32::MAX - 1) {
            return Err(codec(IvfError::TooManyPackets));
        }
        let payload = packet
            .take()
            .ok_or(SessionError::Protocol(ProtocolViolation::EmptyPacket))?;
        let bytes = payload.as_bytes();
        let len = u32::try_from(bytes.len()).map_err(|_| {
            SessionError::Limit(
                zencodec_media::limits::MediaLimitExceeded::check(
                    zencodec::LimitKind::PacketBytes,
                    bytes.len() as u64,
                    u64::from(u32::MAX),
                )
                .expect_err("over u32"),
            )
        })?;
        self.out.push(&len.to_le_bytes());
        self.out.push(&pts.ticks().to_le_bytes());
        self.out.push(bytes);
        self.count += 1;
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

    fn finish(
        self,
        pulse: Option<&dyn zencodec::Pulse>,
    ) -> Result<MuxFinish, SessionError<IvfError>> {
        if let Some(pulse) = pulse {
            pulse.check()?;
        }
        if !self.begun {
            return Err(SessionError::Protocol(ProtocolViolation::NoBegin));
        }
        let patches = if self.patching {
            vec![Patch {
                offset: COUNT_OFFSET,
                bytes: (self.count as u32).to_le_bytes().to_vec(),
            }]
        } else {
            Vec::new()
        };
        Ok(MuxFinish::new(self.out.into_remaining(), patches))
    }
}

/// [`IvfMuxer`] wrapped with output-credit backpressure implementing [`DynMuxer`]
/// and [`BoundedMuxer`].
#[derive(Debug)]
pub struct BoundedIvfMuxer {
    inner: IvfMuxer,
    offer: MuxOffer,
    max_pending: usize,
}

impl BoundedIvfMuxer {
    /// Wrap `inner` with an admitted [`MuxOffer`] and pending-byte cap.
    #[must_use]
    pub fn new(inner: IvfMuxer, offer: MuxOffer, max_pending: usize) -> Self {
        Self {
            inner,
            offer,
            max_pending: max_pending.max(1),
        }
    }
}

fn erase_ivf_err(e: SessionError<IvfError>) -> SessionError<ErasedError> {
    e.map_codec(|c| ErasedError::new(Box::new(c)))
}

impl DynMuxer for BoundedIvfMuxer {
    fn begin_exact_timing(
        &mut self,
        tracks: &[TrackInfo],
        hint: MuxHint,
    ) -> Result<MuxTiming, SessionError<ErasedError>> {
        if HEADER_LEN > self.max_pending {
            return Err(SessionError::Unsupported(UnsupportedMedia::Container));
        }
        self.inner
            .begin_exact_timing(tracks, hint)
            .map_err(erase_ivf_err)
    }

    fn begin(
        &mut self,
        tracks: &[TrackInfo],
        hint: MuxHint,
    ) -> Result<MuxTiming, SessionError<ErasedError>> {
        if HEADER_LEN > self.max_pending {
            return Err(SessionError::Unsupported(UnsupportedMedia::Container));
        }
        self.inner.begin(tracks, hint).map_err(erase_ivf_err)
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
        self.inner.finish(pulse).map_err(erase_ivf_err)
    }

    fn as_bounded(&self) -> Option<&dyn BoundedMuxer> {
        Some(self)
    }

    fn as_bounded_mut(&mut self) -> Option<&mut dyn BoundedMuxer> {
        Some(self)
    }
}

impl BoundedMuxer for BoundedIvfMuxer {
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
        let emission = 12usize.saturating_add(pkt_ref.bytes().len());
        let pending = self.inner.pending_output().len();
        if pending.saturating_add(emission) > self.max_pending {
            if pending > 0 {
                return Ok(PacketWrite::DrainOutput);
            }
            return Err(SessionError::Unsupported(UnsupportedMedia::Container));
        }
        let pkt = packet.take().expect("checked Some above");
        self.inner.write_packet(pkt, pulse).map_err(erase_ivf_err)?;
        Ok(PacketWrite::Accepted)
    }
}

/// Shareable [`MuxFactory`] for IVF containers backed by [`IvfMuxer`].
#[derive(Clone, Copy, Debug, Default)]
pub struct IvfMuxFactory {
    count_patch: bool,
}

impl IvfMuxFactory {
    /// Streaming IVF writer leaving frame count 0 (no finish patches).
    #[must_use]
    pub const fn new() -> Self {
        Self { count_patch: false }
    }

    /// Patch the frame count into the 32-byte header on seekable destinations.
    #[must_use]
    pub const fn with_count_patch(mut self) -> Self {
        self.count_patch = true;
        self
    }
}

impl MuxFactory for IvfMuxFactory {
    fn quote(&self, need: &MuxNeeds, run: &JobResources) -> Result<MuxPlan, MediaError> {
        run.check_stop()?;
        if need.timing == TimingPolicy::Exact {
            // IVF stores PTS only (no DTS, no per-frame duration).
            return Err(MediaError::Unsupported(UnsupportedMedia::ExactTiming));
        }
        let mut dry = if self.count_patch {
            IvfMuxer::new().with_count_patch()
        } else {
            IvfMuxer::new()
        };
        let timing = dry
            .begin(&need.tracks, need.destination)
            .map_err(|e| match e {
                SessionError::Unsupported(u) => MediaError::Unsupported(u),
                SessionError::Codec(IvfError::UnsupportedCodec) => {
                    MediaError::Unsupported(UnsupportedMedia::Codec)
                }
                SessionError::Codec(IvfError::OneVideoTrack) => {
                    MediaError::Unsupported(UnsupportedMedia::Container)
                }
                other => MediaError::from(other),
            })?;
        let header_len = dry.pending_output().len() as u64;
        let default_pending: u64 = 4 << 20;
        let max_pending = match run.limits().max_memory_bytes {
            Some(m) if m < header_len.max(1) => {
                return Err(MediaError::Invalid(
                    "JobResources max_memory_bytes is too small for IVF header",
                ));
            }
            Some(m) => (m / 4).max(header_len).min(m).min(default_pending),
            None => default_pending,
        };
        let offer = MuxOffer::new(
            timing,
            NativeBound::UpperBound(max_pending),
            NativeBound::Unknown,
            NativeBound::Exact(0),
        );
        let mut plan = MuxPlan::new(
            ContainerId::Ivf,
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
        if plan.container != ContainerId::Ivf {
            return Err(MediaError::Unsupported(UnsupportedMedia::Container));
        }
        let max_pending = match plan.offer.maximum_pending {
            NativeBound::Exact(n) | NativeBound::UpperBound(n) => {
                usize::try_from(n).unwrap_or(4 << 20)
            }
            _ => 4 << 20,
        };
        let inner = if self.count_patch && plan.offer.timing.patches_at_finish() {
            IvfMuxer::new().with_count_patch()
        } else {
            IvfMuxer::new()
        };
        Ok(erase_bounded_muxer(BoundedIvfMuxer::new(
            inner,
            plan.offer,
            max_pending,
        )))
    }
}

/// Register [`IvfDemuxFactory`] and [`IvfMuxFactory`] in a [`zencodec::CodecSet`]
/// under `org.imazen.ivf.zenivf`.
pub fn register_ivf_codec_set(
    set: zencodec::CodecSet,
) -> Result<zencodec::CodecSet, zencodec::RegistrationError> {
    use alloc::sync::Arc;
    use zencodec::{NamedFactory, RegistrationId};
    use zencodec_media::registry::{DemuxRole, MuxRole};

    let id = RegistrationId::new("org.imazen.ivf.zenivf").expect("valid registration id");
    let set = set.with_role::<DemuxRole>(
        [ContainerId::Ivf],
        NamedFactory::new(
            id.clone(),
            env!("CARGO_PKG_VERSION"),
            Arc::new(IvfDemuxFactory::new()) as Arc<dyn DemuxFactory>,
        ),
    )?;
    set.with_role::<MuxRole>(
        [ContainerId::Ivf],
        NamedFactory::new(
            id,
            env!("CARGO_PKG_VERSION"),
            Arc::new(IvfMuxFactory::new()) as Arc<dyn MuxFactory>,
        ),
    )
}
