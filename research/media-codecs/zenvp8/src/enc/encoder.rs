//! VP8 video encoder — conformant-stream encoder built from libvpx
//! `vp8/encoder/` emission semantics (`bitstream.c`, `encodemv.c`,
//! `tokenize.c`, `vp8_quantize.c`, `dct.c`) reusing the decoder's proven
//! prediction / MC / IDCT / context machinery.
//!
//! Deliberate v1 constraints (see PORTED-FROM.md — "intentional encoder
//! simplifications"):
//! * intra MBs use whole-block I16 modes only (no B_PRED);
//! * inter MBs use ZEROMV / NEARESTMV / NEARMV / NEWMV (no SPLITMV) and
//!   reference LAST only (golden/altref slots are never refreshed or
//!   referenced, which is bitstream-legal);
//! * one token partition; `mb_no_coeff_skip = 1`; no segmentation.
//!
//! Entropy adaptation and in-loop filtering are real libvpx semantics:
//! coefficient / mode / MV probability updates and `prob_skip_false` are
//! computed from per-frame symbol counts (`vp8_update_coef_probs`,
//! `update_mode`, `vp8_write_mvprobs`, `pack_inter_mode_mvs`), and the loop
//! filter level is picked by `vp8cx_pick_filter_level_fast` with the
//! realtime default LF deltas applied to the reconstruction.
//!
//! Mode selection is encoder-side policy (not a libvpx port): per-MB SAD
//! comparison over the candidate set plus a small fixed rate penalty.

use super::adapt::{self, FrameCounts};
use super::boolw::BoolWriter;
use super::dct;
use super::lf;
use super::mv::encode_mv;
use super::quant::{regular_quantize_b, QuantSet};
use super::tokens::{pack_tokens, tokenize_mb, MbCoeffs, TokRec, COEF_TREE};
use crate::framebuf::FrameBuf;
use crate::inter::find_near_mvs;
use crate::loopfilter::LoopFilterTables;
use crate::mc::{self, RefView, SubpelKind};
use crate::predict;
use crate::tables::*;
use crate::tokens::CTX_CELLS;
use crate::types::{FrameContext, MbInfo, MbMode, Mv, RefFrame};
use std::collections::VecDeque;

/// Encoder settings.
#[derive(Clone, Copy, Debug)]
pub struct EncoderConfig {
    /// Visible luma dimensions in pixels.
    pub width: usize,
    pub height: usize,
    /// Base quantizer index — clamped to the VP8 range 0..=127
    /// (`base_qindex`).
    pub qindex: i32,
    /// Force a keyframe every N frames (0 = first frame only).
    pub keyframe_interval: usize,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        EncoderConfig {
            width: 0,
            height: 0,
            qindex: 30,
            keyframe_interval: 0,
        }
    }
}

/// Errors returned by [`Vp8Encoder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EncodeError {
    /// `width`/`height` zero or not representable in the 14-bit field.
    InvalidDimensions,
    /// Input planes smaller than `width*height` / chroma requirement.
    SourceTooShort,
    /// Internal capacity guard (mirrors decoder `TooLarge`).
    TooLarge,
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::InvalidDimensions => "invalid dimensions",
            Self::SourceTooShort => "source planes too short",
            Self::TooLarge => "frame too large",
        };
        f.write_str(s)
    }
}
impl std::error::Error for EncodeError {}

/// `vp8_treed_write` on an arbitrary mode tree — emit the branch bits that
/// reach leaf `-value`. `probs[i >> 1]` is the branch probability at node
/// array index `i`.
fn emit_tree(w: &mut BoolWriter, tree: &[i8], probs: &[u8], value: i8) {
    fn subtree_has(tree: &[i8], node: usize, v: i8) -> bool {
        for b in 0..2usize {
            let t = tree[node + b];
            if t <= 0 {
                if -t == v {
                    return true;
                }
            } else if subtree_has(tree, t as usize, v) {
                return true;
            }
        }
        false
    }
    let mut i = 0usize;
    loop {
        let t0 = tree[i];
        let zero_side =
            (t0 <= 0 && -t0 == value) || (t0 > 0 && subtree_has(tree, t0 as usize, value));
        if zero_side {
            w.write(0, probs[i >> 1]);
            if t0 <= 0 {
                break;
            }
            i = t0 as usize;
        } else {
            w.write(1, probs[i >> 1]);
            let t1 = tree[i + 1];
            debug_assert!(t1 <= 0 && -t1 == value || t1 > 0);
            if t1 <= 0 {
                break;
            }
            i = t1 as usize;
            debug_assert!(
                subtree_has(tree, i, value),
                "emit_tree: value {value} not in tree"
            );
        }
    }
}

/// A whole input frame, MB-padded by edge replication (libvpx's padded
/// source `YV12_BUFFER_CONFIG` equivalent).
struct SrcFrame {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    y_stride: usize,
    uv_stride: usize,
}

impl SrcFrame {
    #[allow(clippy::too_many_arguments)]
    fn build(
        y: &[u8],
        u: &[u8],
        v: &[u8],
        y_stride: usize,
        uv_stride: usize,
        w: usize,
        h: usize,
        y_w: usize,
        y_h: usize,
    ) -> Result<Self, EncodeError> {
        let uv_w = y_w / 2;
        let uv_h = y_h / 2;
        let (uw, uh) = (w.div_ceil(2), h.div_ceil(2));
        if y_stride < w || uv_stride < uw {
            return Err(EncodeError::SourceTooShort);
        }
        if y.len() < y_stride * h || u.len() < uv_stride * uh || v.len() < uv_stride * uh {
            return Err(EncodeError::SourceTooShort);
        }
        let mut sy = vec![0u8; y_w * y_h];
        let mut su = vec![0u8; uv_w * uv_h];
        let mut sv = vec![0u8; uv_w * uv_h];
        for r in 0..y_h {
            let sr = r.min(h - 1);
            for c in 0..y_w {
                sy[r * y_w + c] = y[sr * y_stride + c.min(w - 1)];
            }
        }
        for (src, dst) in [(u, &mut su), (v, &mut sv)] {
            for r in 0..uv_h {
                let sr = r.min(uh - 1);
                for c in 0..uv_w {
                    dst[r * uv_w + c] = src[sr * uv_stride + c.min(uw - 1)];
                }
            }
        }
        Ok(SrcFrame {
            y: sy,
            u: su,
            v: sv,
            y_stride: y_w,
            uv_stride: uv_w,
        })
    }
}

