//! `#![no_std]` VP9 uncompressed frame header and Annex B superframe parser,
//! preserving full `color_range`, `color_space`, `bit_depth`, subsampling,
//! coded/render geometry, and ISOBMFF v1 `vpcC` synthesis.

use alloc::vec::Vec;

/// VP9 bitstream `color_space` code (3-bit header field, VP9 spec §6.2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Vp9ColorSpace {
    /// Unknown / unspecified (`0`).
    Unknown = 0,
    /// ITU-R BT.601 (`1`).
    Bt601 = 1,
    /// ITU-R BT.709 (`2`).
    Bt709 = 2,
    /// SMPTE-170 (`3`).
    Smpte170 = 3,
    /// SMPTE-240 (`4`).
    Smpte240 = 4,
    /// ITU-R BT.2020 (`5`).
    Bt2020 = 5,
    /// Reserved (`6`).
    Reserved = 6,
    /// sRGB / identity RGB (`7`).
    Rgb = 7,
}

impl Vp9ColorSpace {
    /// Decode from the 3-bit VP9 header value.
    #[must_use]
    pub const fn from_code(code: u8) -> Self {
        match code & 7 {
            1 => Self::Bt601,
            2 => Self::Bt709,
            3 => Self::Smpte170,
            4 => Self::Smpte240,
            5 => Self::Bt2020,
            6 => Self::Reserved,
            7 => Self::Rgb,
            _ => Self::Unknown,
        }
    }

    /// Map to CICP `(primaries, transfer, matrix)`.
    #[must_use]
    pub const fn cicp_triplet(self) -> (u8, u8, u8) {
        match self {
            Self::Bt601 | Self::Smpte170 => (6, 6, 6),
            Self::Bt709 => (1, 1, 1),
            Self::Smpte240 => (7, 7, 7),
            Self::Bt2020 => (9, 14, 9),
            Self::Rgb => (1, 13, 0),
            Self::Unknown | Self::Reserved => (2, 2, 2),
        }
    }
}

/// VP9 chroma subsampling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Vp9Subsampling {
    /// 4:2:0 (`subsampling_x = 1, subsampling_y = 1`).
    Yuv420,
    /// 4:2:2 (`subsampling_x = 1, subsampling_y = 0`).
    Yuv422,
    /// 4:4:4 (`subsampling_x = 0, subsampling_y = 0`).
    Yuv444,
}

impl Vp9Subsampling {
    /// ISOBMFF VP Codec Binding v1 `chromaSubsampling` 3-bit code.
    #[must_use]
    pub const fn vpcc_code(self) -> u8 {
        match self {
            Self::Yuv420 => 0,
            Self::Yuv422 => 2,
            Self::Yuv444 => 3,
        }
    }
}

/// Validated VP9 codec configuration parsed from either ISOBMFF v1 `vpcC`
/// or WebM VP9 `CodecPrivate` feature-TLV entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vp9CodecConfig {
    /// VP9 profile (`0..=3`).
    pub profile: u8,
    /// VP9 level (`0` = undefined, or defined level code `10..=62`).
    pub level: u8,
    /// Sample bit depth (`8`, `10`, or `12`), if specified.
    pub bit_depth: Option<u8>,
    /// Chroma subsampling, if specified.
    pub subsampling: Option<Vp9Subsampling>,
    /// Exact raw chroma subsampling/siting code (`0` = 4:2:0 vertical, `1` = 4:2:0 colocated,
    /// `2` = 4:2:2, `3` = 4:4:4) preserved across `vpcC` and WebM `CodecPrivate` round-trips.
    pub chroma_code: Option<u8>,
    /// Full-range flag (`true` = full, `false` = studio/limited), if specified.
    pub full_range: Option<bool>,
    /// CICP `(primaries, transfer, matrix)`, if specified (from `vpcC`).
    pub cicp: Option<(u8, u8, u8)>,
}

const fn is_valid_vp9_level(level: u8) -> bool {
    matches!(
        level,
        0 | 10 | 11 | 20 | 21 | 30 | 31 | 40 | 41 | 50 | 51 | 52 | 60 | 61 | 62
    )
}

