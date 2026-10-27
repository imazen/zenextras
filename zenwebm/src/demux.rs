//! The sans-I/O WebM reader.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use zencodec::Cicp;
use zencodec::time::{FrameDuration, TimeBase, Timestamp};
use zencodec_media::container::{
    AccessPattern, DemuxEvent, DemuxProgress, Demuxer, Input, InputRequest, SeekMode, SeekResult,
    SeekStatus, SeekTarget,
};
use zencodec_media::limits::{MediaLimitExceeded, MediaLimits};
use zencodec_media::packet::{Discard, Packet, PacketAddition, PacketPayload, RandomAccess};
use zencodec_media::session::{SessionError, UnsupportedMedia};
use zencodec_media::track::{AudioTrackInfo, ChannelLayout, TrackInfo, TrackKind, VideoTrackInfo};

use crate::ebml::{self, Header, MAX_HEADER_LEN, id};
use crate::{WebmError, codec_from_id};

type Res<T> = Result<T, SessionError<WebmError>>;

fn err<T>(e: WebmError) -> Res<T> {
    Err(SessionError::Codec(e))
}

const NS_PER_SECOND: u32 = 1_000_000_000;
const DEFAULT_SCALE_NS: u32 = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ebml,
    Segment,
    Body,
    /// A seek is indexing block headers.
    Scanning,
    Done,
}

/// One block, as a seek index records it: enough to resume demuxing there.
#[derive(Clone, Copy, Debug)]
struct SeekEntry {
    /// Offset of the SimpleBlock or BlockGroup element.
    offset: u64,
    cluster_end: Option<u64>,
    cluster_ts: i64,
    track: usize,
    /// The first frame's timestamp (later frames of a lace have none).
    pts: i64,
    key: bool,
    /// Per-track ordinal of the first frame.
    ordinal: u64,
    frames: u64,
}

/// Where a seek's index scan is.
#[derive(Debug)]
struct Scan {
    pos: u64,
    /// In a cluster: its end (when known) and Timestamp (once read).
    cluster: Option<(Option<u64>, Option<i64>)>,
    ordinals: Vec<u64>,
}

#[derive(Debug)]
struct Cluster {
    /// Absolute end offset, when the cluster's size is known.
    end: Option<u64>,
    /// The cluster's Timestamp, once read.
    timestamp: Option<i64>,
}

/// A TrackEntry as read, before the segment's clock is known.
#[derive(Debug, Default)]
struct RawTrack {
    number: u64,
    kind: u64,
    codec: String,
    private: Option<Vec<u8>>,
    codec_delay_ns: u64,
    seek_preroll_ns: u64,
    video: Option<RawVideo>,
    audio: Option<(f64, u64)>,
}

#[derive(Debug, Default)]
struct RawVideo {
    width: u64,
    height: u64,
    display: (Option<u64>, Option<u64>),
    display_unit: u64,
    /// Primaries, transfer, matrix, range, when a Colour element is present.
    color: Option<[u64; 4]>,
}

/// What a BlockGroup adds to its Block.
#[derive(Debug, Default)]
struct GroupExtras {
    duration: Option<u64>,
    referenced: bool,
    discard_padding: Option<i64>,
    additions: Vec<(u64, Vec<u8>)>,
}

/// Sans-I/O WebM reader: one element per step, sequential.
#[derive(Debug)]
pub struct WebmDemuxer {
    limits: MediaLimits,
    state: State,
    requests: Vec<InputRequest>,
    /// Offset of the next element header.
    pos: u64,
    segment_end: Option<u64>,
    cluster: Option<Cluster>,
    raw_tracks: Option<Vec<RawTrack>>,
    tracks: Vec<TrackInfo>,
    numbers: Vec<u64>,
    ordinals: Vec<u64>,
    scale_ns: u32,
    time_base: TimeBase,
    headers_sent: bool,
    pending: VecDeque<Packet>,
    matroska: bool,
    /// The source length, once a driver told it.
    total: Option<u64>,
    /// Offset of the first byte of the Segment's data.
    segment_data: u64,
    /// Every block's header, built by the first seek.
    index: Vec<SeekEntry>,
    index_complete: bool,
    scan: Option<Scan>,
    pending_seek: Option<SeekTarget>,
}

impl WebmDemuxer {
    /// A reader enforcing `limits` (packet size, header size, track count).
    pub fn new(limits: MediaLimits) -> Self {
        Self {
            limits,
            state: State::Ebml,
            requests: Vec::new(),
            pos: 0,
            segment_end: None,
            cluster: None,
            raw_tracks: None,
            tracks: Vec::new(),
            numbers: Vec::new(),
            ordinals: Vec::new(),
            scale_ns: DEFAULT_SCALE_NS,
            time_base: TimeBase::new(DEFAULT_SCALE_NS, NS_PER_SECOND).expect("nonzero"),
            headers_sent: false,
            pending: VecDeque::new(),
            matroska: false,
            total: None,
            segment_data: 0,
            index: Vec::new(),
            index_complete: false,
            scan: None,
            pending_seek: None,
        }
    }

