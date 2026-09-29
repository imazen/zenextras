//! Frame header parsing — port of `vp8/decoder/decodeframe.c`'s
//! `vp8_decode_frame` header section plus `init_frame` semantics.
//!
//! Parse order (partition 0 bool decoder over `data..data_end`):
//!   [kf: colorspace,clamp] segmentation; filter_type+level+sharpness;
//!   lf deltas; token-partition count; quantizer block; [inter: refresh +
//!   copy flags + sign biases]; refresh_entropy; refresh_last; coef updates.

use crate::boold::BoolReader;
use crate::error::DecodeError;
use crate::tables::{COEFF_UPDATE_PROBS, MB_FEATURE_DATA_BITS};
use crate::types::{FrameContext, LfDeltas, Segmentation};

/// `vp8_setup_version` — profile-dependent defaults.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct VersionInfo {
    /// 0..3, from the frame tag.
    pub version: u8,
    /// `filter_type` default before the stream bit overrides it.
    pub filter_type_simple: bool,
    /// `use_bilinear_mc_filter`.
    pub use_bilinear_mc: bool,
    /// `full_pixel` — forces chroma MVs to whole-pel.
    pub full_pixel: bool,
    /// `clamp_type` (keyframe only; unused downstream, kept for parity).
    pub clamp_type: bool,
}

impl VersionInfo {
    pub(crate) fn for_version(v: u8) -> Self {
        match v {
            0 => VersionInfo {
                version: 0,
                filter_type_simple: false,
                use_bilinear_mc: false,
                full_pixel: false,
                clamp_type: false,
            },
            1 => VersionInfo {
                version: 1,
                filter_type_simple: true,
                use_bilinear_mc: true,
                full_pixel: false,
                clamp_type: false,
            },
            2 => VersionInfo {
                version: 2,
                filter_type_simple: false,
                use_bilinear_mc: true,
                full_pixel: false,
                clamp_type: false,
            },
            _ => VersionInfo {
                version: 3,
                filter_type_simple: true,
                use_bilinear_mc: true,
                full_pixel: true,
                clamp_type: false,
            },
        }
    }
}

/// Everything decode needs from the frame header.
/// A few fields (`version`, scale factors) are informational only.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct FrameHeader {
    pub key_frame: bool,
    pub version: u8,
    pub show_frame: bool,
    pub first_partition_len: usize,

    /// Visible dimensions (keyframe-coded; inter reuses).
    pub width: usize,
    pub height: usize,
    /// 2-bit scale fields (informational; scaling unsupported — recorded).
    pub horiz_scale: u8,
    pub vert_scale: u8,

    pub filter_type_simple: bool,
    pub filter_level: u8,
    pub sharpness_level: u8,
    pub use_bilinear_mc: bool,
    pub full_pixel: bool,

    /// `multi_token_partition` exponent — partitions = 1<<n.
    pub multi_token_partition: u8,

    pub base_qindex: i32,
    /// y1dc, y2dc, y2ac, uvdc, uvac deltas.
    pub q_deltas: [i32; 5],

    pub refresh_golden: bool,
    pub refresh_alt_ref: bool,
    /// copy_buffer_to_gf/arf values (0=none,1=last,2=golden,3=altref src).
    pub copy_buffer_to_gf: u8,
    pub copy_buffer_to_arf: u8,
    /// `[INTRA,LAST,GOLDEN,ALTREF]` sign biases (only 2,3 meaningful).
    pub sign_bias: [bool; 4],
    pub refresh_entropy: bool,
    pub refresh_last: bool,

    /// `update_mb_segmentation_map`.
    pub seg_update_map: bool,
    /// `update_mb_segmentation_data`.
    pub seg_update_data: bool,
}

/// Parsed frame tag + optional keyframe geometry (the 7-byte section).
pub(crate) struct TagInfo {
    pub key_frame: bool,
    pub version: u8,
    pub show_frame: bool,
    pub first_partition_len: usize,
    /// Filled for keyframes.
    pub width: usize,
    pub height: usize,
    pub horiz_scale: u8,
    pub vert_scale: u8,
    /// Offset where partition-0 bool decoding starts (past kf startcode).
    pub data_start: usize,
}