const fn is_valid_vp9_profile_combo(
    profile: u8,
    bit_depth: u8,
    subsampling: Vp9Subsampling,
    is_rgb: bool,
) -> bool {
    if is_rgb && (!matches!(subsampling, Vp9Subsampling::Yuv444) || profile == 0 || profile == 2) {
        return false;
    }
    match profile {
        0 => bit_depth == 8 && matches!(subsampling, Vp9Subsampling::Yuv420),
        1 => {
            bit_depth == 8 && matches!(subsampling, Vp9Subsampling::Yuv422 | Vp9Subsampling::Yuv444)
        }
        2 => matches!(bit_depth, 10 | 12) && matches!(subsampling, Vp9Subsampling::Yuv420),
        3 => {
            matches!(bit_depth, 10 | 12)
                && matches!(subsampling, Vp9Subsampling::Yuv422 | Vp9Subsampling::Yuv444)
        }
        _ => false,
    }
}

impl Vp9CodecConfig {
    /// Serialize as WebM VP9 `CodecPrivate` feature-TLV `(id, len, val)` bytes.
    #[must_use]
    pub fn to_webm_codec_private(self) -> Option<Vec<u8>> {
        if self.profile > 3 || !is_valid_vp9_level(self.level) {
            return None;
        }
        let is_rgb = self.cicp.is_some_and(|(_, _, m)| m == 0);
        if is_rgb && matches!(self.profile, 0 | 2) {
            return None;
        }
        if let Some(bd) = self.bit_depth {
            let depth_ok = if self.profile <= 1 {
                bd == 8
            } else {
                matches!(bd, 10 | 12)
            };
            if !depth_ok {
                return None;
            }
        }
        if let Some(sub) = self.subsampling {
            let sub_ok = if self.profile == 0 || self.profile == 2 {
                matches!(sub, Vp9Subsampling::Yuv420)
            } else {
                matches!(sub, Vp9Subsampling::Yuv422 | Vp9Subsampling::Yuv444)
            };
            if !sub_ok || (is_rgb && sub != Vp9Subsampling::Yuv444) {
                return None;
            }
        }
        if let (Some(bd), Some(sub)) = (self.bit_depth, self.subsampling)
            && !is_valid_vp9_profile_combo(self.profile, bd, sub, is_rgb)
        {
            return None;
        }
        let mut out = Vec::with_capacity(12);
        out.extend_from_slice(&[1, 1, self.profile]);
        if self.level != 0 {
            out.extend_from_slice(&[2, 1, self.level]);
        }
        if let Some(bd) = self.bit_depth {
            if !matches!(bd, 8 | 10 | 12) {
                return None;
            }
            out.extend_from_slice(&[3, 1, bd]);
        }
        if let Some(sub) = self.subsampling {
            let code = self.chroma_code.unwrap_or_else(|| sub.vpcc_code());
            let code_matches = matches!(
                (sub, code),
                (Vp9Subsampling::Yuv420, 0 | 1)
                    | (Vp9Subsampling::Yuv422, 2)
                    | (Vp9Subsampling::Yuv444, 3)
            );
            if !code_matches {
                return None;
            }
            out.extend_from_slice(&[4, 1, code]);
        }
        Some(out)
    }
}

/// Parse a canonical 12-byte ISOBMFF FullBox v1 `vpcC` record for VP9.
#[must_use]
pub fn parse_vpcc_v1(bytes: &[u8]) -> Option<Vp9CodecConfig> {
    if bytes.len() != 12 || bytes[0] != 1 || bytes[1..4] != [0, 0, 0] || bytes[10..12] != [0, 0] {
        return None;
    }
    let profile = bytes[4];
    let level = bytes[5];
    let bit_depth = bytes[6] >> 4;
    let chroma = (bytes[6] >> 1) & 0x07;
    let full_range = (bytes[6] & 1) != 0;
    let subsampling = match chroma {
        0 | 1 => Vp9Subsampling::Yuv420,
        2 => Vp9Subsampling::Yuv422,
        3 => Vp9Subsampling::Yuv444,
        _ => return None,
    };
    let cicp = (bytes[7], bytes[8], bytes[9]);
    if !is_valid_vp9_level(level)
        || !is_valid_vp9_profile_combo(profile, bit_depth, subsampling, cicp.2 == 0)
    {
        return None;
    }
    Some(Vp9CodecConfig {
        profile,
        level,
        bit_depth: Some(bit_depth),
        subsampling: Some(subsampling),
        chroma_code: Some(chroma),
        full_range: Some(full_range),
        cicp: Some(cicp),
    })
}

