//! Top-level decoder — port of `vp8_decode_frame` + `decode_mb_rows` +
//! `decode_macroblock` + `swap_frame_buffers` orchestration.
//!
//! The push model: feed one complete VP8 frame payload (no container
//! headers) to [`Vp8Decoder::decode`]; pull decoded frames via
//! [`Vp8Decoder::next_frame`]. Non-`show_frame` packets update reference
//! state without producing output.

use std::collections::VecDeque;

use crate::boold::BoolReader;
use crate::error::DecodeError;
use crate::framebuf::{FrameBuf, RefRole};
use crate::header::{self, FrameHeader};
use crate::idct;
use crate::inter;
use crate::loopfilter::{self, LoopFilterTables, MODE_LF_LUT};
use crate::mc::{self, RefView, SubpelKind};
use crate::predict;
use crate::tables::{AC_QUANT, DC_QUANT, VP8_AC_TABLE2};
use crate::tokens;
use crate::types::{FrameContext, LfDeltas, MbInfo, MbMode, RefFrame, Segmentation};

/// A decoded video frame — tightly packed planar I420 (visible dims).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct DecodedFrame {
    /// Visible width/height in pixels.
    pub width: usize,
    pub height: usize,
    /// `width * height` luma.
    pub y: Vec<u8>,
    /// `ceil(w/2) * ceil(h/2)` chroma.
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    /// libvpx `yv12_fb->corrupted` — residual/reference corruption latch.
    pub corrupted: bool,
}

/// Per-segment resolved dequant tables (`xd->dequant_*` after
/// `vp8_mb_init_dequantizer`).
#[derive(Clone, Copy)]
struct SegDq {
    y1: [i16; 16],
    /// `dequant_y1_dc` — DC cell forced to 1 for the post-WHT path.
    y1dc: [i16; 16],
    y2: [i16; 16],
    uv: [i16; 16],
}

#[inline]
fn clamp_q(q: i32) -> usize {
    q.clamp(0, 127) as usize
}

impl SegDq {
    /// `vp8cx_init_de_quantizer` row + `vp8_mb_init_dequantizer` for one
    /// resolved QIndex. `deltas` = [y1dc, y2dc, y2ac, uvdc, uvac].
    fn for_qindex(q: usize, deltas: &[i32; 5]) -> Self {
        let mut y1 = [0i16; 16];
        let mut y1dc = [0i16; 16];
        let mut y2 = [0i16; 16];
        let mut uv = [0i16; 16];

        y1[0] = DC_QUANT[clamp_q(q as i32 + deltas[0])];
        // vp8_dc2quant: dc_qlookup[q] * 2
        y2[0] = DC_QUANT[clamp_q(q as i32 + deltas[1])].saturating_mul(2);
        // vp8_dc_uv_quant: dc_qlookup clamped to 132
        uv[0] = DC_QUANT[clamp_q(q as i32 + deltas[3])].min(132);

        let ac = AC_QUANT[q];
        // vp8_ac2quant: (ac * 101581) >> 16, min 8 — precomputed table
        let ac2 = VP8_AC_TABLE2[clamp_q(q as i32 + deltas[2])] as i16;
        let acuv = AC_QUANT[clamp_q(q as i32 + deltas[4])];

        for i in 1..16 {
            y1[i] = ac;
            y1dc[i] = ac;
            y2[i] = ac2;
            uv[i] = acuv;
        }
        y1dc[0] = 1; // DC was already scaled through the Y2 path
        SegDq { y1, y1dc, y2, uv }
    }
}

/// Stateless VP8 bitstream decoder (push/pull).
pub struct Vp8Decoder {
    /// Working entropy context (`pc->fc`).
    fc: FrameContext,
    /// Saved copy for `refresh_entropy_probs == 0` (`pc->lfc`).
    lfc: FrameContext,
    lfd: LfDeltas,
    seg: Segmentation,
    lft: LoopFilterTables,
    last_sharpness: i32,

    /// `yv12_fb[0..4]` — index order matches [`RefRole`].
    refs: Option<Box<[FrameBuf; 4]>>,
    /// `pc->mip` mode grid, `(mb_rows + 1) * (mb_cols + 1)` — persistent:
    /// segment ids survive across frames when the map isn't refreshed.
    mi: Vec<MbInfo>,
    /// `pc->above_context` — 9 cells per MB column.
    above_ctx: Vec<u8>,

    /// Macroblock-count allocation ceiling (corrupt-stream resource limit).
    max_mbs: usize,

