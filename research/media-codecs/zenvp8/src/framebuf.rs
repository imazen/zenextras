//! Frame buffers — the safe-Rust equivalent of libvpx `YV12_BUFFER_CONFIG`
//! plus the `yv12_fb[]` reference-role machinery.
//!
//! Layout per plane: `BORDER` luma pixels of padding on every side
//! (`BORDER/2` for chroma); index of pixel (0,0) is `origin` into the Vec.
//!
//! Reference roles are buffer indices; `copy_buffer_to_*` copies pixels
//! between buffers (same observable result as libvpx's refcounted swap).

/// `VP8BORDERINPIXELS`.
pub(crate) const BORDER: usize = 32;

/// Logical reference role (index into the buffer array).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RefRole {
    /// The frame being decoded (`frame_to_show` candidate).
    New = 0,
    /// `LAST_FRAME`.
    Last = 1,
    /// `GOLDEN_FRAME`.
    Golden = 2,
    /// `ALTREF_FRAME`.
    AltRef = 3,
}

impl RefRole {
    #[allow(dead_code)]
    pub(crate) fn from_idx(i: usize) -> Self {
        [Self::New, Self::Last, Self::Golden, Self::AltRef][i & 3]
    }
}

/// One padded YUV420 frame.
#[derive(Clone)]
pub(crate) struct FrameBuf {
    /// Padded luma width (`mb_cols * 16`).
    pub y_w: usize,
    /// Padded luma height (`mb_rows * 16`).
    pub y_h: usize,
    /// Padded chroma width (`mb_cols * 8`).
    pub uv_w: usize,
    /// Padded chroma height (`mb_rows * 8`).
    pub uv_h: usize,
    pub y_stride: usize,
    pub uv_stride: usize,
    /// Plane storage including borders.
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    /// Index of pixel (0,0) in `y` / in `u`,`v`.
    pub y_origin: usize,
    pub uv_origin: usize,
    /// `yv12_fb->corrupted` latch.
    pub corrupted: bool,
}

impl FrameBuf {
    /// `vp8_yv12_alloc_frame_buffer(w, h, VP8BORDERINPIXELS)` with `w`,`h`
    /// already MB-padded.
    pub(crate) fn new(y_w: usize, y_h: usize) -> Self {
        let uv_w = y_w / 2;
        let uv_h = y_h / 2;
        let y_stride = y_w + 2 * BORDER;
        let uv_stride = uv_w + BORDER; // BORDER/2 each side
        let y_origin = BORDER * y_stride + BORDER;
        let uv_origin = (BORDER / 2) * uv_stride + BORDER / 2;
        FrameBuf {
            y_w,
            y_h,
            uv_w,
            uv_h,
            y_stride,
            uv_stride,
            y: vec![0; y_stride * (y_h + 2 * BORDER)],
            u: vec![0; uv_stride * (uv_h + BORDER)],
            v: vec![0; uv_stride * (uv_h + BORDER)],
            y_origin,
            uv_origin,
            corrupted: false,
        }
    }

    /// Flat index of luma pixel (x,y) — x,y may step into the border.
    #[inline(always)]
    pub(crate) fn y_off(&self, x: isize, y: isize) -> usize {
        (self.y_origin as isize + y * self.y_stride as isize + x) as usize
    }
    /// Flat index of chroma pixel (x,y).
    #[inline(always)]
    pub(crate) fn uv_off(&self, x: isize, y: isize) -> usize {
        (self.uv_origin as isize + y * self.uv_stride as isize + x) as usize
    }

    /// `vp8_setup_intra_recon_top_line` — row -1 = 127 for `width + 5`
    /// columns starting at col -1, on all three planes.
    pub(crate) fn setup_intra_recon_top_line(&mut self) {
        let base = self.y_off(-1, -1);
        self.y[base..base + self.y_w + 5].fill(127);
        let base_u = self.uv_off(-1, -1);
        self.u[base_u..base_u + self.uv_w + 5].fill(127);
        self.v[base_u..base_u + self.uv_w + 5].fill(127);
    }

