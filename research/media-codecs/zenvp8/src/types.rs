//! Core decoder data types — ported shapes from libvpx `blockd.h`/`onyxc_int.h`
//! collapsed into safe-Rust owned arrays.

use crate::tables::MvContext;

/// `MV_REFERENCE_FRAME` — the entropy-coded reference selector.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum RefFrame {
    /// Intra-coded MB.
    #[default]
    Intra = 0,
    /// Last frame.
    Last = 1,
    /// Golden frame.
    Golden = 2,
    /// Alt-ref frame.
    AltRef = 3,
}

impl RefFrame {
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Intra,
            1 => Self::Last,
            2 => Self::Golden,
            _ => Self::AltRef,
        }
    }
}

/// `MB_PREDICTION_MODE` — whole-MB mode. Intra modes share values with
/// `LumaMode`; inter modes extend the range. Numeric values match libvpx
/// (`blockd.h`) since trees encode them directly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(i8)]
pub enum MbMode {
    #[default]
    DcPred = 0,
    VPred = 1,
    HPred = 2,
    TmPred = 3,
    /// 4x4 intra (B_PRED).
    BPred = 4,
    NearestMv = 5,
    NearMv = 6,
    /// Inter with zero MV.
    ZeroMv = 7,
    NewMv = 8,
    /// 4x4 inter with per-sub-block MVs.
    SplitMv = 9,
}

impl MbMode {
    #[allow(dead_code)]
    pub(crate) fn is_intra(self) -> bool {
        (self as i8) <= MbMode::BPred as i8
    }
    /// libvpx `is_4x4` — B_PRED or SPLITMV (no Y2 block).
    pub(crate) fn is_4x4(self) -> bool {
        matches!(self, MbMode::BPred | MbMode::SplitMv)
    }
    pub(crate) fn from_i8(v: i8) -> Self {
        match v {
            0 => Self::DcPred,
            1 => Self::VPred,
            2 => Self::HPred,
            3 => Self::TmPred,
            4 => Self::BPred,
            5 => Self::NearestMv,
            6 => Self::NearMv,
            7 => Self::ZeroMv,
            8 => Self::NewMv,
            _ => Self::SplitMv,
        }
    }
}

/// Motion vector in eighth-pel units (libvpx `int_mv`/`MV` pair).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mv {
    pub row: i16,
    pub col: i16,
}

impl Mv {
    pub const ZERO: Mv = Mv { row: 0, col: 0 };
    /// `int_mv.as_int` equivalent for equality checks — exact bit packing of
    /// two i16s; identity comparisons only.
    #[inline(always)]
    pub(crate) fn as_int(self) -> u32 {
        ((self.row as u16 as u32) << 16) | (self.col as u16 as u32)
    }
}

/// Per-sub-block info (bmi[16]): for SPLITMV, the sub-block MV; for B_PRED,
/// the 4x4 mode.
#[derive(Clone, Copy, Debug, Default)]
pub struct BlockMode {
    /// SPLITMV: this 4x4's MV. Also used to hold the MB MV for the
    /// findnearmv bias math.
    pub mv: Mv,
    /// B_PRED: this sub-block's intra mode index (0..=9).
    pub mode: i8,
}

/// `MB_MODE_INFO` — one macroblock's decoded mode state.
#[derive(Clone, Debug)]
pub struct MbInfo {
    pub mode: MbMode,
    /// Chroma intra mode (intra MBs only).
    pub uv_mode: i8,
    /// Which reference frame inter MBs predict from.
    pub ref_frame: RefFrame,
    /// Segment index 0..3 (from update_mb_segmentation_map or prior frame).
    pub segment_id: u8,
    /// `mb_skip_coeff` — bitstream flag; rewritten to `eobtotal == 0` after
    /// token decode, and drives inner-edge loop filtering.
    pub mb_skip_coeff: bool,
    /// 4x4 prediction modes (B_PRED) or sub-block MVs (SPLITMV).
    pub bmi: [BlockMode; 16],
    /// Whole-MB MV (inter modes; the SPLITMV value is bmi[15] post-decode).
    pub mv: Mv,
    /// `need_to_clamp_mvs` — NEWMV/SPLITMV whose MVs exceed the legal region;
    /// triggers `clamp_mv_to_umv_border` at recon time.
    pub need_to_clamp_mvs: bool,
    /// `partitioning` — SPLITMV split configuration 0..=3.
    pub partitioning: u8,
}