    /// The segment's tick length in nanoseconds (`TimestampScale`).
    pub fn timestamp_scale_ns(&self) -> u32 {
        self.scale_ns
    }

    /// Whether the document type is `matroska` rather than `webm`.
    pub fn is_matroska(&self) -> bool {
        self.matroska
    }

    fn need(&mut self, consumed: usize) -> Res<DemuxProgress> {
        // An element that runs past the known end is truncated input, not
        // a request for bytes that do not exist.
        if self.total.is_some_and(|t| self.pos > t) {
            return err(WebmError::Truncated);
        }
        self.requests
            .push(InputRequest::at_least(self.pos, MAX_HEADER_LEN));
        Ok(DemuxProgress::need(consumed))
    }

    /// The element header at `pos`, or a request for it.
    fn header(&mut self, input: &Input<'_>) -> Res<Result<Header, DemuxProgress>> {
        if input.offset != self.pos {
            return self.need(0).map(Err);
        }
        match ebml::header(input.bytes).map_err(SessionError::Codec)? {
            Some(h) => Ok(Ok(h)),
            None if input.reaches_end() => err(WebmError::Truncated),
            None => self.need(0).map(Err),
        }
    }

    /// The whole payload of the element at `pos`, or a request for it.
    fn body<'a>(&mut self, h: &Header, input: &Input<'a>) -> Res<Result<&'a [u8], DemuxProgress>> {
        let total = h
            .total()
            .and_then(|t| usize::try_from(t).ok())
            .ok_or(SessionError::Codec(WebmError::UnknownSize))?;
        if input.bytes.len() >= total {
            return Ok(Ok(&input.bytes[h.len..total]));
        }
        if input.reaches_end() {
            return err(WebmError::Truncated);
        }
        self.requests.push(InputRequest::at_least(self.pos, total));
        Ok(Err(DemuxProgress::need(0)))
    }

    /// Step over the element at `pos` without reading it.
    fn skip(&mut self, h: &Header, input: &Input<'_>) -> Res<DemuxProgress> {
        let total = h
            .total()
            .ok_or(SessionError::Codec(WebmError::UnknownSize))?;
        let consumed =
            usize::try_from(total).map_or(input.bytes.len(), |t| t.min(input.bytes.len()));
        self.pos += total;
        self.need(consumed)
    }

    fn finish(&mut self) -> Res<DemuxProgress> {
        if !self.headers_sent {
            // A file with tracks and no clusters still has headers.
            self.send_headers()?;
            return Ok(DemuxProgress::new(0, DemuxEvent::Headers));
        }
        self.state = State::Done;
        Ok(DemuxProgress::new(0, DemuxEvent::Done))
    }

    fn ebml_header(&mut self, input: Input<'_>) -> Res<DemuxProgress> {
        let h = match self.header(&input)? {
            Ok(h) => h,
            Err(p) => return Ok(p),
        };
        if h.id != id::EBML {
            return err(WebmError::NotEbml);
        }
        let total = h
            .total()
            .ok_or(SessionError::Codec(WebmError::UnknownSize))?;
        self.limits
            .check_header_bytes(total)
            .map_err(SessionError::Limit)?;
        let body = match self.body(&h, &input)? {
            Ok(b) => b,
            Err(p) => return Ok(p),
        };
        let mut doc = None;
        for child in ebml::children(body) {
            let (cid, p) = child.map_err(SessionError::Codec)?;
            if cid == id::DOC_TYPE {
                doc = Some(p);
            }
        }
        // A missing DocType defaults to "matroska".
        let doc = doc.unwrap_or(b"matroska");
        let doc = doc.strip_suffix(&[0]).unwrap_or(doc);
        self.matroska = match doc {
            b"webm" => false,
            b"matroska" => true,
            _ => return err(WebmError::UnsupportedDocType),
        };
        self.pos = total;
        self.state = State::Segment;
        self.need(total as usize)
    }

    fn segment_header(&mut self, input: Input<'_>) -> Res<DemuxProgress> {
        if input.offset == self.pos && input.bytes.is_empty() && input.reaches_end() {
            return err(WebmError::NoSegment);
        }
        let h = match self.header(&input)? {
            Ok(h) => h,
            Err(p) => return Ok(p),
        };
        match h.id {
            id::SEGMENT => {
                self.pos += h.len as u64;
                self.segment_data = self.pos;
                self.segment_end = h.size.map(|s| self.pos + s);
                self.state = State::Body;
                self.need(h.len)
            }
            // Void and CRC-32 may sit between the header and the Segment.
            id::VOID | id::CRC32 => self.skip(&h, &input),
            _ => err(WebmError::NoSegment),
        }
    }

    fn body_step(&mut self, input: Input<'_>) -> Res<DemuxProgress> {
        if self
            .cluster
            .as_ref()
            .is_some_and(|c| c.end == Some(self.pos))
        {
            self.cluster = None;
        }
        if self.segment_end == Some(self.pos) {
            return self.finish();
        }
        if input.offset == self.pos && input.bytes.is_empty() && input.reaches_end() {
            // The file ends between elements: fine only where sizes are
            // unknown.
            if self.segment_end.is_some() || self.cluster.as_ref().is_some_and(|c| c.end.is_some())
            {
                return err(WebmError::Truncated);
            }
            return self.finish();
        }
        let h = match self.header(&input)? {
            Ok(h) => h,
            Err(p) => return Ok(p),
        };
        if let Some(c) = &self.cluster
            && id::is_top_level(h.id)
        {
            if c.end.is_some() {
                return err(WebmError::Overrun);
            }
            // A top-level element ends a cluster of unknown size.
            self.cluster = None;
        }
        let parent_end = match &self.cluster {
            Some(c) => c.end.or(self.segment_end),
            None => self.segment_end,
        };
        if let (Some(total), Some(end)) = (h.total(), parent_end)
            && self.pos.checked_add(total).is_none_or(|e| e > end)
        {
            return err(WebmError::Overrun);
        }
        if h.size.is_none() && h.id != id::CLUSTER {
            return err(WebmError::UnknownSize);
        }
        if self.cluster.is_some() {
            self.cluster_child(h, input)
        } else {
            self.segment_child(h, input)
        }
    }

    fn segment_child(&mut self, h: Header, input: Input<'_>) -> Res<DemuxProgress> {
        match h.id {
            id::INFO | id::TRACKS => {
                self.limits
                    .check_header_bytes(h.total().unwrap_or(u64::MAX))
                    .map_err(SessionError::Limit)?;
                let body = match self.body(&h, &input)? {
                    Ok(b) => b,
                    Err(p) => return Ok(p),
                };
                if h.id == id::INFO {
                    self.read_info(body)?;
                } else {
                    if self.raw_tracks.is_some() || self.headers_sent {
                        return err(WebmError::DuplicateTracks);
                    }
                    self.raw_tracks = Some(read_tracks(body, &self.limits)?);
                }
                let total = h.len + body.len();
                self.pos += total as u64;
                self.need(total)
            }
            id::CLUSTER => {
                self.pos += h.len as u64;
                self.cluster = Some(Cluster {
                    end: h.size.map(|s| self.pos + s),
                    timestamp: None,
                });
                if !self.headers_sent {
                    self.send_headers()?;
                    self.requests
                        .push(InputRequest::at_least(self.pos, MAX_HEADER_LEN));
                    return Ok(DemuxProgress::new(h.len, DemuxEvent::Headers));
                }
                self.need(h.len)
            }
            // A second EBML document (a chained file) ends this one.
            id::EBML | id::SEGMENT => self.finish(),
            _ => self.skip(&h, &input),
        }
    }

    fn cluster_child(&mut self, h: Header, input: Input<'_>) -> Res<DemuxProgress> {
        match h.id {
            id::TIMESTAMP | id::SIMPLE_BLOCK | id::BLOCK_GROUP => {
                let size = h.size.expect("checked: only clusters have unknown size");
                if h.id != id::TIMESTAMP {
                    self.limits
                        .check_packet_bytes(size)
                        .map_err(SessionError::Limit)?;
                }
                let body = match self.body(&h, &input)? {
                    Ok(b) => b,
                    Err(p) => return Ok(p),
                };
                let body_offset = self.pos + h.len as u64;
                match h.id {
                    id::TIMESTAMP => {
                        let ts = ebml::uint(body).map_err(SessionError::Codec)?;
                        let ts = i64::try_from(ts)
                            .map_err(|_| SessionError::Codec(WebmError::TimestampOverflow))?;
                        self.cluster.as_mut().expect("in a cluster").timestamp = Some(ts);
                    }
                    id::SIMPLE_BLOCK => {
                        self.block(body, body_offset, true, GroupExtras::default())?;
                    }
                    _ => self.block_group(body, body_offset)?,
                }
                let total = h.len + body.len();
                self.pos += total as u64;
                match self.pending.pop_front() {
                    Some(p) => Ok(DemuxProgress::new(total, DemuxEvent::Packet(p))),
                    None => self.need(total),
                }
            }
            _ => self.skip(&h, &input),
        }
    }

    /// Ask for `len` bytes at `pos` during a scan.
    fn scan_need(&mut self, pos: u64, len: usize) -> Res<DemuxProgress> {
        if self.total.is_some_and(|t| pos > t) {
            return err(WebmError::Truncated);
        }
        self.requests.push(InputRequest::at_least(pos, len));
        Ok(DemuxProgress::need(0))
    }

    /// One step of a seek's index scan: read the element header at the scan
    /// position and record blocks without reading their payloads.
    fn scan_step(&mut self, input: Input<'_>) -> Res<DemuxProgress> {
        let scan = self.scan.as_mut().expect("scanning");
        if let Some((Some(end), _)) = scan.cluster
            && end == scan.pos
        {
            scan.cluster = None;
        }
        let pos = scan.pos;
        let at_end = self.segment_end == Some(pos)
            || (input.offset == pos && input.bytes.is_empty() && input.reaches_end());
        if at_end {
            self.index_complete = true;
            self.scan = None;
            return self.resolve_seek();
        }
        if input.offset != pos {
            return self.scan_need(pos, 32);
        }
        let h = match ebml::header(input.bytes).map_err(SessionError::Codec)? {
            Some(h) => h,
            None if input.reaches_end() => return err(WebmError::Truncated),
            None => return self.scan_need(pos, 32),
        };
        let scan = self.scan.as_mut().expect("scanning");
        if scan.cluster.is_some() && id::is_top_level(h.id) {
            // A top-level element ends a cluster of unknown size.
            scan.cluster = None;
        }
        let Some((cluster_end, cluster_ts)) = scan.cluster else {
            if h.id == id::CLUSTER {
                scan.cluster = Some((h.size.map(|s| pos + h.len as u64 + s), None));
                scan.pos = pos + h.len as u64;
                return self.scan_need(pos + h.len as u64, 32);
            }
            if matches!(h.id, id::EBML | id::SEGMENT) {
                self.index_complete = true;
                self.scan = None;
                return self.resolve_seek();
            }
            let total = h
                .total()
                .ok_or(SessionError::Codec(WebmError::UnknownSize))?;
            let next = pos + total;
            scan.pos = next;
            return self.scan_need(next, 32);
        };
        let total = h
            .total()
            .ok_or(SessionError::Codec(WebmError::UnknownSize))?;
        match h.id {
            id::TIMESTAMP | id::SIMPLE_BLOCK | id::BLOCK_GROUP => {
                // A Timestamp's body; a SimpleBlock's head (track number,
                // timecode, flags, lace count); a BlockGroup whole.
                let want = match h.id {
                    id::SIMPLE_BLOCK => total.min(h.len as u64 + 12),
                    id::BLOCK_GROUP => {
                        self.limits
                            .check_packet_bytes(total)
                            .map_err(SessionError::Limit)?;
                        total
                    }
                    _ => total,
                };
                let want =
                    usize::try_from(want).map_err(|_| SessionError::Codec(WebmError::Overrun))?;
                if input.bytes.len() < want {
                    if input.reaches_end() {
                        return err(WebmError::Truncated);
                    }
                    return self.scan_need(pos, want);
                }
                let body = &input.bytes[h.len..want];
                match h.id {
                    id::TIMESTAMP => {
                        let ts = ebml::uint(body).map_err(SessionError::Codec)?;
                        let ts = i64::try_from(ts)
                            .map_err(|_| SessionError::Codec(WebmError::TimestampOverflow))?;
                        let scan = self.scan.as_mut().expect("scanning");
                        scan.cluster = Some((cluster_end, Some(ts)));
                    }
                    _ => {
                        let (head, key_from_group) = if h.id == id::SIMPLE_BLOCK {
                            (body, None)
                        } else {
                            let mut block = None;
                            let mut referenced = false;
                            for child in ebml::children(body) {
                                let (cid, p) = child.map_err(SessionError::Codec)?;
                                match cid {
                                    id::BLOCK => block = Some(p),
                                    id::REFERENCE_BLOCK => referenced = true,
                                    _ => {}
                                }
                            }
                            (
                                block.ok_or(SessionError::Codec(WebmError::BadBlock))?,
                                Some(!referenced),
                            )
                        };
                        let bad = SessionError::Codec(WebmError::BadBlock);
                        let (number, nlen) = ebml::vint(head, 8, false)
                            .map_err(SessionError::Codec)?
                            .ok_or(bad)?;
                        if head.len() < nlen + 3 {
                            return err(WebmError::BadBlock);
                        }
                        let rel = i16::from_be_bytes([head[nlen], head[nlen + 1]]);
                        let flags = head[nlen + 2];
                        let frames = if (flags >> 1) & 3 != 0 {
                            u64::from(
                                *head
                                    .get(nlen + 3)
                                    .ok_or(SessionError::Codec(WebmError::BadBlock))?,
                            ) + 1
                        } else {
                            1
                        };
                        let track = self
                            .numbers
                            .iter()
                            .position(|&n| n == number)
                            .ok_or(SessionError::Codec(WebmError::UndeclaredTrack))?;
                        let base = cluster_ts
                            .ok_or(SessionError::Codec(WebmError::BlockBeforeTimestamp))?;
                        let pts = base
                            .checked_add(i64::from(rel))
                            .ok_or(SessionError::Codec(WebmError::TimestampOverflow))?;
                        self.limits
                            .check_table_entries(self.index.len() as u64 + 1)
                            .map_err(SessionError::Limit)?;
                        let scan = self.scan.as_mut().expect("scanning");
                        let ordinal = scan.ordinals[track];
                        scan.ordinals[track] += frames;
                        self.index.push(SeekEntry {
                            offset: pos,
                            cluster_end,
                            cluster_ts: base,
                            track,
                            pts,
                            key: key_from_group.unwrap_or(flags & 0x80 != 0),
                            ordinal,
                            frames,
                        });
                    }
                }
            }
            _ => {}
        }
        let next = pos + total;
        self.scan.as_mut().expect("scanning").pos = next;
        self.scan_need(next, 32)
    }

    /// The index entry a seek selects.
    fn pick(&self, target: &SeekTarget) -> Res<SeekEntry> {
        let track = target.track as usize;
        let tb = self.time_base;
        let t = target.time;
        let value = |pts: i64| {
            i128::from(pts) * i128::from(tb.numerator()) * i128::from(t.base().denominator())
        };
        let want =
            i128::from(t.ticks()) * i128::from(t.base().numerator()) * i128::from(tb.denominator());
        let candidates = self.index.iter().filter(|e| {
            e.track == track && (e.key || target.scope == zencodec_media::container::SeekScope::Any)
        });
        let chosen = match target.mode {
            SeekMode::Nearest => candidates.min_by(|a, b| {
                (value(a.pts) - want)
                    .abs()
                    .cmp(&(value(b.pts) - want).abs())
                    .then(a.pts.cmp(&b.pts))
            }),
            SeekMode::AtOrBefore => candidates
                .filter(|e| value(e.pts) <= want)
                .max_by_key(|e| (e.pts, e.offset)),
            SeekMode::AtOrAfter => candidates
                .filter(|e| value(e.pts) >= want)
                .min_by_key(|e| (e.pts, e.offset)),
            _ => return Err(SessionError::Unsupported(UnsupportedMedia::Seek)),
        };
        chosen
            .copied()
            .ok_or(SessionError::Codec(WebmError::NoSuchFrame))
    }

    /// Resume demuxing at `e`; every track's ordinals continue from there.
    fn position(&mut self, e: SeekEntry) -> SeekResult {
        self.pos = e.offset;
        self.cluster = Some(Cluster {
            end: e.cluster_end,
            timestamp: Some(e.cluster_ts),
        });
        self.pending.clear();
        for (t, ord) in self.ordinals.iter_mut().enumerate() {
            *ord = self
                .index
                .iter()
                .find(|x| x.track == t && x.offset >= e.offset)
                .map_or_else(
                    || {
                        self.index
                            .iter()
                            .filter(|x| x.track == t)
                            .map(|x| x.frames)
                            .sum()
                    },
                    |x| x.ordinal,
                );
        }
        self.state = State::Body;
        SeekResult::new(
            Timestamp::new(e.pts, self.time_base),
            e.ordinal,
            if e.key {
                RandomAccess::Yes
            } else {
                RandomAccess::No
            },
            0,
        )
    }

    fn resolve_seek(&mut self) -> Res<DemuxProgress> {
        let target = self.pending_seek.take().expect("a scan serves a seek");
        let e = self.pick(&target)?;
        let r = self.position(e);
        Ok(DemuxProgress::new(0, DemuxEvent::Seeked(r)))
    }

    fn read_info(&mut self, body: &[u8]) -> Res<()> {
        for child in ebml::children(body) {
            let (cid, p) = child.map_err(SessionError::Codec)?;
            if cid == id::TIMESTAMP_SCALE {
                let scale = ebml::uint(p).map_err(SessionError::Codec)?;
                let scale = u32::try_from(scale)
                    .ok()
                    .filter(|&s| s > 0)
                    .ok_or(SessionError::Codec(WebmError::BadTimestampScale))?;
                self.scale_ns = scale;
                self.time_base = TimeBase::new(scale, NS_PER_SECOND)
                    .map_err(|_| SessionError::Codec(WebmError::BadTimestampScale))?;
            }
        }
        Ok(())
    }

    fn send_headers(&mut self) -> Res<()> {
        let raw = self
            .raw_tracks
            .take()
            .ok_or(SessionError::Codec(WebmError::NoTracks))?;
        let mut tracks = Vec::with_capacity(raw.len());
        for (i, t) in raw.iter().enumerate() {
            tracks.push(build_track(i as u32, t, self.time_base)?);
        }
        self.numbers = raw.iter().map(|t| t.number).collect();
        self.ordinals = vec![0; tracks.len()];
        self.tracks = tracks;
        self.headers_sent = true;
        Ok(())
    }

    fn block_group(&mut self, body: &[u8], body_offset: u64) -> Res<()> {
        let mut block = None;
        let mut extras = GroupExtras::default();
        let mut offset = 0usize;
        for child in ebml::children(body) {
            let (cid, p) = child.map_err(SessionError::Codec)?;
            // `p` is a subslice of `body`: its offset is the pointer distance.
            let at = p.as_ptr() as usize - body.as_ptr() as usize;
            match cid {
                id::BLOCK => {
                    block = Some(p);
                    offset = at;
                }
                id::BLOCK_DURATION => {
                    extras.duration = Some(ebml::uint(p).map_err(SessionError::Codec)?);
                }
                id::REFERENCE_BLOCK => extras.referenced = true,
                id::DISCARD_PADDING => {
                    extras.discard_padding = Some(ebml::int(p).map_err(SessionError::Codec)?);
                }
                id::BLOCK_ADDITIONS => {
                    for more in ebml::children(p) {
                        let (mid, mp) = more.map_err(SessionError::Codec)?;
                        if mid != id::BLOCK_MORE {
                            continue;
                        }
                        // BlockAddID defaults to 1.
                        let (mut add_id, mut data) = (1, None);
                        for f in ebml::children(mp) {
                            let (fid, fp) = f.map_err(SessionError::Codec)?;
                            match fid {
                                id::BLOCK_ADD_ID => {
                                    add_id = ebml::uint(fp).map_err(SessionError::Codec)?;
                                }
                                id::BLOCK_ADDITIONAL => data = Some(fp),
                                _ => {}
                            }
                        }
                        if let Some(d) = data {
                            extras.additions.push((add_id, d.to_vec()));
                        }
                    }
                }
                _ => {}
            }
        }
        let block = block.ok_or(SessionError::Codec(WebmError::BadBlock))?;
        self.block(block, body_offset + offset as u64, false, extras)
    }

    /// Decode a SimpleBlock or Block into packets on `pending`.
    fn block(
        &mut self,
        body: &[u8],
        body_offset: u64,
        simple: bool,
        extras: GroupExtras,
    ) -> Res<()> {
        let bad = SessionError::Codec(WebmError::BadBlock);
        let (number, nlen) = ebml::vint(body, 8, false)
            .map_err(SessionError::Codec)?
            .ok_or(bad)?;
        if body.len() < nlen + 3 {
            return err(WebmError::BadBlock);
        }
        let rel = i16::from_be_bytes([body[nlen], body[nlen + 1]]);
        let flags = body[nlen + 2];
        let track = self
            .numbers
            .iter()
            .position(|&n| n == number)
            .ok_or(SessionError::Codec(WebmError::UndeclaredTrack))?;
        let base = self
            .cluster
            .as_ref()
            .and_then(|c| c.timestamp)
            .ok_or(SessionError::Codec(WebmError::BlockBeforeTimestamp))?;
        let pts = base
            .checked_add(i64::from(rel))
            .ok_or(SessionError::Codec(WebmError::TimestampOverflow))?;
        let random_access = match (simple, flags & 0x80 != 0, extras.referenced) {
            (true, true, _) | (false, _, false) => RandomAccess::Yes,
            _ => RandomAccess::No,
        };
        let data = &body[nlen + 3..];
        let data_offset = body_offset + (nlen + 3) as u64;
        let frames = unlace(data, (flags >> 1) & 3)?;
        let n = frames.len();
        let discard = |ns: i64| {
            FrameDuration::new(ns.unsigned_abs(), NS_PER_SECOND)
                .map_err(|_| SessionError::Codec(WebmError::TimestampOverflow))
        };
        for (i, (start, len)) in frames.into_iter().enumerate() {
            if len == 0 {
                return err(WebmError::EmptyFrame);
            }
            self.limits
                .check_packet_bytes(len as u64)
                .map_err(SessionError::Limit)?;
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(len)
                .map_err(|_| SessionError::Limit(alloc_refused(len)))?;
            bytes.extend_from_slice(&data[start..start + len]);
            let ordinal = self.ordinals[track];
            self.ordinals[track] += 1;
            let mut p = Packet::from_vec(bytes)
                .map_err(|_| SessionError::Codec(WebmError::EmptyFrame))?
                .with_track(track as u32)
                .with_ordinal(ordinal)
                .with_input_offset(data_offset + start as u64)
                .with_random_access(random_access);
            // Later frames of a lace have no recorded timestamp.
            if i == 0 {
                p = p.with_pts(Timestamp::new(pts, self.time_base));
                for (add_id, d) in &extras.additions {
                    let add_id = u32::try_from(*add_id)
                        .map_err(|_| SessionError::Codec(WebmError::BadInteger))?;
                    p = p.with_addition(PacketAddition::new(
                        add_id,
                        PacketPayload::Owned(d.clone()),
                    ));
                }
            }
            if n == 1
                && let Some(d) = extras.duration
            {
                p = p.with_duration_ticks(d);
            }
            match extras.discard_padding {
                // Positive padding trims the end of the block, negative
                // its start.
                Some(ns) if ns > 0 && i == n - 1 => p = p.with_discard(Discard::end(discard(ns)?)),
                Some(ns) if ns < 0 && i == 0 => p = p.with_discard(Discard::start(discard(ns)?)),
                _ => {}
            }
            self.pending.push_back(p);
        }
        Ok(())
    }
}