/// Parse the 3-byte frame tag + 7-byte keyframe preamble.
/// `pkt` is the complete frame payload.
pub(crate) fn parse_tag(pkt: &[u8]) -> Result<TagInfo, DecodeError> {
    if pkt.len() < 3 {
        return Err(DecodeError::NotEnoughData);
    }
    let tag = pkt[0] as u32 | ((pkt[1] as u32) << 8) | ((pkt[2] as u32) << 16);
    let key_frame = (tag & 1) == 0;
    let version = ((tag >> 1) & 0x7) as u8;
    let show_frame = (tag >> 4) & 1 != 0;
    let first_partition_len = (tag >> 5) as usize;
    if first_partition_len == 0 {
        return Err(DecodeError::Partition0TooShort);
    }
    let mut t = TagInfo {
        key_frame,
        version,
        show_frame,
        first_partition_len,
        width: 0,
        height: 0,
        horiz_scale: 0,
        vert_scale: 0,
        data_start: 3,
    };
    if key_frame {
        if pkt.len() < 10 {
            return Err(DecodeError::NotEnoughData);
        }
        if pkt[3] != 0x9d || pkt[4] != 0x01 || pkt[5] != 0x2a {
            return Err(DecodeError::InvalidSignature);
        }
        t.width = ((pkt[6] as usize) | ((pkt[7] as usize) << 8)) & 0x3fff;
        t.horiz_scale = pkt[7] >> 6;
        t.height = ((pkt[8] as usize) | ((pkt[9] as usize) << 8)) & 0x3fff;
        t.vert_scale = pkt[9] >> 6;
        if t.width == 0 || t.height == 0 {
            return Err(DecodeError::InvalidDimensions);
        }
        t.data_start = 10;
    }
    if pkt.len() < t.data_start + first_partition_len {
        return Err(DecodeError::PartitionTruncated);
    }
    Ok(t)
}

/// `get_delta_q` — optional 4-bit signed delta.
fn get_delta_q(bc: &mut BoolReader) -> i32 {
    if bc.bit() == 0 {
        0
    } else {
        let v = bc.literal(4) as i32;
        if bc.bit() != 0 {
            -v
        } else {
            v
        }
    }
}