/// Parse WebM VP9 `CodecPrivate` feature-TLV entries (`(id, len=1, val)`).
#[must_use]
pub fn parse_webm_codec_private(bytes: &[u8]) -> Option<Vp9CodecConfig> {
    if bytes.is_empty() {
        return None;
    }
    let mut pos = 0usize;
    let mut profile = None;
    let mut level = 0u8;
    let mut bit_depth = None;
    let mut subsampling = None;
    let mut chroma_code = None;
    while pos < bytes.len() {
        if pos + 2 > bytes.len() {
            return None;
        }
        let id = bytes[pos];
        let len = usize::from(bytes[pos + 1]);
        pos += 2;
        if len != 1 || pos + len > bytes.len() {
            return None;
        }
        let val = bytes[pos];
        pos += 1;
        match id {
            1 if val <= 3 => profile = Some(val),
            2 if val != 0 && is_valid_vp9_level(val) => level = val,
            3 if matches!(val, 8 | 10 | 12) => bit_depth = Some(val),
            4 => {
                chroma_code = Some(val);
                subsampling = Some(match val {
                    0 | 1 => Vp9Subsampling::Yuv420,
                    2 => Vp9Subsampling::Yuv422,
                    3 => Vp9Subsampling::Yuv444,
                    _ => return None,
                });
            }
            _ => return None,
        }
    }
    let profile = profile?;
    if let (Some(bd), Some(sub)) = (bit_depth, subsampling)
        && !is_valid_vp9_profile_combo(profile, bd, sub, false)
    {
        return None;
    }
    if let Some(bd) = bit_depth {
        let depth_ok = if profile <= 1 {
            bd == 8
        } else {
            matches!(bd, 10 | 12)
        };
        if !depth_ok {
            return None;
        }
    }
    if let Some(sub) = subsampling {
        let sub_ok = if profile == 0 || profile == 2 {
            matches!(sub, Vp9Subsampling::Yuv420)
        } else {
            matches!(sub, Vp9Subsampling::Yuv422 | Vp9Subsampling::Yuv444)
        };
        if !sub_ok {
            return None;
        }
    }
    Some(Vp9CodecConfig {
        profile,
        level,
        bit_depth,
        subsampling,
        chroma_code,
        full_range: None,
        cicp: None,
    })
}

/// Parse VP9 private config from either ISOBMFF v1 `vpcC` or WebM VP9 feature-TLV.
#[must_use]
pub fn parse_codec_private(bytes: &[u8]) -> Option<Vp9CodecConfig> {
    parse_vpcc_v1(bytes).or_else(|| parse_webm_codec_private(bytes))
}

/// Parsed fields from one VP9 uncompressed frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vp9FrameHeader {
    /// VP9 profile (`0..=3`).
    pub profile: u8,
    /// Whether this frame merely displays an existing reference buffer.
    pub show_existing_frame: bool,
    /// Index of reference buffer displayed when `show_existing_frame` is true.
    pub frame_to_show_map_idx: Option<u8>,
    /// True for key frames (`frame_type == 0`).
    pub key_frame: bool,
    /// True if this frame is displayed (`show_frame == 1`).
    pub show_frame: bool,
    /// True if intra-only inter frame (`intra_only == 1`).
    pub intra_only: bool,
    /// 8-bit mask of reference buffer slots refreshed by this frame (`0xFF` on keyframes).
    pub refresh_frame_flags: u8,
    /// Reference buffer slot whose coded dimensions are inherited on inter frames (`found_ref == 1`).
    pub size_from_ref: Option<u8>,
    /// Coded sample bit depth (`8`, `10`, or `12`), when coded in this header.
    pub bit_depth: Option<u8>,
    /// Coded VP9 color space, when coded in this header (keyframes and intra-only frames).
    pub color_space: Option<Vp9ColorSpace>,
    /// Coded full-range flag (`true` = full range, `false` = studio/limited), when coded.
    pub full_range: Option<bool>,
    /// Coded subsampling, when coded in this header.
    pub subsampling: Option<Vp9Subsampling>,
    /// Coded frame dimensions `(width, height)`, when coded in this header.
    pub coded_size: Option<(u32, u32)>,
    /// Render dimensions `(width, height)`, when coded in this header.
    pub render_size: Option<(u32, u32)>,
}