fn alloc_refused(len: usize) -> MediaLimitExceeded {
    MediaLimitExceeded::check(zencodec::LimitKind::Memory, len as u64, 0)
        .expect_err("a zero limit refuses any allocation")
}

/// Frame (offset, length) pairs within a block's data, by lacing mode.
fn unlace(data: &[u8], lacing: u8) -> Res<Vec<(usize, usize)>> {
    let bad = || SessionError::Codec(WebmError::BadBlock);
    if lacing == 0 {
        return Ok(vec![(0, data.len())]);
    }
    let (&count, mut rest) = data.split_first().ok_or_else(bad)?;
    let n = usize::from(count) + 1;
    let mut sizes = Vec::with_capacity(n);
    match lacing {
        // Xiph: each size as a run of 255s and a final byte.
        1 => {
            for _ in 1..n {
                let mut size = 0usize;
                loop {
                    let (&b, r) = rest.split_first().ok_or_else(bad)?;
                    rest = r;
                    size += usize::from(b);
                    if b != 255 {
                        break;
                    }
                }
                sizes.push(size);
            }
        }
        // Fixed: equal sizes.
        2 => {
            if rest.len() % n != 0 {
                return Err(bad());
            }
            sizes.extend(core::iter::repeat_n(rest.len() / n, n - 1));
        }
        // EBML: the first size, then signed differences.
        _ => {
            let mut size = 0i64;
            for k in 1..n {
                let (v, len) = ebml::vint(rest, 8, false)
                    .map_err(SessionError::Codec)?
                    .ok_or_else(bad)?;
                rest = &rest[len..];
                size = if k == 1 {
                    i64::try_from(v).map_err(|_| bad())?
                } else {
                    let bias = (1i64 << (7 * len - 1)) - 1;
                    size + (v as i64 - bias)
                };
                sizes.push(usize::try_from(size).map_err(|_| bad())?);
            }
        }
    }
    let header = data.len() - rest.len();
    let used: usize = sizes
        .iter()
        .try_fold(0usize, |a, &s| a.checked_add(s))
        .ok_or_else(bad)?;
    let last = rest.len().checked_sub(used).ok_or_else(bad)?;
    sizes.push(last);
    let mut at = header;
    Ok(sizes
        .into_iter()
        .map(|s| {
            let f = (at, s);
            at += s;
            f
        })
        .collect())
}