/// Parse everything inside partition 0 after the tag.
///
/// `bc` is the partition-0 bool decoder (spans `data_start..pkt_end`); on
/// return it is positioned at the start of the mode/MV section — the caller
/// continues decoding modes on the same decoder.
/// Returns `(header, token_partitions)` where each `(off, len)` is relative
/// to `pkt`, and mutates `fc`/`seg`/`lf`/`lfc` (persistent state).
#[allow(clippy::too_many_arguments)]
pub(crate) fn parse_partition0(
    bc: &mut BoolReader,
    pkt: &[u8],
    tag: &TagInfo,
    key_frame: bool,
    fc: &mut FrameContext,
    seg: &mut Segmentation,
    lf: &mut LfDeltas,
    lfc: &mut FrameContext,
) -> Result<(FrameHeader, Vec<(usize, usize)>), DecodeError> {
    let vi = VersionInfo::for_version(tag.version);
    let data_start = tag.data_start;

    if key_frame {
        // colorspace + clamp_type bits (keyframe only)
        let _colorspace = bc.bit();
        let _clamp_type = bc.bit();
    }

    // --- segmentation ---
    seg.enabled = bc.bit() != 0;
    let mut seg_update_map = false;
    let mut seg_update_data = false;
    if seg.enabled {
        seg_update_map = bc.bit() != 0;
        seg_update_data = bc.bit() != 0;
        if seg_update_data {
            seg.abs_delta = bc.bit() != 0;
            seg.feature_data = [[0; 4]; 2];
            for (i, row) in seg.feature_data.iter_mut().enumerate() {
                for cell in row.iter_mut() {
                    if bc.bit() != 0 {
                        let v = bc.literal(MB_FEATURE_DATA_BITS[i]) as i32;
                        *cell = if bc.bit() != 0 { -v } else { v } as i8;
                    } else {
                        *cell = 0;
                    }
                }
            }
        }
        if seg_update_map {
            seg.tree_probs = [255; 3];
            for i in 0..3 {
                if bc.bit() != 0 {
                    seg.tree_probs[i] = bc.literal(8) as u8;
                }
            }
        }
        seg.update_map = seg_update_map;
    } else {
        // No segmentation updates on this frame — update flags cleared,
        // but feature data / tree probs / abs_delta persist.
        seg.update_map = false;
    }

    // --- loop filter ---
    let filter_type_simple = bc.bit() != 0; // overrides version default
    let filter_level = bc.literal(6) as u8;
    let sharpness_level = bc.literal(3) as u8;

    lf.enabled = bc.bit() != 0;
    if lf.enabled {
        let update = bc.bit() != 0;
        if update {
            for i in 0..4 {
                if bc.bit() != 0 {
                    let v = bc.literal(6) as i32;
                    lf.ref_deltas[i] = (if bc.bit() != 0 { -v } else { v }) as i8;
                }
            }
            for i in 0..4 {
                if bc.bit() != 0 {
                    let v = bc.literal(6) as i32;
                    lf.mode_deltas[i] = (if bc.bit() != 0 { -v } else { v }) as i8;
                }
            }
        }
    }

    // --- token partitions (count from the *partition-0* reader) ---
    let multi_token_partition = bc.literal(2) as u8;
    let num_partitions = 1usize << multi_token_partition;
    let mut token_parts = Vec::with_capacity(num_partitions);
    {
        let base = data_start + tag.first_partition_len;
        let size_table = base; // 3-byte LE sizes at partition-1 start
        let mut off = base + 3 * (num_partitions - 1);
        let mut left = pkt.len().saturating_sub(off);
        for i in 0..num_partitions {
            let sz = if i + 1 < num_partitions {
                if size_table + i * 3 + 3 > pkt.len() {
                    return Err(DecodeError::PartitionTruncated);
                }
                pkt[size_table + i * 3] as usize
                    | ((pkt[size_table + i * 3 + 1] as usize) << 8)
                    | ((pkt[size_table + i * 3 + 2] as usize) << 16)
            } else {
                left
            };
            if sz > left {
                return Err(DecodeError::PartitionTruncated);
            }
            token_parts.push((off, sz));
            off += sz;
            left -= sz;
        }
    }

    // --- quantizer ---
    let base_qindex = bc.literal(7) as i32;
    let mut q_deltas = [0i32; 5];
    for d in q_deltas.iter_mut() {
        *d = get_delta_q(bc);
    }

    // --- refresh flags + sign bias (inter only) ---
    let mut refresh_golden = true;
    let mut refresh_alt_ref = true;
    let mut copy_gf = 0u8;
    let mut copy_arf = 0u8;
    let mut sign_bias = [false; 4];
    if !key_frame {
        refresh_golden = bc.bit() != 0;
        refresh_alt_ref = bc.bit() != 0;
        if !refresh_golden {
            copy_gf = bc.literal(2) as u8;
        }
        if !refresh_alt_ref {
            copy_arf = bc.literal(2) as u8;
        }
        sign_bias[2] = bc.bit() != 0;
        sign_bias[3] = bc.bit() != 0;
    }

    let refresh_entropy = bc.bit() != 0;
    if !refresh_entropy {
        *lfc = fc.clone(); // save for post-decode restore
    }
    let refresh_last = key_frame || bc.bit() != 0;

    // --- coefficient probability updates ---
    for (pi, ci) in COEFF_UPDATE_PROBS.iter().zip(fc.coef_probs.iter_mut()) {
        for (pj, cj) in pi.iter().zip(ci.iter_mut()) {
            for (pk, ck) in pj.iter().zip(cj.iter_mut()) {
                for (pl, cl) in pk.iter().zip(ck.iter_mut()) {
                    if bc.bool_read(*pl) != 0 {
                        *cl = bc.literal(8) as u8;
                    }
                }
            }
        }
    }

    let hdr = FrameHeader {
        key_frame,
        version: tag.version,
        show_frame: tag.show_frame,
        first_partition_len: tag.first_partition_len,
        width: tag.width,
        height: tag.height,
        horiz_scale: tag.horiz_scale,
        vert_scale: tag.vert_scale,
        filter_type_simple,
        filter_level,
        sharpness_level,
        use_bilinear_mc: vi.use_bilinear_mc,
        full_pixel: vi.full_pixel,
        multi_token_partition,
        base_qindex,
        q_deltas,
        refresh_golden,
        refresh_alt_ref,
        copy_buffer_to_gf: copy_gf,
        copy_buffer_to_arf: copy_arf,
        sign_bias,
        refresh_entropy,
        refresh_last,
        seg_update_map,
        seg_update_data,
    };
    // The caller continues with `bc` for the mode/MV pass.
    Ok((hdr, token_parts))
}