/// SAD helpers over strided blocks.
fn sad16(src: &[u8], ss: usize, pred: &[u8], ps: usize) -> u32 {
    let mut s = 0u32;
    for r in 0..16 {
        for c in 0..16 {
            s += (src[r * ss + c] as i32 - pred[r * ps + c] as i32).unsigned_abs();
        }
    }
    s
}
fn sad8(src: &[u8], ss: usize, pred: &[u8], ps: usize) -> u32 {
    let mut s = 0u32;
    for r in 0..8 {
        for c in 0..8 {
            s += (src[r * ss + c] as i32 - pred[r * ps + c] as i32).unsigned_abs();
        }
    }
    s
}

/// Scratch prediction planes for one MB (flat 16x16 luma, 8x8 chroma).
struct PredBuf {
    y: [u8; 256],
    u: [u8; 64],
    v: [u8; 64],
}

/// Build the inter prediction for `mv` into `pred` via the exact MC path
/// the decoder uses (same clamping semantics via `need_to_clamp_mvs`).
/// `src_y_off`/`src_uv_off` are the MB's position index in `rf`'s planes.
#[allow(clippy::too_many_arguments)]
fn inter_pred(
    rf: &RefView,
    edges: (i32, i32, i32, i32),
    edges_margined: (i32, i32, i32, i32),
    src_y_off: usize,
    src_uv_off: usize,
    mv: Mv,
    pred: &mut PredBuf,
) {
    let need = mc::check_mv_bounds(mv, edges_margined);
    mc::fetch_inter16x16(
        mv,
        need,
        edges,
        !0,
        SubpelKind::Sixtap,
        rf,
        src_y_off,
        src_uv_off,
        &mut pred.y,
        &mut pred.u,
        &mut pred.v,
        16,
        8,
        0,
        0,
    );
}

/// Chosen-mode record replayed at emission time.
#[derive(Clone)]
struct MbChoice {
    mode: MbMode,
    ref_frame: RefFrame,
    mv: Mv,
    /// Clamped `best_ref_mv` used as the NEWMV delta base (and the
    /// `update_mvcount` reference). Zero for non-NEWMV choices.
    best_ref_mv: Mv,
    uv_mode: i8,
}

/// `set_default_lf_deltas` (MODE_REALTIME): ref deltas
/// `[INTRA +2, LAST 0, GOLDEN -2, ALTREF -2]`, mode deltas
/// `[B_PRED +4, ZEROMV -12, NEWMV +2, SPLITMV +4]`.
const DEFAULT_REF_LF_DELTAS: [i8; 4] = [2, 0, -2, -2];
const DEFAULT_MODE_LF_DELTAS: [i8; 4] = [4, -12, 2, 4];

/// Pure-safe VP8 video encoder (feature `encoder`). Output packets are
/// raw VP8 frames (not IVF/WebP) — the same payload unit
/// `Vp8Decoder::push` accepts.
pub struct Vp8Encoder {
    cfg: EncoderConfig,
    mb_rows: usize,
    mb_cols: usize,
    y_w: usize,
    y_h: usize,
    quant: QuantSet,
    frame_count: u64,
    /// LAST reference (the only ref slot ever used).
    last: Option<FrameBuf>,
    /// `(mb_rows+1)*(mb_cols+1)` mode grid — same layout as the decoder.
    mi: Vec<MbInfo>,
    /// `pc->above_context` — 9 cells per MB column.
    above_ctx: Vec<u8>,
    /// Entropy context (`cm->fc`) — updated in place each frame because
    /// `refresh_entropy = 1` is always signalled.
    fc: FrameContext,
    /// Per-frame symbol counts feeding probability updates.
    counts: FrameCounts,
    /// Precomputed tree encodings for `tree_probs_from_distribution`.
    enc_coef: Vec<(i32, i32)>,
    enc_ymode: Vec<(i32, i32)>,
    enc_uv: Vec<(i32, i32)>,
    enc_mvshort: Vec<(i32, i32)>,
    /// `cm->lf_info` + `xd->last_sharpness_level` (persistent tables).
    lft: LoopFilterTables,
    last_sharpness: i32,
    /// `cm->filter_level` — persists across frames; the picker seeds from it.
    filter_level: i32,
    /// `xd->ref_lf_deltas` / `mode_lf_deltas` (reset to the realtime
    /// defaults by `setup_features` on every keyframe).
    ref_lf_deltas: [i8; 4],
    mode_lf_deltas: [i8; 4],
    /// `xd->last_*_lf_deltas` — last values actually emitted, for the
    /// per-delta update-flag dedup.
    last_ref_lf_deltas: [i8; 4],
    last_mode_lf_deltas: [i8; 4],
    /// `xd->mode_ref_lf_delta_enabled`/`update` — armed by
    /// `setup_features` at keyframes, then persist.
    lf_delta_enabled: bool,
    lf_delta_update: bool,
    toks: Vec<TokRec>,
    packets: VecDeque<Vec<u8>>,
}