fn read_tracks(body: &[u8], limits: &MediaLimits) -> Res<Vec<RawTrack>> {
    let mut out: Vec<RawTrack> = Vec::new();
    for child in ebml::children(body) {
        let (cid, p) = child.map_err(SessionError::Codec)?;
        if cid != id::TRACK_ENTRY {
            continue;
        }
        limits
            .check_tracks(out.len() as u64 + 1)
            .map_err(SessionError::Limit)?;
        let t = read_track(p)?;
        if t.number == 0 || out.iter().any(|o| o.number == t.number) {
            return err(WebmError::BadTrackNumber);
        }
        out.push(t);
    }
    if out.is_empty() {
        return err(WebmError::NoTracks);
    }
    Ok(out)
}

fn read_track(body: &[u8]) -> Res<RawTrack> {
    let c = SessionError::Codec;
    let mut t = RawTrack::default();
    for child in ebml::children(body) {
        let (cid, p) = child.map_err(c)?;
        match cid {
            id::TRACK_NUMBER => t.number = ebml::uint(p).map_err(c)?,
            id::TRACK_TYPE => t.kind = ebml::uint(p).map_err(c)?,
            id::CODEC_ID => {
                let s = core::str::from_utf8(p).map_err(|_| c(WebmError::UnsupportedCodec))?;
                t.codec = String::from(s.trim_end_matches('\0'));
            }
            id::CODEC_PRIVATE => t.private = Some(p.to_vec()),
            id::CODEC_DELAY => t.codec_delay_ns = ebml::uint(p).map_err(c)?,
            id::SEEK_PRE_ROLL => t.seek_preroll_ns = ebml::uint(p).map_err(c)?,
            id::TRACK_TIMESTAMP_SCALE => {
                if ebml::float(p).map_err(c)? != 1.0 {
                    return err(WebmError::TrackTimestampScale);
                }
            }
            id::CONTENT_ENCODINGS => return err(WebmError::ContentEncoding),
            id::VIDEO => t.video = Some(read_video(p)?),
            id::AUDIO => {
                // SamplingFrequency defaults to 8000, Channels to 1.
                let (mut rate, mut channels) = (8000.0, 1);
                for a in ebml::children(p) {
                    let (aid, ap) = a.map_err(c)?;
                    match aid {
                        id::SAMPLING_FREQUENCY => rate = ebml::float(ap).map_err(c)?,
                        id::CHANNELS => channels = ebml::uint(ap).map_err(c)?,
                        _ => {}
                    }
                }
                t.audio = Some((rate, channels));
            }
            _ => {}
        }
    }
    Ok(t)
}

