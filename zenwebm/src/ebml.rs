//! EBML primitives over byte slices: element headers, integer and float
//! payloads, and the writers the muxer uses.
//!
//! An element is an ID, a size and a payload. ID and size share one
//! variable-length encoding: the leading zero bits of the first byte give
//! the field's length. IDs keep their length marker (`0x1A45DFA3`); sizes
//! drop it, and a size with every value bit set means "unknown" (only
//! legal for Segment and Cluster in WebM).

use alloc::vec::Vec;

use crate::WebmError;

/// Longest element ID WebM allows (`EBMLMaxIDLength` 4).
pub(crate) const MAX_ID_LEN: usize = 4;
/// Longest size field (`EBMLMaxSizeLength` 8).
pub(crate) const MAX_SIZE_LEN: usize = 8;
/// The most bytes an element header can take.
pub(crate) const MAX_HEADER_LEN: usize = MAX_ID_LEN + MAX_SIZE_LEN;

/// An element's ID, its payload size (`None` when unknown) and the bytes
/// its header took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub id: u32,
    pub size: Option<u64>,
    pub len: usize,
}

impl Header {
    /// The element's total length, when its size is known.
    pub fn total(&self) -> Option<u64> {
        self.size.map(|s| s + self.len as u64)
    }
}

/// Read a variable-length integer at the start of `b`: its value (marker
/// removed unless `keep_marker`) and length. `Ok(None)` when `b` is too
/// short to hold it.
pub(crate) fn vint(
    b: &[u8],
    max_len: usize,
    keep_marker: bool,
) -> Result<Option<(u64, usize)>, WebmError> {
    let Some(&first) = b.first() else {
        return Ok(None);
    };
    let len = first.leading_zeros() as usize + 1;
    if first == 0 || len > max_len {
        return Err(WebmError::BadVint);
    }
    if b.len() < len {
        return Ok(None);
    }
    let mut v = if keep_marker {
        u64::from(first)
    } else {
        u64::from(first) & (0xFF >> len)
    };
    for &x in &b[1..len] {
        v = (v << 8) | u64::from(x);
    }
    Ok(Some((v, len)))
}

/// Parse the element header at the start of `b`; `Ok(None)` when `b`
/// ends inside it.
pub(crate) fn header(b: &[u8]) -> Result<Option<Header>, WebmError> {
    let Some((id, id_len)) = vint(b, MAX_ID_LEN, true)? else {
        return Ok(None);
    };
    let Some((raw, size_len)) = vint(&b[id_len..], MAX_SIZE_LEN, false)? else {
        return Ok(None);
    };
    let unknown = raw == (1u64 << (7 * size_len)) - 1;
    Ok(Some(Header {
        id: id as u32,
        size: (!unknown).then_some(raw),
        len: id_len + size_len,
    }))
}

/// The children of a master element's payload, each as (ID, payload).
pub(crate) fn children(body: &[u8]) -> Children<'_> {
    Children { rest: body }
}

pub(crate) struct Children<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Children<'a> {
    type Item = Result<(u32, &'a [u8]), WebmError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let r = match header(self.rest) {
            Ok(Some(Header {
                id,
                size: Some(size),
                len,
            })) => match usize::try_from(size)
                .ok()
                .and_then(|s| s.checked_add(len))
                .filter(|&end| end <= self.rest.len())
            {
                Some(end) => {
                    let body = &self.rest[len..end];
                    self.rest = &self.rest[end..];
                    Ok((id, body))
                }
                None => Err(WebmError::Overrun),
            },
            Ok(Some(_)) => Err(WebmError::UnknownSize),
            Ok(None) => Err(WebmError::Overrun),
            Err(e) => Err(e),
        };
        if r.is_err() {
            self.rest = &[];
        }
        Some(r)
    }
}

/// An unsigned integer payload (0 to 8 bytes, big-endian).
pub(crate) fn uint(b: &[u8]) -> Result<u64, WebmError> {
    if b.len() > 8 {
        return Err(WebmError::BadInteger);
    }
    Ok(b.iter().fold(0, |v, &x| (v << 8) | u64::from(x)))
}

/// A signed integer payload (0 to 8 bytes, two's complement).
pub(crate) fn int(b: &[u8]) -> Result<i64, WebmError> {
    if b.len() > 8 {
        return Err(WebmError::BadInteger);
    }
    let neg = b.first().is_some_and(|&x| x & 0x80 != 0);
    Ok(b.iter()
        .fold(if neg { -1i64 } else { 0 }, |v, &x| (v << 8) | i64::from(x)))
}