impl Vp8Encoder {
    /// Create an encoder. `width`/`height` must fit the 14-bit fields.
    pub fn new(cfg: EncoderConfig) -> Result<Self, EncodeError> {
        if cfg.width == 0 || cfg.height == 0 || cfg.width > 0x3fff || cfg.height > 0x3fff {
            return Err(EncodeError::InvalidDimensions);
        }
        let mb_cols = cfg.width.div_ceil(16);
        let mb_rows = cfg.height.div_ceil(16);
        // mirror the decoder's resource cap
        if mb_rows * mb_cols > 1 << 16 {
            return Err(EncodeError::TooLarge);
        }
        let (y_w, y_h) = (mb_cols * 16, mb_rows * 16);
        Ok(Vp8Encoder {
            cfg,
            mb_rows,
            mb_cols,
            y_w,
            y_h,
            quant: QuantSet::new(cfg.qindex),
            frame_count: 0,
            last: None,
            mi: vec![MbInfo::new(); (mb_rows + 1) * (mb_cols + 1)],
            above_ctx: vec![0u8; CTX_CELLS * mb_cols],
            fc: FrameContext::new(),
            counts: FrameCounts::default(),
            enc_coef: adapt::tree_encodings(&COEF_TREE, 12),
            enc_ymode: adapt::tree_encodings(&YMODE_TREE, 5),
            enc_uv: adapt::tree_encodings(&UV_MODE_TREE, 4),
            enc_mvshort: adapt::tree_encodings(&SMALL_MV_TREE, 8),
            lft: LoopFilterTables::default(),
            last_sharpness: -1,
            filter_level: 0,
            ref_lf_deltas: [0; 4],
            mode_lf_deltas: [0; 4],
            last_ref_lf_deltas: [0; 4],
            last_mode_lf_deltas: [0; 4],
            lf_delta_enabled: false,
            lf_delta_update: false,
            toks: Vec::new(),
            packets: VecDeque::new(),
        })
    }

    /// Submit one frame of planar I420 input. `y_stride`/`uv_stride` are
    /// row pitches in bytes; chroma dimensions are `(w+1)/2 × (h+1)/2`.
    /// Produces exactly one packet, retrievable via `pull_packet`.
    pub fn push_frame(
        &mut self,
        y: &[u8],
        u: &[u8],
        v: &[u8],
        y_stride: usize,
        uv_stride: usize,
    ) -> Result<(), EncodeError> {
        let src = SrcFrame::build(
            y,
            u,
            v,
            y_stride,
            uv_stride,
            self.cfg.width,
            self.cfg.height,
            self.y_w,
            self.y_h,
        )?;
        let key_frame = self.last.is_none()
            || (self.cfg.keyframe_interval != 0
                && (self.frame_count as usize).is_multiple_of(self.cfg.keyframe_interval));
        let pkt = self.encode_one(&src, key_frame);
        self.packets.push_back(pkt);
        self.frame_count += 1;
        Ok(())
    }

    /// Pull the next encoded packet (one per `push_frame`).
    pub fn pull_packet(&mut self) -> Option<Vec<u8>> {
        self.packets.pop_front()
    }

    /// `setup_features` keyframe branch — reset segmentation (unused),
    /// re-arm LF deltas at the realtime defaults and clear the
    /// last-emitted delta bookkeeping. Mirrors the decoder's KF reset of
    /// the same state, so per-delta update flags are only suppressed when
    /// the decoder is guaranteed to already hold the value.
    fn setup_features_keyframe(&mut self) {
        self.lf_delta_enabled = true;
        self.lf_delta_update = true;
        self.ref_lf_deltas = DEFAULT_REF_LF_DELTAS;
        self.mode_lf_deltas = DEFAULT_MODE_LF_DELTAS;
        self.last_ref_lf_deltas = [0; 4];
        self.last_mode_lf_deltas = [0; 4];
    }