fn read_video(body: &[u8]) -> Res<RawVideo> {
    let c = SessionError::Codec;
    let mut v = RawVideo::default();
    for child in ebml::children(body) {
        let (cid, p) = child.map_err(c)?;
        match cid {
            id::PIXEL_WIDTH => v.width = ebml::uint(p).map_err(c)?,
            id::PIXEL_HEIGHT => v.height = ebml::uint(p).map_err(c)?,
            id::DISPLAY_WIDTH => v.display.0 = Some(ebml::uint(p).map_err(c)?),
            id::DISPLAY_HEIGHT => v.display.1 = Some(ebml::uint(p).map_err(c)?),
            id::DISPLAY_UNIT => v.display_unit = ebml::uint(p).map_err(c)?,
            id::COLOUR => {
                // Each code defaults to 2 (unspecified); Range to 0.
                let mut codes = [2, 2, 2, 0];
                let mut any = false;
                for k in ebml::children(p) {
                    let (kid, kp) = k.map_err(c)?;
                    let slot = match kid {
                        id::PRIMARIES => 0,
                        id::TRANSFER_CHARACTERISTICS => 1,
                        id::MATRIX_COEFFICIENTS => 2,
                        id::RANGE => 3,
                        _ => continue,
                    };
                    codes[slot] = ebml::uint(kp).map_err(c)?;
                    any = true;
                }
                if any {
                    v.color = Some(codes);
                }
            }
            _ => {}
        }
    }
    Ok(v)
}