impl Vp9FrameHeader {
    /// Synthesize a canonical 12-byte ISOBMFF FullBox v1 `vpcC` payload from a
    /// validated keyframe or intra-only header carrying color/bit-depth metadata.
    ///
    /// Returns `None` if profile, level (`0` = undefined or defined VP9 level),
    /// bit depth, subsampling, or RGB-matrix constraints are invalid.
    #[must_use]
    pub const fn to_vpcc_v1(self, level: u8) -> Option<[u8; 12]> {
        if !is_valid_vp9_level(level) {
            return None;
        }
        let Some(bit_depth) = self.bit_depth else {
            return None;
        };
        let Some(subsampling) = self.subsampling else {
            return None;
        };
        let cs = match self.color_space {
            Some(c) => c,
            None => Vp9ColorSpace::Unknown,
        };
        let is_rgb = matches!(cs, Vp9ColorSpace::Rgb);
        if !is_valid_vp9_profile_combo(self.profile, bit_depth, subsampling, is_rgb) {
            return None;
        }
        let full_range = match self.full_range {
            Some(r) => r,
            None => is_rgb,
        };
        let (primaries, transfer, matrix) = cs.cicp_triplet();
        let b6 = ((bit_depth & 0x0F) << 4)
            | ((subsampling.vpcc_code() & 0x07) << 1)
            | (full_range as u8);
        Some([
            1,
            0,
            0,
            0,
            self.profile,
            level,
            b6,
            primaries,
            transfer,
            matrix,
            0,
            0,
        ])
    }

    /// Synthesize WebM VP9 `CodecPrivate` feature-TLV bytes from this header.
    #[must_use]
    pub fn to_webm_codec_private(self, level: u8) -> Option<Vec<u8>> {
        let _ = self.to_vpcc_v1(level)?;
        let cfg = Vp9CodecConfig {
            profile: self.profile,
            level,
            bit_depth: self.bit_depth,
            subsampling: self.subsampling,
            chroma_code: self.subsampling.map(Vp9Subsampling::vpcc_code),
            full_range: self.full_range,
            cicp: self.color_space.map(Vp9ColorSpace::cicp_triplet),
        };
        cfg.to_webm_codec_private()
    }
}

struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn f(&mut self, n: usize) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.b.get(self.pos / 8)?;
            v = (v << 1) | u32::from((byte >> (7 - (self.pos % 8))) & 1);
            self.pos += 1;
        }
        Some(v)
    }
}

/// Split a VP9 packet (Annex B superframe or single raw frame) into its
/// constituent frame slices in bitstream order.
#[must_use]
pub fn split_superframe(packet: &[u8]) -> Vec<&[u8]> {
    if packet.is_empty() {
        return Vec::new();
    }
    if packet.len() >= 4 {
        let marker = packet[packet.len() - 1];
        if (marker & 0xE0) == 0xC0 {
            let bytes_per_size = usize::from((marker >> 3) & 0x03) + 1;
            let count = usize::from(marker & 0x07) + 1;
            let index_sz = 2 + count * bytes_per_size;
            if packet.len() > index_sz && packet[packet.len() - index_sz] == marker {
                let mut pos = 0usize;
                let size_start = packet.len() - index_sz + 1;
                let data_end = packet.len() - index_sz;
                let mut frames = Vec::with_capacity(count);
                let mut ok = true;
                for i in 0..count {
                    let mut len = 0usize;
                    for b in 0..bytes_per_size {
                        len |= usize::from(packet[size_start + i * bytes_per_size + b]) << (b * 8);
                    }
                    if len == 0 || pos.checked_add(len).is_none_or(|e| e > data_end) {
                        ok = false;
                        break;
                    }
                    frames.push(&packet[pos..pos + len]);
                    pos += len;
                }
                if ok && pos == data_end {
                    return frames;
                }
            }
        }
    }
    alloc::vec![packet]
}