    /// Encode one frame end-to-end: mode select → recon → tokenize →
    /// loop-filter pick/apply → emit.
    fn encode_one(&mut self, src: &SrcFrame, key_frame: bool) -> Vec<u8> {
        let stride = self.mb_cols + 1;
        let mut new = FrameBuf::new(self.y_w, self.y_h);
        new.setup_intra_recon_top_line();

        if key_frame {
            // init_frame() KF branch: the decoder resets entropy to the
            // default tables on a keyframe — the encoder must do the same
            // before tokenizing/counting so prob updates stay in sync.
            self.fc = FrameContext::new();
            self.setup_features_keyframe();
        }

        self.toks.clear();
        // Real cells are rewritten each frame; border cells must hold the
        // calloc'd defaults (libvpx's once-allocated mode_info_context).
        for m in self.mi.iter_mut() {
            *m = MbInfo::new();
        }

        // Token contexts reset every frame (libvpx zeroes above_context in
        // init_frame; decoder.rs mirrors this at each frame start).
        for c in self.above_ctx.iter_mut() {
            *c = 0;
        }
        let mut left_ctx = [0u8; CTX_CELLS];
        self.counts.reset((self.mb_rows * self.mb_cols) as u32);
        let mut choices = Vec::with_capacity(self.mb_rows * self.mb_cols);
        let mut skips = Vec::with_capacity(self.mb_rows * self.mb_cols);

        for mb_row in 0..self.mb_rows {
            left_ctx.fill(0);
            new.setup_intra_recon_left(mb_row);
            for mb_col in 0..self.mb_cols {
                let cur = (mb_row + 1) * stride + mb_col + 1;
                let choice = self.select_mode(src, &new, mb_row, mb_col, key_frame);
                {
                    let m = &mut self.mi[cur];
                    m.mode = choice.mode;
                    m.ref_frame = choice.ref_frame;
                    m.mv = choice.mv;
                    m.uv_mode = choice.uv_mode;
                }
                // Symbol counts (x->ymode_count / uv_mode_count /
                // count_mb_ref_frame_usage / MVcount).
                self.counts.ymode[choice.mode as usize] += 1;
                self.counts.uv_mode[choice.uv_mode as usize] += 1;
                self.counts.ref_frames[choice.ref_frame as usize] += 1;
                if choice.mode == MbMode::NewMv {
                    adapt::update_mvcount(&mut self.counts, choice.mv, choice.best_ref_mv);
                }
                let mc = self.recon_mb(src, &mut new, mb_row, mb_col, &choice);
                let skip = tokenize_mb(
                    &mc,
                    &mut self.above_ctx[mb_col * CTX_CELLS..mb_col * CTX_CELLS + CTX_CELLS],
                    &mut left_ctx,
                    &mut self.toks,
                    &mut self.counts.coef,
                );
                self.mi[cur].mb_skip_coeff = skip;
                self.counts.skip_true += skip as u32;
                skips.push(skip);
                choices.push(choice);
            }
            new.extend_mb_row(mb_row);
            if mb_row > 0 {
                new.extend_lr_rows((mb_row - 1) * 16, 16);
            }
        }
        if self.mb_rows > 0 {
            new.extend_lr_rows((self.mb_rows - 1) * 16, 16);
        }
        new.extend_tb();

        // --- loop filter: pick the level on a mid-frame strip, then run
        // the full filter so the stored reference matches what any
        // decoder will reconstruct (vp8_loopfilter_frame runs before the
        // reference buffers move). ---
        let mut mb_modes = Vec::with_capacity(self.mb_rows * self.mb_cols);
        for r in 0..self.mb_rows {
            for c in 0..self.mb_cols {
                mb_modes.push(self.mi[(r + 1) * stride + c + 1].clone());
            }
        }
        self.filter_level = lf::pick_filter_level_fast(
            &src.y,
            src.y_stride,
            &new,
            &mb_modes,
            key_frame,
            self.quant.qindex,
            self.filter_level,
            &mut self.lft,
            self.ref_lf_deltas,
            self.mode_lf_deltas,
            &mut self.last_sharpness,
        );
        // C gates the actual filter pass on `filter_level > 0 &&
        // update_any_ref_buffers` — refresh_last is always set, so the
        // level alone decides (matching the decoder's `> 0` gate).
        if self.filter_level > 0 {
            lf::loop_filter_frame(
                &mut new,
                &mb_modes,
                self.mb_cols,
                self.mb_rows,
                key_frame,
                &mut self.lft,
                self.filter_level,
                self.ref_lf_deltas,
                self.mode_lf_deltas,
                &mut self.last_sharpness,
            );
        }

        let pkt = self.emit_packet(key_frame, &choices, &skips);
        // refresh_last = 1 always → `new` becomes the LAST reference.
        self.last = Some(new);
        pkt
    }

