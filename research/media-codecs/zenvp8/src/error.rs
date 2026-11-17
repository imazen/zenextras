//! Error type for the VP8 decoder.

use core::fmt;

/// All recoverable decode failures.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// Frame payload too short to contain a frame tag / header.
    NotEnoughData,
    /// Keyframe magic bytes (0x9d 0x2a) or version reserved bits were invalid.
    InvalidSignature,
    /// Declared dimensions are zero, exceed limits, or regressed illegally.
    InvalidDimensions,
    /// Bool decoder consumed beyond the tolerated overrun (libvpx
    /// `vp8dx_bool_error` equivalent).
    BitstreamCorrupt,
    /// A token partition declared size ran past the packet end.
    PartitionTruncated,
    /// A non-keyframe arrived before any complete keyframe.
    MissingKeyframe,
    /// First partition too short to contain the mode/mv data.
    Partition0TooShort,
    /// Allocation size sanity limit exceeded.
    TooLarge,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::NotEnoughData => "packet too short",
            Self::InvalidSignature => "invalid keyframe signature",
            Self::InvalidDimensions => "invalid frame dimensions",
            Self::BitstreamCorrupt => "bool decoder read past end of input",
            Self::PartitionTruncated => "token partition overruns packet",
            Self::MissingKeyframe => "inter frame before first keyframe",
            Self::Partition0TooShort => "first partition undersized",
            Self::TooLarge => "frame dimensions exceed allocation limit",
        };
        f.write_str(s)
    }
}

impl std::error::Error for DecodeError {}