fn build_track(index: u32, t: &RawTrack, tb: TimeBase) -> Res<TrackInfo> {
    let c = SessionError::Codec;
    let codec = codec_from_id(&t.codec);
    let mut b = match (t.kind, &t.video, t.audio) {
        (1, Some(v), _) => {
            let (Ok(w), Ok(h)) = (u32::try_from(v.width), u32::try_from(v.height)) else {
                return err(WebmError::BadVideoSize);
            };
            let mut info = VideoTrackInfo::new(w, h);
            // Display size in pixels only (DisplayUnit 0).
            if let ((Some(dw), Some(dh)), 0) = (v.display, v.display_unit)
                && let (Ok(dw), Ok(dh)) = (u32::try_from(dw), u32::try_from(dh))
            {
                info = info.with_display(dw, dh);
            }
            if let Some([p, tr, m, r]) = v.color {
                let code = |x: u64| u8::try_from(x).map_err(|_| c(WebmError::BadInteger));
                // Range 2 is full; 1 is broadcast; 0 (unspecified) and 3
                // (from the codes) are read as not full.
                info = info.with_color(Cicp::new(code(p)?, code(tr)?, code(m)?, r == 2));
            }
            TrackInfo::video(index, codec, tb, info)
        }
        (1, None, _) => return err(WebmError::BadVideoSize),
        (2, _, audio) => {
            let (rate, channels) = audio.unwrap_or((8000.0, 1));
            let integral =
                rate >= 1.0 && rate <= f64::from(u32::MAX) && rate == f64::from(rate as u32);
            let channels = u16::try_from(channels).map_err(|_| c(WebmError::BadInteger))?;
            if !integral {
                return err(WebmError::BadAudio);
            }
            let info = AudioTrackInfo::new(rate as u32, ChannelLayout::new(channels));
            TrackInfo::audio(index, codec, tb, info)
        }
        (17, _, _) => TrackInfo::other(index, TrackKind::Subtitle, codec, tb),
        _ => TrackInfo::other(index, TrackKind::Data, codec, tb),
    };
    if let Some(p) = &t.private {
        b = b.with_codec_private(Arc::from(p.as_slice()));
    }
    let ns =
        |v: u64| FrameDuration::new(v, NS_PER_SECOND).map_err(|_| c(WebmError::TimestampOverflow));
    if t.codec_delay_ns > 0 {
        b = b.with_codec_delay(ns(t.codec_delay_ns)?);
    }
    if t.seek_preroll_ns > 0 {
        b = b.with_seek_preroll(ns(t.seek_preroll_ns)?);
    }
    b.build().map_err(|e| {
        c(match e {
            zencodec_media::track::TrackError::ZeroDimension => WebmError::BadVideoSize,
            _ => WebmError::BadAudio,
        })
    })
}