    /// Per-MB mode selection (encoder policy). Evaluates candidate modes
    /// by SAD + small fixed rate penalty.
    fn select_mode(
        &mut self,
        src: &SrcFrame,
        new: &FrameBuf,
        mb_row: usize,
        mb_col: usize,
        key_frame: bool,
    ) -> MbChoice {
        let cur = (mb_row + 1) * (self.mb_cols + 1) + mb_col + 1;
        let y_off = new.y_off((mb_col * 16) as isize, (mb_row * 16) as isize);
        let uv_off = new.uv_off((mb_col * 8) as isize, (mb_row * 8) as isize);
        let sy = mb_row * 16 * src.y_stride + mb_col * 16;
        let suv = mb_row * 8 * src.uv_stride + mb_col * 8;

        // ---- best intra I16 ymode (SAD) ----
        let a_off_y = new.y_off((mb_col * 16) as isize - 1, (mb_row * 16) as isize - 1);
        let above_l: [u8; 21] = new.y[a_off_y..a_off_y + 21].try_into().unwrap();
        let mut left_l = [0u8; 16];
        for (i, l) in left_l.iter_mut().enumerate() {
            *l = new.y[y_off - 1 + i * new.y_stride];
        }
        let mut ypred = [0u8; 256];
        let mut best_y = (MbMode::DcPred, u32::MAX);
        for mode in [MbMode::DcPred, MbMode::VPred, MbMode::HPred, MbMode::TmPred] {
            predict::predict_luma16(
                mode as i8,
                &mut ypred,
                16,
                &above_l,
                &left_l,
                mb_col > 0,
                mb_row > 0,
            );
            let s = sad16(&src.y[sy..], src.y_stride, &ypred, 16);
            if s < best_y.1 {
                best_y = (mode, s);
            }
        }

        // ---- best chroma uv_mode (U+V SAD) ----
        let a_off_uv = new.uv_off((mb_col * 8) as isize - 1, (mb_row * 8) as isize - 1);
        let above_c: [u8; 13] = new.u[a_off_uv..a_off_uv + 13].try_into().unwrap();
        let mut left_c = [0u8; 8];
        for (i, l) in left_c.iter_mut().enumerate() {
            *l = new.u[uv_off - 1 + i * new.uv_stride];
        }
        let mut cp = [0u8; 64];
        let mut best_uv = (DC_PRED, u32::MAX);
        for uv in [DC_PRED, V_PRED, H_PRED, TM_PRED] {
            let mut s = 0;
            predict::predict_chroma8(uv, &mut cp, 8, &above_c, &left_c, mb_col > 0, mb_row > 0);
            s += sad8(&src.u[suv..], src.uv_stride, &cp, 8);
            // V plane shares `above_c`/`left_c` layout — recompute from v
            let above_v: [u8; 13] = new.v[a_off_uv..a_off_uv + 13].try_into().unwrap();
            let mut left_v = [0u8; 8];
            for (i, l) in left_v.iter_mut().enumerate() {
                *l = new.v[uv_off - 1 + i * new.uv_stride];
            }
            predict::predict_chroma8(uv, &mut cp, 8, &above_v, &left_v, mb_col > 0, mb_row > 0);
            s += sad8(&src.v[suv..], src.uv_stride, &cp, 8);
            if s < best_uv.1 {
                best_uv = (uv, s);
            }
        }

        if key_frame {
            return MbChoice {
                mode: best_y.0,
                ref_frame: RefFrame::Intra,
                mv: Mv::ZERO,
                best_ref_mv: Mv::ZERO,
                uv_mode: best_uv.0,
            };
        }

        // ---- inter candidates ----
        let (mut near_mvs, mut cnt, nmv) =
            find_near_mvs(&self.mi, cur, self.mb_cols + 1, RefFrame::Last, &[false; 4]);
        const CNT_INTRA: usize = 0;
        const CNT_NEAREST: usize = 1;
        const CNT_NEAR: usize = 2;
        const CNT_SPLITMV: usize = 3;
        cnt[CNT_NEAREST] += ((cnt[CNT_SPLITMV] > 0)
            && (near_mvs[nmv].as_int() == near_mvs[CNT_NEAREST].as_int()))
            as i32;
        if cnt[CNT_NEAR] > cnt[CNT_NEAREST] {
            cnt.swap(CNT_NEAR, CNT_NEAREST);
            near_mvs.swap(CNT_NEAR, CNT_NEAREST);
        }
        let edges = (
            -((mb_col * 16) as i32) << 3,
            (((self.mb_cols - 1 - mb_col) * 16) as i32) << 3,
            -((mb_row * 16) as i32) << 3,
            (((self.mb_rows - 1 - mb_row) * 16) as i32) << 3,
        );
        let edges_margined = (
            edges.0 - (16 << 3),
            edges.1 + (16 << 3),
            edges.2 - (16 << 3),
            edges.3 + (16 << 3),
        );

        let last = self.last.as_ref().expect("inter frame requires last ref");
        let rf = RefView {
            y: &last.y,
            u: &last.u,
            v: &last.v,
            y_stride: last.y_stride,
            uv_stride: last.uv_stride,
        };
        // Anchors: the MB's position index in the reference's planes.
        let rf_y = last.y_off((mb_col * 16) as isize, (mb_row * 16) as isize);
        let rf_uv = last.uv_off((mb_col * 8) as isize, (mb_row * 8) as isize);
        let mut pred = PredBuf {
            y: [0; 256],
            u: [0; 64],
            v: [0; 64],
        };
        let eval = |mv: Mv, pen: u32, pred: &mut PredBuf| -> (u32, Mv) {
            let mut cand = mv;
            mc::clamp_mv2(&mut cand, edges);
            inter_pred(&rf, edges, edges_margined, rf_y, rf_uv, cand, pred);
            let s = sad16(&src.y[sy..], src.y_stride, &pred.y, 16)
                + sad8(&src.u[suv..], src.uv_stride, &pred.u, 8)
                + sad8(&src.v[suv..], src.uv_stride, &pred.v, 8)
                + pen;
            (s, cand)
        };

        // intra I16 gets an intra penalty (more bits to code)
        let mut best = (best_y.1 + best_uv.1 + 96, MbMode::DcPred, Mv::ZERO);
        let (s, mv) = eval(Mv::ZERO, 16, &mut pred);
        if s < best.0 {
            best = (s, MbMode::ZeroMv, mv);
        }
        let (s, mv) = eval(near_mvs[CNT_NEAREST], 48, &mut pred);
        if s < best.0 {
            best = (s, MbMode::NearestMv, mv);
        }
        let (s, mv) = eval(near_mvs[CNT_NEAR], 64, &mut pred);
        if s < best.0 {
            best = (s, MbMode::NearMv, mv);
        }

        // NEWMV: quarter-pel diamond search around best_mv. Candidates on
        // the grid {best + 2k} — every emitted delta is even, so the MV
        // round-trips exactly through the `>> 1` coding.
        let near_index = CNT_INTRA + (cnt[CNT_NEAREST] >= cnt[CNT_INTRA]) as usize;
        let mut best_mv = near_mvs[near_index];
        mc::clamp_mv2(&mut best_mv, edges);
        let (mut bs, mut bmv) = (u32::MAX, best_mv);
        let mut step: i32 = 32; // quarter-pel units (32 = 4px)
        let mut center = best_mv;
        while step >= 2 {
            let mut improved = false;
            for (dr, dc) in [(0, -step), (-step, 0), (step, 0), (0, step)] {
                let mut cand = Mv {
                    row: (center.row as i32 + dr) as i16,
                    col: (center.col as i32 + dc) as i16,
                };
                mc::clamp_mv2(&mut cand, edges);
                let (s, _) = eval(cand, 96, &mut pred);
                if s < bs {
                    bs = s;
                    bmv = cand;
                    improved = true;
                }
            }
            if improved {
                center = bmv;
            } else {
                step /= 2;
            }
        }
        let (s0, _) = eval(best_mv, 96, &mut pred);
        if s0 < bs {
            bs = s0;
            bmv = best_mv;
        }
        // The MV is emitted as `delta >> 1` — snap the delta to even so
        // the decoder reproduces exactly the vector we searched (edge
        // clamping inside `eval` can otherwise leave an odd delta).
        bmv = Mv {
            row: best_mv.row + ((bmv.row - best_mv.row) & !1),
            col: best_mv.col + ((bmv.col - best_mv.col) & !1),
        };
        if bs < best.0 {
            best = (bs, MbMode::NewMv, bmv);
        }

        MbChoice {
            mode: best.1,
            ref_frame: if best.1 as i8 <= MbMode::BPred as i8 {
                RefFrame::Intra
            } else {
                RefFrame::Last
            },
            mv: best.2,
            best_ref_mv: if best.1 == MbMode::NewMv {
                best_mv
            } else {
                Mv::ZERO
            },
            uv_mode: best_uv.0,
        }
    }