/// Parse one VP9 uncompressed frame header (single frame, not a superframe).
#[must_use]
pub fn parse_frame_header(frame: &[u8]) -> Option<Vp9FrameHeader> {
    let mut r = Bits { b: frame, pos: 0 };
    if r.f(2)? != 0b10 {
        return None;
    }
    let profile_low = r.f(1)?;
    let profile_high = r.f(1)?;
    let profile = (profile_low | (profile_high << 1)) as u8;
    if profile == 3 && r.f(1)? != 0 {
        return None;
    }
    let show_existing_frame = r.f(1)? != 0;
    if show_existing_frame {
        let idx = r.f(3)? as u8;
        return Some(Vp9FrameHeader {
            profile,
            show_existing_frame: true,
            frame_to_show_map_idx: Some(idx),
            key_frame: false,
            show_frame: true,
            intra_only: false,
            refresh_frame_flags: 0,
            size_from_ref: None,
            bit_depth: None,
            color_space: None,
            full_range: None,
            subsampling: None,
            coded_size: None,
            render_size: None,
        });
    }
    let key_frame = r.f(1)? == 0;
    let show_frame = r.f(1)? != 0;
    let error_resilient_mode = r.f(1)? != 0;

    if key_frame {
        if r.f(8)? != 0x49 || r.f(8)? != 0x83 || r.f(8)? != 0x42 {
            return None;
        }
        let (bit_depth, color_space, full_range, subsampling) =
            parse_color_config(&mut r, profile)?;
        let (coded, render) = parse_frame_and_render_size(&mut r)?;
        return Some(Vp9FrameHeader {
            profile,
            show_existing_frame: false,
            frame_to_show_map_idx: None,
            key_frame: true,
            show_frame,
            intra_only: false,
            refresh_frame_flags: 0xFF,
            size_from_ref: None,
            bit_depth: Some(bit_depth),
            color_space: Some(color_space),
            full_range: Some(full_range),
            subsampling: Some(subsampling),
            coded_size: Some(coded),
            render_size: Some(render),
        });
    }

    let intra_only = if !show_frame { r.f(1)? != 0 } else { false };
    if !error_resilient_mode {
        let _reset_frame_context = r.f(2)?;
    }
    if intra_only {
        if r.f(8)? != 0x49 || r.f(8)? != 0x83 || r.f(8)? != 0x42 {
            return None;
        }
        let (bit_depth, color_space, full_range, subsampling) = if profile > 0 {
            parse_color_config(&mut r, profile)?
        } else {
            (8, Vp9ColorSpace::Unknown, false, Vp9Subsampling::Yuv420)
        };
        let refresh_frame_flags = r.f(8)? as u8;
        let (coded, render) = parse_frame_and_render_size(&mut r)?;
        return Some(Vp9FrameHeader {
            profile,
            show_existing_frame: false,
            frame_to_show_map_idx: None,
            key_frame: false,
            show_frame,
            intra_only: true,
            refresh_frame_flags,
            size_from_ref: None,
            bit_depth: Some(bit_depth),
            color_space: Some(color_space),
            full_range: Some(full_range),
            subsampling: Some(subsampling),
            coded_size: Some(coded),
            render_size: Some(render),
        });
    }

    // Inter frame: parse refresh_frame_flags, ref_frame_idx[0..3], and frame_size_with_refs().
    let refresh_frame_flags = r.f(8)? as u8;
    let mut ref_frame_idx = [0u8; 3];
    for slot in &mut ref_frame_idx {
        *slot = r.f(3)? as u8;
        let _sign_bias = r.f(1)?;
    }
    let mut size_from_ref = None;
    for &idx in &ref_frame_idx {
        if r.f(1)? != 0 {
            size_from_ref = Some(idx);
            break;
        }
    }
    let (coded_size, render_size) = if size_from_ref.is_some() {
        let render = if r.f(1)? != 0 {
            Some((r.f(16)? + 1, r.f(16)? + 1))
        } else {
            None
        };
        (None, render)
    } else {
        let (c, rs) = parse_frame_and_render_size(&mut r)?;
        (Some(c), Some(rs))
    };

    Some(Vp9FrameHeader {
        profile,
        show_existing_frame: false,
        frame_to_show_map_idx: None,
        key_frame: false,
        show_frame,
        intra_only: false,
        refresh_frame_flags,
        size_from_ref,
        bit_depth: None,
        color_space: None,
        full_range: None,
        subsampling: None,
        coded_size,
        render_size,
    })
}