    mb_rows: usize,
    mb_cols: usize,
    width: usize,
    height: usize,

    decoded_key_frame: bool,
    out: VecDeque<DecodedFrame>,
}

impl Default for Vp8Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Vp8Decoder {
    pub fn new() -> Self {
        Vp8Decoder {
            fc: FrameContext::new(),
            lfc: FrameContext::new(),
            lfd: LfDeltas::default(),
            seg: Segmentation::default(),
            lft: LoopFilterTables::default(),
            last_sharpness: -1,
            refs: None,
            mi: Vec::new(),
            above_ctx: Vec::new(),
            max_mbs: Self::DEFAULT_MAX_MBS,
            mb_rows: 0,
            mb_cols: 0,
            width: 0,
            height: 0,
            decoded_key_frame: false,
            out: VecDeque::new(),
        }
    }

    /// Default macroblock ceiling: 65536 MBs ≈ 4096×4096 px of coded area —
    /// covers every realistic VP8 stream while bounding hostile headers to
    /// ~100 MB of reference buffers instead of the ~1.7 GB a 14-bit
    /// 16383×16383 claim would otherwise allocate.
    const DEFAULT_MAX_MBS: usize = 65536;

    /// Decoder with a custom macroblock-count ceiling. Each 16×16 MB needs
    /// ~1.5 KB of reference storage across the four-frame pool; the default
    /// (65536 MBs) bounds allocation to ~100 MB.
    pub fn with_max_macroblocks(max_mbs: usize) -> Self {
        let mut d = Self::new();
        d.max_mbs = max_mbs;
        d
    }

    /// Drop all decoder state (next packet must be a keyframe).
    pub fn reset(&mut self) {
        let max_mbs = self.max_mbs;
        *self = Self::new();
        self.max_mbs = max_mbs;
    }

    /// Push one complete VP8 frame payload.
    pub fn decode(&mut self, pkt: &[u8]) -> Result<(), DecodeError> {
        self.decode_frame(pkt)
    }

    /// Pop the next decoded frame (if a `show_frame` packet was pushed).
    pub fn next_frame(&mut self) -> Option<DecodedFrame> {
        self.out.pop_front()
    }

    /// Visible dimensions of the most recent keyframe.
    pub fn dimensions(&self) -> Option<(usize, usize)> {
        (self.width != 0).then_some((self.width, self.height))
    }

    fn decode_frame(&mut self, pkt: &[u8]) -> Result<(), DecodeError> {
        let tag = header::parse_tag(pkt)?;

        if !tag.key_frame && !self.decoded_key_frame {
            return Err(DecodeError::MissingKeyframe);
        }

        if tag.key_frame {
            // init_frame() keyframe branch — entropy + feature state reset.
            self.fc = FrameContext::new();
            self.seg = Segmentation::default();
            self.lfd = LfDeltas::default();
            self.ensure_geometry(tag.width, tag.height)?;
        }

        // Partition-0 bool decoder spans the *rest* of the packet.
        let mut bc = BoolReader::new(&pkt[tag.data_start..]);
        let (hdr, token_parts) = header::parse_partition0(
            &mut bc,
            pkt,
            &tag,
            tag.key_frame,
            &mut self.fc,
            &mut self.seg,
            &mut self.lfd,
            &mut self.lfc,
        )?;

        // Per-segment dequant tables (`vp8_mb_init_dequantizer` per seg).
        let mut dq = [SegDq::for_qindex(0, &[0; 5]); 4];
        for (seg_id, d) in dq.iter_mut().enumerate() {
            let q = if self.seg.enabled {
                let feat = self.seg.feature_data[0][seg_id] as i32;
                if self.seg.abs_delta {
                    feat
                } else {
                    hdr.base_qindex + feat
                }
            } else {
                hdr.base_qindex
            };
            *d = SegDq::for_qindex(clamp_q(q), &hdr.q_deltas);
        }

        // --- mode & MV pass over partition 0 (vp8_decode_mode_mvs) ---
        let mut corrupted = inter::decode_mode_mvs(
            &mut bc,
            &mut self.mi,
            self.mb_rows,
            self.mb_cols,
            &mut self.fc,
            &hdr.sign_bias,
            self.seg.enabled,
            hdr.seg_update_map,
            &self.seg.tree_probs,
            tag.key_frame,
        );

        // --- token decoders for each coefficient partition ---
        let mut bcs: Vec<BoolReader> = token_parts
            .iter()
            .map(|&(off, len)| BoolReader::new(&pkt[off..off + len]))
            .collect();

        for c in self.above_ctx.iter_mut() {
            *c = 0;
        }

        // --- recon rows (decode_mb_rows) ---
        {
            let Self {
                refs,
                mi,
                above_ctx,
                lft,
                last_sharpness,
                seg,
                lfd,
                mb_rows,
                mb_cols,
                ..
            } = self;
            let refs = refs.as_mut().unwrap();
            let (new_s, old) = refs.split_at_mut(1);
            let new = &mut new_s[0];
            let old: &[FrameBuf; 3] = (&*old).try_into().unwrap();
            decode_mb_rows(
                new,
                old,
                mi,
                above_ctx,
                lft,
                last_sharpness,
                seg,
                lfd,
                *mb_rows,
                *mb_cols,
                &hdr,
                &dq,
                &self.fc,
                &mut bcs,
                &mut corrupted,
            );
        }

        corrupted |= bc.error();

        // `decoded_key_frame` gate — a stream must start with a complete
        // keyframe; corrupt first frames hard-error like libvpx.
        if !self.decoded_key_frame {
            if tag.key_frame && !corrupted {
                self.decoded_key_frame = true;
            } else {
                return Err(DecodeError::MissingKeyframe);
            }
        }

        self.refs.as_mut().unwrap()[RefRole::New as usize].corrupted = corrupted;

        if !hdr.refresh_entropy {
            self.fc = self.lfc.clone();
        }

        self.swap_frame_buffers(&hdr);

        if hdr.show_frame {
            self.out.push_back(self.extract_frame());
        }
        Ok(())
    }