    /// `setup_intra_recon_left` — col -1 of the current MB row's 16 luma
    /// rows (and 8 chroma rows) = 129.
    pub(crate) fn setup_intra_recon_left(&mut self, mb_row: usize) {
        let y0 = self.y_off(-1, (mb_row * 16) as isize);
        for i in 0..16 {
            self.y[y0 + i * self.y_stride] = 129;
        }
        let u0 = self.uv_off(-1, (mb_row * 8) as isize);
        for i in 0..8 {
            self.u[u0 + i * self.uv_stride] = 129;
            self.v[u0 + i * self.uv_stride] = 129;
        }
    }

    /// `vp8_extend_mb_row` — copy the last luma col of rows 14,15 of MB row
    /// `mb_row` into cols y_w..y_w+4 (B_PRED top-right context); same for
    /// chroma rows 6,7.
    pub(crate) fn extend_mb_row(&mut self, mb_row: usize) {
        for dr in [14usize, 15] {
            let off = self.y_off(self.y_w as isize, (mb_row * 16 + dr) as isize);
            let v = self.y[off - 1];
            self.y[off..off + 4].fill(v);
        }
        for pl in [&mut self.u, &mut self.v] {
            for dr in [6usize, 7] {
                let off = self.uv_origin + (mb_row * 8 + dr) * self.uv_stride + self.uv_w;
                let v = pl[off - 1];
                pl[off..off + 4].fill(v);
            }
        }
    }

    /// `yv12_extend_frame_left_right_c` for one band of `rows` luma rows
    /// starting at `row` (data rows only).
    pub(crate) fn extend_lr_rows(&mut self, row: usize, rows: usize) {
        for r in row..row + rows {
            let rs = self.y_off(0, r as isize);
            let lv = self.y[rs];
            let rv = self.y[rs + self.y_w - 1];
            self.y[rs - BORDER..rs].fill(lv);
            self.y[rs + self.y_w..rs + self.y_w + BORDER].fill(rv);
        }
        for r in row / 2..row / 2 + rows / 2 {
            for pl in [&mut self.u, &mut self.v] {
                let rs = self.uv_origin + r * self.uv_stride;
                let lv = pl[rs];
                let rv = pl[rs + self.uv_w - 1];
                pl[rs - BORDER / 2..rs].fill(lv);
                pl[rs + self.uv_w..rs + self.uv_w + BORDER / 2].fill(rv);
            }
        }
    }

    /// `yv12_extend_frame_top_c` + `bottom_c` — replicate the outermost
    /// border-inclusive data rows into the border rows.
    pub(crate) fn extend_tb(&mut self) {
        let yline = self.y_w + 2 * BORDER;
        let top_src = self.y_off(-(BORDER as isize), 0);
        for i in 1..=BORDER {
            let (lo, hi) = (top_src - i * self.y_stride, top_src);
            self.y.copy_within(hi..hi + yline, lo);
        }
        let bot_src = self.y_off(-(BORDER as isize), self.y_h as isize - 1);
        for i in 1..=BORDER {
            let (src, dst) = (bot_src, bot_src + i * self.y_stride);
            self.y.copy_within(src..src + yline, dst);
        }
        let cline = self.uv_w + BORDER;
        for pl in [&mut self.u, &mut self.v] {
            let ctop = self.uv_origin - BORDER / 2;
            for i in 1..=BORDER / 2 {
                let dst = ctop - i * self.uv_stride;
                pl.copy_within(ctop..ctop + cline, dst);
            }
            let cbot = self.uv_origin - BORDER / 2 + (self.uv_h - 1) * self.uv_stride;
            for i in 1..=BORDER / 2 {
                let dst = cbot + i * self.uv_stride;
                pl.copy_within(cbot..cbot + cline, dst);
            }
        }
    }
}