fn parse_color_config(
    r: &mut Bits<'_>,
    profile: u8,
) -> Option<(u8, Vp9ColorSpace, bool, Vp9Subsampling)> {
    let bit_depth = if profile >= 2 {
        if r.f(1)? != 0 { 12 } else { 10 }
    } else {
        8
    };
    let cs = Vp9ColorSpace::from_code(r.f(3)? as u8);
    if cs != Vp9ColorSpace::Rgb {
        let full_range = r.f(1)? != 0;
        let subsampling = if profile == 1 || profile == 3 {
            let sx = r.f(1)?;
            let sy = r.f(1)?;
            let reserved_zero = r.f(1)?;
            if reserved_zero != 0 {
                return None;
            }
            match (sx, sy) {
                (0, 0) => Vp9Subsampling::Yuv444,
                (1, 0) => Vp9Subsampling::Yuv422,
                // VP9 profiles 1 and 3 forbid 4:2:0 (sx=1, sy=1).
                _ => return None,
            }
        } else {
            Vp9Subsampling::Yuv420
        };
        Some((bit_depth, cs, full_range, subsampling))
    } else {
        // VP9 spec §6.2.2: CS_RGB is strictly forbidden in profiles 0 and 2.
        if profile == 0 || profile == 2 {
            return None;
        }
        let reserved_zero = r.f(1)?;
        if reserved_zero != 0 {
            return None;
        }
        Some((bit_depth, cs, true, Vp9Subsampling::Yuv444))
    }
}

fn parse_frame_and_render_size(r: &mut Bits<'_>) -> Option<((u32, u32), (u32, u32))> {
    let w = r.f(16)? + 1;
    let h = r.f(16)? + 1;
    let (rw, rh) = if r.f(1)? != 0 {
        (r.f(16)? + 1, r.f(16)? + 1)
    } else {
        (w, h)
    };
    Some(((w, h), (rw, rh)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synthetic_vp9_keyframe_full_range_and_superframe() {
        // Build a valid profile 0 keyframe header:
        // frame_marker=10, profile=00, show_existing=0, key_frame=0, show=1, err_res=0 -> 0b1000_0010 = 0x82
        // sync = 0x49, 0x83, 0x42
        // color_space=2 (BT.709 = 010), color_range=1 (full) -> 0b0101_...
        // width-1=63 (0x003F), height-1=47 (0x002F), render_and_frame_size_different=0
        let mut bits: u64 = 0;
        let mut n = 0usize;
        let mut push = |val: u64, count: usize| {
            bits = (bits << count) | (val & ((1u64 << count) - 1));
            n += count;
        };
        // First 4 bytes: header + sync
        let mut frame = alloc::vec![0x82, 0x49, 0x83, 0x42];
        // Next bits: cs(3)=2, range(1)=1, w-1(16)=63, h-1(16)=47, diff(1)=0 -> 37 bits -> 5 bytes (40 bits)
        push(2, 3);
        push(1, 1);
        push(63, 16);
        push(47, 16);
        push(0, 1);
        drop(push);
        bits <<= 40 - n;
        frame.extend_from_slice(&bits.to_be_bytes()[3..8]);

        let hdr = parse_frame_header(&frame).unwrap();
        assert_eq!(hdr.profile, 0);
        assert!(hdr.key_frame);
        assert!(hdr.show_frame);
        assert_eq!(hdr.bit_depth, Some(8));
        assert_eq!(hdr.color_space, Some(Vp9ColorSpace::Bt709));
        assert_eq!(hdr.full_range, Some(true));
        assert_eq!(hdr.subsampling, Some(Vp9Subsampling::Yuv420));
        assert_eq!(hdr.coded_size, Some((64, 48)));
        assert_eq!(hdr.render_size, Some((64, 48)));

        let vpcc = hdr.to_vpcc_v1(10).unwrap();
        assert_eq!(vpcc[0], 1);
        assert_eq!(vpcc[4], 0); // profile 0
        assert_eq!(vpcc[6], (8 << 4) | 1); // 8-bit, 4:2:0, full_range=1
        assert_eq!(&vpcc[7..10], &[1, 1, 1]); // BT.709

        // Wrap in a 2-frame superframe
        let f2 = [0x86u8, 0x00, 0x00]; // inter frame
        let mut sf = Vec::new();
        sf.extend_from_slice(&frame);
        sf.extend_from_slice(&f2);
        // marker: 0b110_00_001 = 0xC1 (1 byte per size, 2 frames)
        sf.push(0xC1);
        sf.push(frame.len() as u8);
        sf.push(f2.len() as u8);
        sf.push(0xC1);
        let parts = split_superframe(&sf);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], frame.as_slice());
        assert_eq!(parts[1], f2.as_slice());
    }
}