impl Demuxer for WebmDemuxer {
    type Error = WebmError;

    fn step(&mut self, input: Input<'_>, pulse: &dyn zencodec::Pulse) -> Res<DemuxProgress> {
        pulse.check()?;
        self.requests.clear();
        if input.total_len.is_some() {
            self.total = input.total_len;
        }
        if let Some(p) = self.pending.pop_front() {
            return Ok(DemuxProgress::new(0, DemuxEvent::Packet(p)));
        }
        match self.state {
            State::Ebml => self.ebml_header(input),
            State::Segment => self.segment_header(input),
            State::Body => self.body_step(input),
            State::Scanning => self.scan_step(input),
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
        AccessPattern::Sequential
    }

    /// Seek by time. The first seek scans every element header from the
    /// start of the Segment with range requests (block payloads are not
    /// read; a BlockGroup is, for its ReferenceBlock), indexing each
    /// block's track, timestamp, key flag and per-track ordinal; later
    /// seeks use the index. Demuxing resumes at the chosen block, whose
    /// first frame is the target track's next packet.
    fn seek(&mut self, target: &SeekTarget) -> Res<SeekStatus> {
        if !self.headers_sent {
            return err(WebmError::HeadersFirst);
        }
        if target.track as usize >= self.tracks.len() {
            return err(WebmError::NoSuchFrame);
        }
        if self.index_complete {
            let e = self.pick(target)?;
            return Ok(SeekStatus::Positioned(self.position(e)));
        }
        self.pending_seek = Some(*target);
        self.pending.clear();
        self.scan = Some(Scan {
            pos: self.segment_data,
            cluster: None,
            ordinals: vec![0; self.tracks.len()],
        });
        self.state = State::Scanning;
        Ok(SeekStatus::Scanning)
    }
}