    /// Run the chosen mode's prediction into `new` (recon frame), compute
    /// residual → FDCT/WHT → quantize → dequant → IDCT-add for the closed
    /// loop, and return the MB's coefficient record for tokenizing.
    fn recon_mb(
        &mut self,
        src: &SrcFrame,
        new: &mut FrameBuf,
        mb_row: usize,
        mb_col: usize,
        choice: &MbChoice,
    ) -> MbCoeffs {
        let stride = new.y_stride;
        let uv_stride = new.uv_stride;
        let y_off = new.y_off((mb_col * 16) as isize, (mb_row * 16) as isize);
        let uv_off = new.uv_off((mb_col * 8) as isize, (mb_row * 8) as isize);
        let sy = mb_row * 16 * src.y_stride + mb_col * 16;
        let suv = mb_row * 8 * src.uv_stride + mb_col * 8;
        let cur = (mb_row + 1) * (self.mb_cols + 1) + mb_col + 1;

        // --- prediction into `new` (identical to the decoder's recon) ---
        if choice.ref_frame == RefFrame::Intra {
            let a_off_uv = new.uv_off((mb_col * 8) as isize - 1, (mb_row * 8) as isize - 1);
            for plane in 0..2 {
                let dst = if plane == 0 { &mut new.u } else { &mut new.v };
                let above: [u8; 13] = dst[a_off_uv..a_off_uv + 13].try_into().unwrap();
                let mut left = [0u8; 8];
                for (i, l) in left.iter_mut().enumerate() {
                    *l = dst[uv_off - 1 + i * uv_stride];
                }
                predict::predict_chroma8(
                    choice.uv_mode,
                    &mut dst[uv_off..],
                    uv_stride,
                    &above,
                    &left,
                    mb_col > 0,
                    mb_row > 0,
                );
            }
            let a_off_y = new.y_off((mb_col * 16) as isize - 1, (mb_row * 16) as isize - 1);
            let above: [u8; 21] = new.y[a_off_y..a_off_y + 21].try_into().unwrap();
            let mut left = [0u8; 16];
            for (i, l) in left.iter_mut().enumerate() {
                *l = new.y[y_off - 1 + i * stride];
            }
            predict::predict_luma16(
                choice.mode as i8,
                &mut new.y[y_off..],
                stride,
                &above,
                &left,
                mb_col > 0,
                mb_row > 0,
            );
        } else {
            let last = self.last.as_ref().unwrap();
            let rf = RefView {
                y: &last.y,
                u: &last.u,
                v: &last.v,
                y_stride: last.y_stride,
                uv_stride: last.uv_stride,
            };
            let edges = (
                -((mb_col * 16) as i32) << 3,
                (((self.mb_cols - 1 - mb_col) * 16) as i32) << 3,
                -((mb_row * 16) as i32) << 3,
                (((self.mb_rows - 1 - mb_row) * 16) as i32) << 3,
            );
            let edges_margined = (
                edges.0 - (16 << 3),
                edges.1 + (16 << 3),
                edges.2 - (16 << 3),
                edges.3 + (16 << 3),
            );
            {
                let info = &mut self.mi[cur];
                info.need_to_clamp_mvs = mc::check_mv_bounds(info.mv, edges_margined);
            }
            let info: &MbInfo = &self.mi[cur];
            mc::build_inter16x16(
                info,
                edges,
                !0,
                SubpelKind::Sixtap,
                &rf,
                &mut new.y,
                &mut new.u,
                &mut new.v,
                stride,
                uv_stride,
                y_off,
                uv_off,
            );
        }

        // --- residual → FDCT (+WHT for Y2) → quantize → recon add ---
        let mut mc_out = MbCoeffs {
            qcoeff: [[0; 16]; 25],
            eobs: [0; 25],
            has_y2: true,
        };
        let mut luma_dq = [[0i16; 16]; 16];

        // Luma FDCT blocks; DCs → Y2 input.
        let mut y2diff = [0i16; 16];
        for b in 0..16usize {
            let b_off = y_off + (b >> 2) * 4 * stride + (b & 3) * 4;
            let s_off = sy + (b >> 2) * 4 * src.y_stride + (b & 3) * 4;
            let mut residual = [0i16; 16];
            for r in 0..4 {
                for c in 0..4 {
                    residual[r * 4 + c] = src.y[s_off + r * src.y_stride + c] as i16
                        - new.y[b_off + r * stride + c] as i16;
                }
            }
            let mut coeff = [0i16; 16];
            dct::fdct4x4(&residual, 4, &mut coeff);
            y2diff[b] = coeff[0];
            mc_out.eobs[b] = regular_quantize_b(
                &coeff,
                &self.quant.y1,
                &mut mc_out.qcoeff[b],
                &mut luma_dq[b],
            );
        }

        // Y2: forward WHT of the 16 luma DCs → quantize.
        let mut y2coeff = [0i16; 16];
        dct::walsh4x4(&y2diff, &mut y2coeff);
        let mut y2dq = [0i16; 16];
        mc_out.eobs[24] =
            regular_quantize_b(&y2coeff, &self.quant.y2, &mut mc_out.qcoeff[24], &mut y2dq);
        // Inverse WHT → dequantized luma DCs (into dc_recon[i]).
        let mut dc_scatter = [0i16; 256];
        if mc_out.eobs[24] > 1 {
            crate::idct::inv_walsh4x4(&y2dq, &mut dc_scatter);
        } else {
            crate::idct::inv_walsh4x4_1(y2dq[0] as i32, &mut dc_scatter);
        }

        // Luma recon: DC from the WHT scatter, ACs dequantized (y1 table).
        for b in 0..16usize {
            let b_off = y_off + (b >> 2) * 4 * stride + (b & 3) * 4;
            luma_dq[b][0] = dc_scatter[b * 16];
            if mc_out.eobs[b] > 1 {
                idct_add(&mut luma_dq[b], &mut new.y[b_off..], stride);
            } else {
                crate::idct::dc_only_idct_add(luma_dq[b][0] as i32, &mut new.y[b_off..], stride);
            }
        }

        // Chroma: 8 FDCT blocks (4 U + 4 V) → quantize → recon.
        for plane in 0..2usize {
            let (splane, dplane) = if plane == 0 {
                (&src.u, &mut new.u)
            } else {
                (&src.v, &mut new.v)
            };
            for i in 0..2 {
                for j in 0..2 {
                    let b = plane * 4 + i * 2 + j;
                    let s_off = suv + i * 4 * src.uv_stride + j * 4;
                    let b_off = uv_off + i * 4 * uv_stride + j * 4;
                    let mut residual = [0i16; 16];
                    for r in 0..4 {
                        for c in 0..4 {
                            residual[r * 4 + c] = splane[s_off + r * src.uv_stride + c] as i16
                                - dplane[b_off + r * uv_stride + c] as i16;
                        }
                    }
                    let mut coeff = [0i16; 16];
                    dct::fdct4x4(&residual, 4, &mut coeff);
                    let mut dq = [0i16; 16];
                    mc_out.eobs[16 + b] = regular_quantize_b(
                        &coeff,
                        &self.quant.uv,
                        &mut mc_out.qcoeff[16 + b],
                        &mut dq,
                    );
                    if mc_out.eobs[16 + b] > 1 {
                        idct_add(&mut dq, &mut dplane[b_off..], uv_stride);
                    } else {
                        crate::idct::dc_only_idct_add(
                            dq[0] as i32,
                            &mut dplane[b_off..],
                            uv_stride,
                        );
                    }
                }
            }
        }

        mc_out
    }