/// A float payload: 0 (zero), 4 or 8 bytes.
pub(crate) fn float(b: &[u8]) -> Result<f64, WebmError> {
    match b.len() {
        0 => Ok(0.0),
        4 => Ok(f64::from(f32::from_be_bytes([b[0], b[1], b[2], b[3]]))),
        8 => {
            let mut a = [0u8; 8];
            a.copy_from_slice(b);
            Ok(f64::from_be_bytes(a))
        }
        _ => Err(WebmError::BadFloat),
    }
}

/// Element IDs this crate reads or writes.
pub(crate) mod id {
    pub const EBML: u32 = 0x1A45_DFA3;
    pub const EBML_VERSION: u32 = 0x4286;
    pub const EBML_READ_VERSION: u32 = 0x42F7;
    pub const EBML_MAX_ID_LENGTH: u32 = 0x42F2;
    pub const EBML_MAX_SIZE_LENGTH: u32 = 0x42F3;
    pub const DOC_TYPE: u32 = 0x4282;
    pub const DOC_TYPE_VERSION: u32 = 0x4287;
    pub const DOC_TYPE_READ_VERSION: u32 = 0x4285;
    pub const VOID: u32 = 0xEC;
    pub const CRC32: u32 = 0xBF;

    pub const SEGMENT: u32 = 0x1853_8067;
    pub const SEEK_HEAD: u32 = 0x114D_9B74;
    pub const INFO: u32 = 0x1549_A966;
    pub const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
    pub const MUXING_APP: u32 = 0x4D80;
    pub const WRITING_APP: u32 = 0x5741;
    pub const TRACKS: u32 = 0x1654_AE6B;
    pub const CLUSTER: u32 = 0x1F43_B675;
    pub const CUES: u32 = 0x1C53_BB6B;
    pub const TAGS: u32 = 0x1254_C367;
    pub const CHAPTERS: u32 = 0x1043_A770;
    pub const ATTACHMENTS: u32 = 0x1941_A469;

    pub const TRACK_ENTRY: u32 = 0xAE;
    pub const TRACK_NUMBER: u32 = 0xD7;
    pub const TRACK_UID: u32 = 0x73C5;
    pub const TRACK_TYPE: u32 = 0x83;
    pub const FLAG_LACING: u32 = 0x9C;
    pub const CODEC_ID: u32 = 0x86;
    pub const CODEC_PRIVATE: u32 = 0x63A2;
    pub const CODEC_DELAY: u32 = 0x56AA;
    pub const SEEK_PRE_ROLL: u32 = 0x56BB;
    pub const TRACK_TIMESTAMP_SCALE: u32 = 0x0023_314F;
    pub const CONTENT_ENCODINGS: u32 = 0x6D80;
    pub const VIDEO: u32 = 0xE0;
    pub const PIXEL_WIDTH: u32 = 0xB0;
    pub const PIXEL_HEIGHT: u32 = 0xBA;
    pub const DISPLAY_WIDTH: u32 = 0x54B0;
    pub const DISPLAY_HEIGHT: u32 = 0x54BA;
    pub const DISPLAY_UNIT: u32 = 0x54B2;
    pub const COLOUR: u32 = 0x55B0;
    pub const MATRIX_COEFFICIENTS: u32 = 0x55B1;
    pub const RANGE: u32 = 0x55B9;
    pub const TRANSFER_CHARACTERISTICS: u32 = 0x55BA;
    pub const PRIMARIES: u32 = 0x55BB;
    pub const AUDIO: u32 = 0xE1;
    pub const SAMPLING_FREQUENCY: u32 = 0xB5;
    pub const CHANNELS: u32 = 0x9F;

    pub const TIMESTAMP: u32 = 0xE7;
    pub const SIMPLE_BLOCK: u32 = 0xA3;
    pub const BLOCK_GROUP: u32 = 0xA0;
    pub const BLOCK: u32 = 0xA1;
    pub const BLOCK_ADDITIONS: u32 = 0x75A1;
    pub const BLOCK_MORE: u32 = 0xA6;
    pub const BLOCK_ADD_ID: u32 = 0xEE;
    pub const BLOCK_ADDITIONAL: u32 = 0xA5;
    pub const BLOCK_DURATION: u32 = 0x9B;
    pub const REFERENCE_BLOCK: u32 = 0xFB;
    pub const DISCARD_PADDING: u32 = 0x75A2;