impl MbInfo {
    pub fn new() -> Self {
        MbInfo {
            mode: MbMode::DcPred,
            uv_mode: 0,
            ref_frame: RefFrame::Intra,
            segment_id: 0,
            mb_skip_coeff: false,
            bmi: [BlockMode::default(); 16],
            mv: Mv::ZERO,
            need_to_clamp_mvs: false,
            partitioning: 0,
        }
    }
}

/// Segmentation state — `mb_segment_*`/`segment_feature_data` (`blockd.h`).
/// `update_mb_segmentation_map` itself is per-frame, kept on FrameHeader.
#[derive(Clone)]
pub struct Segmentation {
    pub enabled: bool,
    /// `SEGMENT_ABSDATA` when true.
    pub abs_delta: bool,
    /// [feature][seg] — feature 0 = alt quantizer, 1 = alt loop filter.
    pub feature_data: [[i8; 4]; 2],
    /// Persisted across frames: when a new map isn't coded, MBs reuse this.
    pub update_map: bool,
    pub tree_probs: [u8; 3],
}

impl Default for Segmentation {
    fn default() -> Self {
        Segmentation {
            enabled: false,
            // keyframe reset state (SEGMENT_DELTADATA).
            abs_delta: false,
            feature_data: [[0; 4]; 2],
            update_map: false,
            tree_probs: [255; 3],
        }
    }
}

/// `ref_lf_deltas`/`mode_lf_deltas` — persistent mode/ref loop-filter deltas.
#[derive(Clone, Default)]
pub struct LfDeltas {
    /// `[INTRA, LAST, GOLDEN, ALTREF]`.
    pub ref_deltas: [i8; 4],
    /// `[B_PRED/split-class, ZEROMV-class, NEAREST/NEAR/NEW, SPLITMV]` —
    /// indexed via `MODE_LF_LUT`.
    pub mode_deltas: [i8; 4],
    pub enabled: bool,
}

/// Persistent entropy context — libvpx `FRAME_CONTEXT` (`entropymode.h`).
/// Lives across frames; per-frame updates apply to it unless the frame says
/// `refresh_entropy_probs == 0`, in which case it's restored after decode.
#[derive(Clone)]
pub struct FrameContext {
    /// `fc.ymode_prob` — inter luma mode probs.
    pub ymode_prob: [u8; 4],
    /// `fc.uv_mode_prob`.
    pub uv_mode_prob: [u8; 3],
    /// `fc.bmode_prob` — inter B_PRED probs.
    pub bmode_prob: [u8; 9],
    /// `fc.mvc` — row + column MV contexts.
    pub mvc: [MvContext; 2],
    /// `fc.pre_coef_probs` — flattened [band][ctx][token-tree] table.
    /// Indexed [plane_type(4)][band(8)][context(3)][prob(11)].
    pub coef_probs: [[[[u8; 11]; 3]; 8]; 4],
}

impl FrameContext {
    pub fn new() -> Self {
        FrameContext {
            ymode_prob: crate::tables::DEFAULT_YMODE_PROB,
            uv_mode_prob: crate::tables::DEFAULT_UV_MODE_PROB,
            bmode_prob: crate::tables::DEFAULT_BMODE_PROB,
            mvc: crate::tables::DEFAULT_MV_CONTEXT,
            coef_probs: crate::tables::COEFF_PROBS,
        }
    }
}