    /// Allocate/resize the reference set + mode grid (keyframe path only —
    /// VP8 dimensions are only signalled on keyframes).
    fn ensure_geometry(&mut self, w: usize, h: usize) -> Result<(), DecodeError> {
        let mb_cols = w.div_ceil(16);
        let mb_rows = h.div_ceil(16);
        if self.mb_cols == mb_cols
            && self.mb_rows == mb_rows
            && self.width == w
            && self.height == h
            && self.refs.is_some()
        {
            return Ok(());
        }
        if mb_cols
            .checked_mul(mb_rows)
            .is_none_or(|mbs| mbs > self.max_mbs)
        {
            return Err(DecodeError::TooLarge);
        }
        self.mb_cols = mb_cols;
        self.mb_rows = mb_rows;
        self.width = w;
        self.height = h;
        let fb = || FrameBuf::new(mb_cols * 16, mb_rows * 16);
        self.refs = Some(Box::new([fb(), fb(), fb(), fb()]));
        // calloc'd once — border cells stay INTRA/DC_PRED/zero-MV forever.
        self.mi = vec![MbInfo::new(); (mb_rows + 1) * (mb_cols + 1)];
        self.above_ctx = vec![0; tokens::CTX_CELLS * mb_cols];
        Ok(())
    }

    /// `swap_frame_buffers` — apply copy/refresh flags. Copies read
    /// pre-refresh role contents (libvpx index-swap order).
    fn swap_frame_buffers(&mut self, hdr: &FrameHeader) {
        let refs = self.refs.as_mut().unwrap();
        let (n, l, g, a) = (
            RefRole::New as usize,
            RefRole::Last as usize,
            RefRole::Golden as usize,
            RefRole::AltRef as usize,
        );
        match hdr.copy_buffer_to_arf {
            1 => refs[a] = refs[l].clone(),
            2 => refs[a] = refs[g].clone(),
            _ => {}
        }
        match hdr.copy_buffer_to_gf {
            1 => refs[g] = refs[l].clone(),
            2 => refs[g] = refs[a].clone(),
            _ => {}
        }
        if hdr.refresh_golden {
            refs[g] = refs[n].clone();
        }
        if hdr.refresh_alt_ref {
            refs[a] = refs[n].clone();
        }
        if hdr.refresh_last {
            refs[l] = refs[n].clone();
        }
    }

    /// Pull the visible region out of the `New` buffer.
    fn extract_frame(&self) -> DecodedFrame {
        let f = &self.refs.as_ref().unwrap()[RefRole::New as usize];
        let (w, h) = (self.width, self.height);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let mut y = Vec::with_capacity(w * h);
        let mut u = Vec::with_capacity(cw * ch);
        let mut v = Vec::with_capacity(cw * ch);
        for r in 0..h {
            let s = f.y_off(0, r as isize);
            y.extend_from_slice(&f.y[s..s + w]);
        }
        for r in 0..ch {
            let s = f.uv_off(0, r as isize);
            u.extend_from_slice(&f.u[s..s + cw]);
            v.extend_from_slice(&f.v[s..s + cw]);
        }
        DecodedFrame {
            width: w,
            height: h,
            y,
            u,
            v,
            corrupted: f.corrupted,
        }
    }
}

