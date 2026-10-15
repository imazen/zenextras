//! WebM (and the Matroska subset WebM players read) as a sans-I/O
//! [`Demuxer`](zencodec_media::container::Demuxer) and a pull-output
//! [`Muxer`](zencodec_media::container::Muxer) for `zencodec-media`.
//!
//! A WebM file is EBML: an `EBML` header naming the document type, then a
//! `Segment` holding `Info` (the tick length), `Tracks`, and `Cluster`s of
//! blocks, each block a packet (or several, laced) with a timestamp relative
//! to its cluster's. Segments and clusters may have unknown size, which is
//! how live encoders write them.
//!
//! - [`WebmDemuxer`] reads it from any `zencodec-io` driver, one element per
//!   step, skipping what it does not need by asking for the bytes after it.
//!   Packets carry what the file records: timestamps on the segment's tick
//!   clock (missing for the second and later frames of a laced block), the
//!   random-access flag (`SimpleBlock` keyframe bit, or a `BlockGroup`
//!   without `ReferenceBlock`), `BlockDuration`, `DiscardPadding` as
//!   [`Discard`](zencodec_media::packet::Discard), and `BlockAdditions` as
//!   [`PacketAddition`](zencodec_media::packet::PacketAddition)s. Tracks
//!   carry `CodecPrivate`, `CodecDelay`, `SeekPreRoll`, display size and the
//!   `Colour` element's CICP codes. Compressed or encrypted tracks
//!   (`ContentEncodings`) are refused rather than passed through.
//! - [`WebmMuxer`] writes the live profile: unknown-size `Segment`, one
//!   cluster at a time, each cluster's bytes pending as soon as it closes
//!   (at each video random-access point, and at the configured length), no
//!   patches, no index. The document type is `webm` when every codec is a
//!   WebM codec, `matroska` otherwise.
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod demux;
mod ebml;
mod mux;

use core::fmt;

use zencodec_media::track::{CodecId, CodecTag};

pub use demux::WebmDemuxer;
pub use mux::{MuxReport, WebmMuxer};

/// Malformed or unrepresentable WebM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WebmError {
    /// The file does not start with an EBML header.
    NotEbml,
    /// The EBML document type is neither `webm` nor `matroska`.
    UnsupportedDocType,
    /// A variable-length integer is malformed or longer than allowed.
    BadVint,
    /// An integer element longer than 8 bytes.
    BadInteger,
    /// A float element that is not 0, 4 or 8 bytes.
    BadFloat,
    /// An element of unknown size other than a Segment or Cluster.
    UnknownSize,
    /// An element extends past its parent.
    Overrun,
    /// The file ends inside an element.
    Truncated,
    /// No Segment after the EBML header.
    NoSegment,
    /// A Cluster before the Tracks element, or no Tracks at all.
    NoTracks,
    /// A second Tracks element.
    DuplicateTracks,
    /// A track number of zero, or one used twice.
    BadTrackNumber,
    /// A block names a track the Tracks element does not declare.
    UndeclaredTrack,
    /// A video track without nonzero pixel dimensions.
    BadVideoSize,
    /// An audio track with no channels or a sample rate that is not a
    /// positive whole number.
    BadAudio,
    /// `TimestampScale` is zero or does not fit 32 bits.
    BadTimestampScale,
    /// A `TrackTimestampScale` other than 1.0 (WebM forbids it).
    TrackTimestampScale,
    /// A track's payloads are compressed or encrypted (`ContentEncodings`).
    ContentEncoding,
    /// A block before its cluster's Timestamp.
    BlockBeforeTimestamp,
    /// A block shorter than its header, or a lace that does not fit.
    BadBlock,
    /// A laced frame with no bytes.
    EmptyFrame,
    /// A timestamp outside the signed 64-bit tick range.
    TimestampOverflow,
    /// A seek before the headers were read.
    HeadersFirst,
    /// A seek on a track the file does not have.
    NoSuchTrack,
    /// No packet satisfies the seek.
    NoSuchFrame,
    /// A codec this muxer cannot name in WebM or Matroska.
    UnsupportedCodec,
    /// A track kind other than video or audio.
    UnsupportedTrackKind,
    /// More tracks than one-byte track numbers allow (126).
    TooManyTracks,
    /// Tracks on clocks with no common exact tick length.
    MixedClocks,
    /// A timestamp the segment's tick length cannot represent exactly.
    InexactTimestamp,
    /// A timestamp before what a cluster can reach (-32768 ticks).
    NegativeTimestamp,
    /// A timestamp more than 32767 ticks before its cluster's start.
    OutOfOrder,
    /// A packet that needs trimming at both ends (WebM records one).
    DiscardBothEnds,
    /// A packet coded under a configuration epoch its track did not start in.
    EpochChange,
    /// A track with an edit-list offset, which WebM cannot record.
    EditList,
}