    /// Children of Segment. In a Cluster of unknown size, one of these ends
    /// the Cluster.
    pub const fn is_top_level(id: u32) -> bool {
        matches!(
            id,
            SEEK_HEAD | INFO | TRACKS | CLUSTER | CUES | TAGS | CHAPTERS | ATTACHMENTS
        )
    }
}

/// Append an element ID (its natural width: the marker bit's position).
pub(crate) fn put_id(out: &mut Vec<u8>, id: u32) {
    let len = (4 - id.leading_zeros() as usize / 8).max(1);
    out.extend_from_slice(&id.to_be_bytes()[4 - len..]);
}

/// Append a size field at the smallest width that is not the
/// unknown-size pattern.
pub(crate) fn put_size(out: &mut Vec<u8>, size: u64) {
    let mut len = 1;
    while len < MAX_SIZE_LEN && size >= (1u64 << (7 * len)) - 1 {
        len += 1;
    }
    let field = (1u64 << (7 * len)) | size;
    out.extend_from_slice(&field.to_be_bytes()[8 - len..]);
}

/// The 8-byte unknown size.
pub(crate) fn put_unknown_size(out: &mut Vec<u8>) {
    out.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
}

/// Append a whole element.
pub(crate) fn put(out: &mut Vec<u8>, id: u32, payload: &[u8]) {
    put_id(out, id);
    put_size(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

/// Append an unsigned-integer element at its minimal width (one byte for
/// zero).
pub(crate) fn put_uint(out: &mut Vec<u8>, id: u32, v: u64) {
    let b = v.to_be_bytes();
    let skip = (v.leading_zeros() as usize / 8).min(7);
    put(out, id, &b[skip..]);
}

/// Append a signed-integer element at its minimal two's-complement width.
pub(crate) fn put_int(out: &mut Vec<u8>, id: u32, v: i64) {
    let b = v.to_be_bytes();
    let mut skip = 0;
    while skip < 7
        && ((b[skip] == 0 && b[skip + 1] & 0x80 == 0)
            || (b[skip] == 0xFF && b[skip + 1] & 0x80 != 0))
    {
        skip += 1;
    }
    put(out, id, &b[skip..]);
}

/// Append an 8-byte float element.
pub(crate) fn put_float(out: &mut Vec<u8>, id: u32, v: f64) {
    put(out, id, &v.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_round_trip_and_avoid_the_unknown_pattern() {
        for size in [0, 1, 126, 127, 128, 16_382, 16_383, 16_384, 1 << 40] {
            let mut b = Vec::new();
            put_size(&mut b, size);
            let (v, len) = vint(&b, MAX_SIZE_LEN, false).unwrap().unwrap();
            assert_eq!((v, len), (size, b.len()), "{size}");
            assert_ne!(v, (1u64 << (7 * len)) - 1);
        }
    }

    #[test]
    fn ids_keep_their_width() {
        for id in [id::EBML, id::EBML_VERSION, id::VOID, id::TIMESTAMP_SCALE] {
            let mut b = Vec::new();
            put_id(&mut b, id);
            assert_eq!(
                vint(&b, MAX_ID_LEN, true).unwrap(),
                Some((u64::from(id), b.len()))
            );
        }
    }

    #[test]
    fn integers_round_trip_at_minimal_width() {
        for v in [0i64, 1, -1, 127, 128, -128, -129, i64::MAX, i64::MIN] {
            let mut b = Vec::new();
            put_int(&mut b, id::DISCARD_PADDING, v);
            let h = header(&b).unwrap().unwrap();
            assert_eq!(int(&b[h.len..]).unwrap(), v);
        }
        for v in [0u64, 255, 256, u64::MAX] {
            let mut b = Vec::new();
            put_uint(&mut b, id::TIMESTAMP, v);
            let h = header(&b).unwrap().unwrap();
            assert_eq!(uint(&b[h.len..]).unwrap(), v);
            assert_eq!(
                h.size,
                Some(((64 - v.leading_zeros() as u64).div_ceil(8)).max(1))
            );
        }
    }

    #[test]
    fn unknown_sizes_and_short_input() {
        let mut b = Vec::new();
        put_id(&mut b, id::CLUSTER);
        put_unknown_size(&mut b);
        assert_eq!(header(&b).unwrap().unwrap().size, None);
        assert_eq!(header(&b[..5]).unwrap(), None);
        assert_eq!(header(&[0x00]), Err(WebmError::BadVint));
        assert_eq!(header(&[0x08, 0, 0, 0, 0x81]), Err(WebmError::BadVint));
    }
}