/// `decode_mb_rows` — main reconstruction loop (free function so the `new`
/// /reference split borrows compose cleanly).
#[allow(clippy::too_many_arguments)]
fn decode_mb_rows(
    new: &mut FrameBuf,
    old: &[FrameBuf; 3],
    mi: &mut [MbInfo],
    above_ctx: &mut [u8],
    lft: &mut LoopFilterTables,
    last_sharpness: &mut i32,
    seg: &Segmentation,
    lfd: &LfDeltas,
    mb_rows: usize,
    mb_cols: usize,
    hdr: &FrameHeader,
    dq: &[SegDq; 4],
    fc: &FrameContext,
    bcs: &mut [BoolReader],
    corrupted: &mut bool,
) {
    let num_part = bcs.len();
    let stride = mb_cols + 1;

    let subpel = if hdr.use_bilinear_mc {
        SubpelKind::Bilinear
    } else {
        SubpelKind::Sixtap
    };
    let fullpixel_mask: i32 = if hdr.full_pixel { !7 } else { !0 };

    if hdr.filter_level > 0 {
        lft.frame_init(
            hdr.sharpness_level as i32,
            last_sharpness,
            hdr.filter_level as i32,
            seg.enabled,
            seg.abs_delta,
            seg.feature_data[1],
            lfd.enabled,
            lfd.ref_deltas,
            lfd.mode_deltas,
        );
    }

    new.setup_intra_recon_top_line();

    let mut left_ctx = [0u8; tokens::CTX_CELLS];
    let mut qcoeff = [0i16; 400];
    let mut eobs = [0u8; 25];

    for mb_row in 0..mb_rows {
        let bc = &mut bcs[mb_row % num_part];
        left_ctx.fill(0);
        new.setup_intra_recon_left(mb_row);

        for mb_col in 0..mb_cols {
            let cur = (mb_row + 1) * stride + mb_col + 1;

            // --- residual token decode (decode_macroblock head) ---
            if mi[cur].mb_skip_coeff {
                tokens::reset_mb_tokens_context(
                    &mut above_ctx[mb_col * 9..mb_col * 9 + 9],
                    &mut left_ctx,
                    mi[cur].mode.is_4x4(),
                );
                eobs.fill(0);
            } else if !bc.error() {
                let eobtotal = tokens::decode_mb_tokens(
                    bc,
                    &fc.coef_probs,
                    mi[cur].mode.is_4x4(),
                    &mut above_ctx[mb_col * 9..mb_col * 9 + 9],
                    &mut left_ctx,
                    &mut qcoeff,
                    &mut eobs,
                );
                mi[cur].mb_skip_coeff = eobtotal == 0;
            } else {
                *corrupted = true;
            }

            let mbmi: &MbInfo = &mi[cur];
            let sdq = &dq[mbmi.segment_id as usize & 3];

            let y_off = new.y_off((mb_col * 16) as isize, (mb_row * 16) as isize);
            let uv_off = new.uv_off((mb_col * 8) as isize, (mb_row * 8) as isize);

            // MB edge distances in eighth-pels (for UMV clamping)
            let edges = (
                -((mb_col * 16) as i32) << 3,
                (((mb_cols - 1 - mb_col) * 16) as i32) << 3,
                -((mb_row * 16) as i32) << 3,
                (((mb_rows - 1 - mb_row) * 16) as i32) << 3,
            );

            if mbmi.ref_frame == RefFrame::Intra {
                // chroma first (`vp8_build_intra_predictors_mbuv_s`)
                let stride_uv = new.uv_stride;
                let a_off = new.uv_off((mb_col * 8) as isize - 1, (mb_row * 8) as isize - 1);
                for plane in 0..2 {
                    let dst = if plane == 0 { &mut new.u } else { &mut new.v };
                    let above: [u8; 13] = dst[a_off..a_off + 13].try_into().unwrap();
                    let mut left = [0u8; 8];
                    for (i, l) in left.iter_mut().enumerate() {
                        *l = dst[uv_off - 1 + i * stride_uv];
                    }
                    predict::predict_chroma8(
                        mbmi.uv_mode,
                        &mut dst[uv_off..],
                        stride_uv,
                        &above,
                        &left,
                        mb_col > 0,
                        mb_row > 0,
                    );
                }

                if mbmi.mode != MbMode::BPred {
                    let a_off = new.y_off((mb_col * 16) as isize - 1, (mb_row * 16) as isize - 1);
                    let above: [u8; 21] = new.y[a_off..a_off + 21].try_into().unwrap();
                    let mut left = [0u8; 16];
                    for (i, l) in left.iter_mut().enumerate() {
                        *l = new.y[y_off - 1 + i * new.y_stride];
                    }
                    predict::predict_luma16(
                        mbmi.mode as i8,
                        &mut new.y[y_off..],
                        new.y_stride,
                        &above,
                        &left,
                        mb_col > 0,
                        mb_row > 0,
                    );
                } else {
                    // B_PRED: predict + recon each 4x4 in raster order.
                    predict::intra_prediction_down_copy(&mut new.y, new.y_stride, y_off);
                    for b in 0..16 {
                        let b_off = y_off + (b >> 2) * 4 * new.y_stride + (b & 3) * 4;
                        let a_off = b_off - new.y_stride - 1;
                        let above: [u8; 9] = new.y[a_off..a_off + 9].try_into().unwrap();
                        let mut left = [0u8; 4];
                        for (i, l) in left.iter_mut().enumerate() {
                            *l = new.y[b_off - 1 + i * new.y_stride];
                        }
                        predict::predict_4x4(
                            mbmi.bmi[b].mode,
                            &mut new.y[b_off..],
                            new.y_stride,
                            &above,
                            &left,
                        );
                        if eobs[b] > 1 {
                            let mut q16 = [0i16; 16];
                            q16.copy_from_slice(&qcoeff[b * 16..b * 16 + 16]);
                            idct::dequant_idct_add(
                                &mut q16,
                                &sdq.y1,
                                &mut new.y[b_off..],
                                new.y_stride,
                            );
                            qcoeff[b * 16..b * 16 + 16].copy_from_slice(&q16);
                        } else if eobs[b] == 1 {
                            idct::dc_only_idct_add(
                                qcoeff[b * 16] as i32 * sdq.y1[0] as i32,
                                &mut new.y[b_off..],
                                new.y_stride,
                            );
                            qcoeff[b * 16] = 0;
                            qcoeff[b * 16 + 1] = 0;
                        }
                    }
                }
            } else {
                // inter — propagate reference corruption, then MC.
                *corrupted |= old[mbmi.ref_frame as usize - 1].corrupted;
                let rf = &old[mbmi.ref_frame as usize - 1];
                let view = RefView {
                    y: &rf.y,
                    u: &rf.u,
                    v: &rf.v,
                    y_stride: rf.y_stride,
                    uv_stride: rf.uv_stride,
                };
                if mbmi.mode == MbMode::SplitMv {
                    mc::build_inter4x4(
                        mbmi,
                        edges,
                        fullpixel_mask,
                        subpel,
                        &view,
                        &mut new.y,
                        &mut new.u,
                        &mut new.v,
                        new.y_stride,
                        new.uv_stride,
                        y_off,
                        uv_off,
                    );
                } else {
                    mc::build_inter16x16(
                        mbmi,
                        edges,
                        fullpixel_mask,
                        subpel,
                        &view,
                        &mut new.y,
                        &mut new.u,
                        &mut new.v,
                        new.y_stride,
                        new.uv_stride,
                        y_off,
                        uv_off,
                    );
                }
            }

            // --- residual add (non-B_PRED luma path uses the Y2 stage) ---
            if !mbmi.mb_skip_coeff && mbmi.mode != MbMode::BPred {
                let y_dq = if mbmi.mode != MbMode::SplitMv {
                    let y2 = 24 * 16;
                    let mut w = [0i16; 16];
                    w.copy_from_slice(&qcoeff[y2..y2 + 16]);
                    if eobs[24] > 1 {
                        let mut d = [0i16; 16];
                        idct::dequantize_b(&w, &sdq.y2, &mut d);
                        idct::inv_walsh4x4(&d, &mut qcoeff);
                        qcoeff[y2..y2 + 16].fill(0);
                    } else {
                        idct::inv_walsh4x4_1(w[0] as i32 * sdq.y2[0] as i32, &mut qcoeff);
                        qcoeff[y2] = 0;
                        qcoeff[y2 + 1] = 0;
                    }
                    &sdq.y1dc
                } else {
                    &sdq.y1
                };
                idct::dequant_idct_add_y_block(
                    &mut qcoeff,
                    y_dq,
                    &mut new.y,
                    y_off,
                    new.y_stride,
                    eobs[..16].try_into().unwrap(),
                );
            }
            if !mbmi.mb_skip_coeff {
                idct::dequant_idct_add_uv_block(
                    &mut qcoeff[16 * 16..],
                    &sdq.uv,
                    &mut new.u,
                    &mut new.v,
                    uv_off,
                    uv_off,
                    new.uv_stride,
                    &eobs[16..],
                );
            }

            *corrupted |= bc.error();
        }

        new.extend_mb_row(mb_row);

        if hdr.filter_level > 0 && mb_row > 0 {
            filter_row(
                new,
                mi,
                lft,
                mb_row - 1,
                stride,
                mb_cols,
                hdr.filter_type_simple,
                hdr.key_frame,
            );
            if mb_row > 1 {
                new.extend_lr_rows((mb_row - 2) * 16, 16);
            }
        } else if mb_row > 0 {
            new.extend_lr_rows((mb_row - 1) * 16, 16);
        }
    }

    if hdr.filter_level > 0 && mb_rows > 0 {
        filter_row(
            new,
            mi,
            lft,
            mb_rows - 1,
            stride,
            mb_cols,
            hdr.filter_type_simple,
            hdr.key_frame,
        );
        if mb_rows > 1 {
            new.extend_lr_rows((mb_rows - 2) * 16, 16);
        }
    }
    if mb_rows > 0 {
        new.extend_lr_rows((mb_rows - 1) * 16, 16);
    }
    new.extend_tb();
}