impl fmt::Display for WebmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotEbml => "not an EBML file",
            Self::UnsupportedDocType => "EBML document type is not webm or matroska",
            Self::BadVint => "malformed EBML variable-length integer",
            Self::BadInteger => "EBML integer longer than 8 bytes",
            Self::BadFloat => "EBML float is not 0, 4 or 8 bytes",
            Self::UnknownSize => "unknown size on an element other than Segment or Cluster",
            Self::Overrun => "EBML element extends past its parent",
            Self::Truncated => "WebM file ends inside an element",
            Self::NoSegment => "no Segment after the EBML header",
            Self::NoTracks => "no Tracks element before the first Cluster",
            Self::DuplicateTracks => "a second Tracks element",
            Self::BadTrackNumber => "track number is zero or repeated",
            Self::UndeclaredTrack => "block for a track the file does not declare",
            Self::BadVideoSize => "video track without pixel dimensions",
            Self::BadAudio => "audio track without channels or a whole sample rate",
            Self::BadTimestampScale => "TimestampScale is zero or over 32 bits",
            Self::TrackTimestampScale => "TrackTimestampScale other than 1.0",
            Self::ContentEncoding => "compressed or encrypted track (ContentEncodings)",
            Self::BlockBeforeTimestamp => "block before its cluster's Timestamp",
            Self::BadBlock => "malformed block or lace",
            Self::EmptyFrame => "laced frame with no bytes",
            Self::TimestampOverflow => "timestamp outside the 64-bit tick range",
            Self::HeadersFirst => "seek before the WebM headers were read",
            Self::NoSuchTrack => "no such track",
            Self::NoSuchFrame => "no packet satisfies the seek",
            Self::UnsupportedCodec => "codec has no WebM or Matroska codec ID",
            Self::UnsupportedTrackKind => "WebM carries video and audio tracks only",
            Self::TooManyTracks => "more than 126 tracks",
            Self::MixedClocks => "tracks have no common exact tick length",
            Self::InexactTimestamp => "timestamp is not a whole number of segment ticks",
            Self::NegativeTimestamp => "timestamp earlier than -32768 segment ticks",
            Self::OutOfOrder => "timestamp more than 32767 ticks before its cluster",
            Self::DiscardBothEnds => "WebM records discard padding at one end only",
            Self::EpochChange => "codec configuration changed mid-track",
            Self::EditList => "WebM cannot record an edit-list offset",
        })
    }
}

impl core::error::Error for WebmError {}

/// The codec a Matroska codec ID names.
pub fn codec_from_id(id: &str) -> CodecId {
    match id {
        "V_AV1" => CodecId::Av1,
        "V_VP9" => CodecId::Vp9,
        "V_VP8" => CodecId::Vp8,
        "V_MPEG4/ISO/AVC" => CodecId::H264,
        "V_MPEGH/ISO/HEVC" => CodecId::Hevc,
        "A_OPUS" => CodecId::Opus,
        "A_AAC" => CodecId::Aac,
        "A_FLAC" => CodecId::Flac,
        "A_PCM/INT/LIT" => CodecId::Pcm,
        other => CodecId::Other(CodecTag::new(other)),
    }
}

/// The Matroska codec ID for a codec, and whether WebM allows it.
pub fn codec_id_for(codec: &CodecId) -> Option<(&str, bool)> {
    Some(match codec {
        CodecId::Av1 => ("V_AV1", true),
        CodecId::Vp9 => ("V_VP9", true),
        CodecId::Vp8 => ("V_VP8", true),
        CodecId::Opus => ("A_OPUS", true),
        CodecId::H264 => ("V_MPEG4/ISO/AVC", false),
        CodecId::Hevc => ("V_MPEGH/ISO/HEVC", false),
        CodecId::Aac => ("A_AAC", false),
        CodecId::Flac => ("A_FLAC", false),
        CodecId::Pcm => ("A_PCM/INT/LIT", false),
        CodecId::Other(tag) => {
            let s = tag.as_str();
            if !(s.starts_with("V_") || s.starts_with("A_")) {
                return None;
            }
            (s, s == "A_VORBIS")
        }
        _ => return None,
    })
}