    /// Emit the assembled packet: tag + kf preamble + partition-0 (header +
    /// modes) + token partition. Probability updates mutate `self.fc` as
    /// they are emitted so the token partition and the next frame both see
    /// the post-update context (refresh_entropy = 1 is always signalled).
    fn emit_packet(&mut self, key_frame: bool, choices: &[MbChoice], skips: &[bool]) -> Vec<u8> {
        if std::env::var_os("VP8_SYMLOG").is_some() {
            eprintln!("START 0");
        }
        let mut w = BoolWriter::with_id(0);

        // --- header (mirrors parse_partition0's read order) ---
        if key_frame {
            w.write(0, 128); // colorspace = 0
            w.write(0, 128); // clamp_type = 0
        }
        w.write(0, 128); // segmentation_enabled = 0
        w.write(0, 128); // filter_type = normal
        w.literal(self.filter_level as u32, 6);
        w.literal(0, 3); // sharpness_level = 0

        // Loop-filter deltas (bitstream.c pack header block):
        // enabled persists from the KF `setup_features`; `send_update`
        // follows `mode_ref_lf_delta_update`; per-delta flags fire only
        // when the value differs from the last emitted one.
        w.write(self.lf_delta_enabled as i32, 128);
        if self.lf_delta_enabled {
            let send_update = self.lf_delta_update;
            w.write(send_update as i32, 128);
            if send_update {
                for i in 0..4 {
                    if self.ref_lf_deltas[i] != self.last_ref_lf_deltas[i] {
                        self.last_ref_lf_deltas[i] = self.ref_lf_deltas[i];
                        w.write(1, 128);
                        let d = self.ref_lf_deltas[i];
                        w.literal((d.unsigned_abs() as u32) & 0x3f, 6);
                        w.write((d < 0) as i32, 128);
                    } else {
                        w.write(0, 128);
                    }
                }
                for i in 0..4 {
                    if self.mode_lf_deltas[i] != self.last_mode_lf_deltas[i] {
                        self.last_mode_lf_deltas[i] = self.mode_lf_deltas[i];
                        w.write(1, 128);
                        let d = self.mode_lf_deltas[i];
                        w.literal((d.unsigned_abs() as u32) & 0x3f, 6);
                        w.write((d < 0) as i32, 128);
                    } else {
                        w.write(0, 128);
                    }
                }
            }
        }

        w.literal(0, 2); // multi_token_partition = 0
        w.literal(self.quant.qindex as u32, 7);
        for _ in 0..5 {
            w.write(0, 128); // no q deltas
        }
        if !key_frame {
            w.write(0, 128); // refresh_golden = 0
            w.write(0, 128); // refresh_alt_ref = 0
            w.literal(0, 2); // copy_buffer_to_gf = 0
            w.literal(0, 2); // copy_buffer_to_arf = 0
            w.write(0, 128); // sign_bias golden
            w.write(0, 128); // sign_bias altref
        }
        w.write(1, 128); // refresh_entropy = 1
        if !key_frame {
            w.write(1, 128); // refresh_last = 1
        }

        // Coefficient probability updates (vp8_update_coef_probs) — emits
        // per-context update flags and mutates fc.coef_probs where the
        // savings justify an update.
        adapt::update_coef_probs(
            &mut w,
            &mut self.fc,
            &self.counts,
            &COEFF_UPDATE_PROBS,
            &self.enc_coef,
            &COEF_TREE,
        );

        // mb_mode_mv_init: `prob_skip_false` from this frame's skip count
        // (pack_inter_mode_mvs / write_kfmodes both emit it this way).
        w.write(1, 128); // mb_no_coeff_skip = 1
        let mbs = self.counts.mbs;
        let prob_skip_false = adapt::prob_from_total(mbs - self.counts.skip_true, mbs);
        w.literal(prob_skip_false as u32, 8);

        let mut probs = (0u8, 0u8, 0u8); // prob_intra, prob_last, prob_gf
        if !key_frame {
            probs = adapt::convert_rfct_to_prob(self.counts.ref_frames);
            w.literal(probs.0 as u32, 8);
            w.literal(probs.1 as u32, 8);
            w.literal(probs.2 as u32, 8);

            // update_mbintra_mode_probs: whole-tree ymode/uvmode updates.
            adapt::update_mode(
                &mut w,
                &self.enc_ymode,
                &YMODE_TREE,
                &mut self.fc.ymode_prob,
                &self.counts.ymode,
            );
            adapt::update_mode(
                &mut w,
                &self.enc_uv,
                &UV_MODE_TREE,
                &mut self.fc.uv_mode_prob,
                &self.counts.uv_mode,
            );
            // vp8_write_mvprobs — per-component MV prob updates.
            adapt::write_mvprobs(
                &mut w,
                &self.counts,
                &mut self.fc,
                &self.enc_mvshort,
                &SMALL_MV_TREE,
            );
        }

        // --- per-MB modes (raster — decoder order) ---
        let stride = self.mb_cols + 1;
        let sign_bias = [false; 4];
        for mb_row in 0..self.mb_rows {
            for mb_col in 0..self.mb_cols {
                let idx = mb_row * self.mb_cols + mb_col;
                let cur = (mb_row + 1) * stride + mb_col + 1;
                let ch = &choices[idx];
                w.write(skips[idx] as i32, prob_skip_false);
                if key_frame {
                    emit_tree(
                        &mut w,
                        &KEYFRAME_YMODE_TREE,
                        &KEYFRAME_YMODE_PROBS,
                        ch.mode as i8,
                    );
                    emit_tree(
                        &mut w,
                        &KEYFRAME_UV_MODE_TREE,
                        &KEYFRAME_UV_MODE_PROBS,
                        ch.uv_mode,
                    );
                } else if ch.ref_frame == RefFrame::Intra {
                    w.write(0, probs.0); // ref_frame = intra
                    emit_tree(&mut w, &YMODE_TREE, &self.fc.ymode_prob, ch.mode as i8);
                    emit_tree(&mut w, &UV_MODE_TREE, &self.fc.uv_mode_prob, ch.uv_mode);
                } else {
                    w.write(1, probs.0); // inter
                    w.write(0, probs.1); // ref = LAST
                    let (mut near_mvs, mut cnt, nmv) =
                        find_near_mvs(&self.mi, cur, stride, RefFrame::Last, &sign_bias);
                    const CNT_INTRA: usize = 0;
                    const CNT_NEAREST: usize = 1;
                    const CNT_NEAR: usize = 2;
                    const CNT_SPLITMV: usize = 3;
                    if ch.mode == MbMode::ZeroMv {
                        w.write(0, MODE_CONTEXTS[cnt[CNT_INTRA] as usize][0]);
                    } else {
                        w.write(1, MODE_CONTEXTS[cnt[CNT_INTRA] as usize][0]);
                        cnt[CNT_NEAREST] += ((cnt[CNT_SPLITMV] > 0)
                            && (near_mvs[nmv].as_int() == near_mvs[CNT_NEAREST].as_int()))
                            as i32;
                        if cnt[CNT_NEAR] > cnt[CNT_NEAREST] {
                            cnt.swap(CNT_NEAR, CNT_NEAREST);
                            near_mvs.swap(CNT_NEAR, CNT_NEAREST);
                        }
                        match ch.mode {
                            MbMode::NearestMv => {
                                w.write(0, MODE_CONTEXTS[cnt[CNT_NEAREST] as usize][1]);
                            }
                            MbMode::NearMv => {
                                w.write(1, MODE_CONTEXTS[cnt[CNT_NEAREST] as usize][1]);
                                w.write(0, MODE_CONTEXTS[cnt[CNT_NEAR] as usize][2]);
                            }
                            MbMode::NewMv => {
                                w.write(1, MODE_CONTEXTS[cnt[CNT_NEAREST] as usize][1]);
                                w.write(1, MODE_CONTEXTS[cnt[CNT_NEAR] as usize][2]);
                                let cnt_split = (((self.mi[cur - stride].mode == MbMode::SplitMv)
                                    as i32
                                    + (self.mi[cur - 1].mode == MbMode::SplitMv) as i32)
                                    * 2
                                    + (self.mi[cur - stride - 1].mode == MbMode::SplitMv) as i32)
                                    as usize;
                                w.write(0, MODE_CONTEXTS[cnt_split][3]);
                                encode_mv(
                                    &mut w,
                                    Mv {
                                        row: ch.mv.row - ch.best_ref_mv.row,
                                        col: ch.mv.col - ch.best_ref_mv.col,
                                    },
                                    &self.fc.mvc,
                                );
                            }
                            _ => unreachable!("v1 encoder emits only ZEROMV/NEAREST/NEAR/NEWMV"),
                        }
                    }
                }
            }
        }
        let part0 = w.finish();

        // --- token partition (single partition — no size table) ---
        if std::env::var_os("VP8_SYMLOG").is_some() {
            eprintln!("START 1");
        }
        let mut tw = BoolWriter::with_id(1);
        pack_tokens(&mut tw, &self.toks, &self.fc.coef_probs);
        let part1 = tw.finish();

        // --- assemble ---
        let mut pkt = Vec::with_capacity(10 + part0.len() + part1.len());
        let tag = ((part0.len() as u32) << 5) | (1u32 << 4) | (!key_frame) as u32;
        pkt.extend_from_slice(&[
            (tag & 0xff) as u8,
            ((tag >> 8) & 0xff) as u8,
            ((tag >> 16) & 0xff) as u8,
        ]);
        if key_frame {
            pkt.extend_from_slice(&[0x9d, 0x01, 0x2a]);
            let (w16, h16) = (self.cfg.width as u16, self.cfg.height as u16);
            pkt.extend_from_slice(&[
                (w16 & 0xff) as u8,
                ((w16 >> 8) & 0x3f) as u8,
                (h16 & 0xff) as u8,
                ((h16 >> 8) & 0x3f) as u8,
            ]);
        }
        pkt.extend_from_slice(&part0);
        pkt.extend_from_slice(&part1);
        pkt
    }
}

/// `idct4x4_add` wrapper — input is already dequantized (the encoder
/// dequantizes via the quantizer's `dqcoeff`, not the decoder's
/// `dequant_idct_add`).
fn idct_add(dq: &mut [i16; 16], dst: &mut [u8], stride: usize) {
    crate::idct::idct_add_block(dq, dst, stride);
}