/// `vp8_loop_filter_row_normal`/`_simple` for one MB row.
#[allow(clippy::too_many_arguments)]
fn filter_row(
    new: &mut FrameBuf,
    mi: &[MbInfo],
    lft: &LoopFilterTables,
    mb_row: usize,
    stride: usize,
    mb_cols: usize,
    simple: bool,
    kf: bool,
) {
    for mb_col in 0..mb_cols {
        let m = &mi[(mb_row + 1) * stride + mb_col + 1];
        let skip_lf = m.mode != MbMode::BPred && m.mode != MbMode::SplitMv && m.mb_skip_coeff;
        let mode_index = MODE_LF_LUT[m.mode as usize] as usize;
        let lvl = lft.lvl[m.segment_id as usize & 3][m.ref_frame as usize][mode_index] as usize;
        if lvl == 0 {
            continue;
        }
        let lfi = lft.lfi(lvl, kf);
        let y_off = new.y_off((mb_col * 16) as isize, (mb_row * 16) as isize);
        let uv_off = new.uv_off((mb_col * 8) as isize, (mb_row * 8) as isize);
        if simple {
            if mb_col > 0 {
                loopfilter::simple_mbv(&mut new.y, y_off, new.y_stride, lfi.mblim);
            }
            if !skip_lf {
                loopfilter::simple_bv(&mut new.y, y_off, new.y_stride, lfi.blim);
            }
            if mb_row > 0 {
                loopfilter::simple_mbh(&mut new.y, y_off, new.y_stride, lfi.mblim);
            }
            if !skip_lf {
                loopfilter::simple_bh(&mut new.y, y_off, new.y_stride, lfi.blim);
            }
        } else {
            if mb_col > 0 {
                loopfilter::filter_mbv(
                    &mut new.y,
                    y_off,
                    new.y_stride,
                    &mut new.u,
                    uv_off,
                    &mut new.v,
                    uv_off,
                    new.uv_stride,
                    &lfi,
                );
            }
            if !skip_lf {
                loopfilter::filter_bv(
                    &mut new.y,
                    y_off,
                    new.y_stride,
                    &mut new.u,
                    uv_off,
                    &mut new.v,
                    uv_off,
                    new.uv_stride,
                    &lfi,
                );
            }
            if mb_row > 0 {
                loopfilter::filter_mbh(
                    &mut new.y,
                    y_off,
                    new.y_stride,
                    &mut new.u,
                    uv_off,
                    &mut new.v,
                    uv_off,
                    new.uv_stride,
                    &lfi,
                );
            }
            if !skip_lf {
                loopfilter::filter_bh(
                    &mut new.y,
                    y_off,
                    new.y_stride,
                    &mut new.u,
                    uv_off,
                    &mut new.v,
                    uv_off,
                    new.uv_stride,
                    &lfi,
                );
            }
        }
    }
}
