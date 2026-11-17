//! Streaming windowed-sinc resampler with exact rational sample accounting.
//!
//! Ratios like 44100→48000 keep `n * in_rate / out_rate` as an integer
//! position plus a remainder — no accumulated f64 phase error. The filter is a
//! Blackman-windowed sinc, 2·`R` taps, cutoff `min(1, out/in)` in input-time
//! units, normalized per output sample so DC gain is exactly 1.
//!
//! Bounded: `push` keeps only the filter tail (2R input frames/channel) plus
//! what has been pushed since the last `drain`; `finish` emits the exact
//! `round(total_in * out_rate / in_rate)` count with zero padding at both ends.

/// Half the FIR span, in input frames.
const R: usize = 32;

pub struct StreamingResampler {
    in_rate: u64,
    out_rate: u64,
    channels: usize,
    /// Interleaved input still reachable by the filter window.
    buf: Vec<f32>,
    /// Absolute input frame index of `buf[0]`'s frame.
    buf_base: u64,
    /// Absolute count of input frames pushed (per channel).
    in_total: u64,
    /// Absolute index of the next output frame.
    out_next: u64,
    /// Set by `finish` — the exact number of outputs this stream produces.
    out_total: Option<u64>,
    /// Normalized cutoff `min(1, out/in)`.
    cutoff: f64,
}

impl StreamingResampler {
    pub fn new(in_rate: u32, out_rate: u32, channels: usize) -> Self {
        assert!(in_rate > 0 && out_rate > 0 && channels > 0);
        Self {
            in_rate: in_rate as u64,
            out_rate: out_rate as u64,
            channels,
            buf: Vec::new(),
            buf_base: 0,
            in_total: 0,
            out_next: 0,
            out_total: None,
            cutoff: (out_rate.min(in_rate) as f64) / in_rate as f64,
        }
    }

    /// True when a resampler is actually needed.
    pub fn needed(in_rate: u32, out_rate: u32) -> bool {
        in_rate != out_rate
    }

    /// Push one interleaved input block; `out` receives every output frame the
    /// window can now reach (right edge must stay inside pushed input).
    pub fn push(&mut self, interleaved: &[f32], out: &mut Vec<f32>) {
        debug_assert!(self.out_total.is_none(), "push after finish");
        debug_assert_eq!(interleaved.len() % self.channels, 0);
        self.in_total += (interleaved.len() / self.channels) as u64;
        self.buf.extend_from_slice(interleaved);
        self.produce(out, false);
    }

    /// Flush: emit the exact output count, zero-padding the tail edge.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        if self.out_total.is_none() {
            self.out_total =
                Some((self.in_total * self.out_rate + self.in_rate / 2) / self.in_rate);
        }
        self.produce(out, true);
    }

    /// Absolute input frame index `p`, channel `c` → sample (0 outside input).
    fn sample(&self, p: i64, c: usize) -> f64 {
        if p < 0 || p as u64 >= self.in_total {
            return 0.0;
        }
        let rel = p as u64 - self.buf_base;
        self.buf[rel as usize * self.channels + c] as f64
    }

    fn produce(&mut self, out: &mut Vec<f32>, finished: bool) {
        loop {
            let cap = match self.out_total {
                Some(t) => t,
                // Streaming: emit only outputs whose right window edge is inside
                // pushed input: in_idx + R < in_total.
                None => {
                    // n's center: in_idx = n*in/out. Need in_idx + R < in_total.
                    let n = self.out_next;
                    let in_idx = n * self.in_rate / self.out_rate;
                    if in_idx + R as u64 >= self.in_total {
                        break;
                    }
                    n + 1
                }
            };
            if self.out_next >= cap {
                break;
            }

            let num = self.out_next * self.in_rate;
            let in_idx = (num / self.out_rate) as i64;
            let frac = (num % self.out_rate) as f64 / self.out_rate as f64;

            // Sum taps k = -(R-1)..=R ; window centered on `frac`.
            for c in 0..self.channels {
                let mut acc = 0.0;
                let mut norm = 0.0;
                for k in (-(R as i64) + 1)..=R as i64 {
                    let x = k as f64 - frac;
                    let w = blackman((x + R as f64) / (2.0 * R as f64));
                    if w == 0.0 {
                        continue;
                    }
                    let h = self.cutoff * sinc(self.cutoff * x) * w;
                    norm += h;
                    acc += self.sample(in_idx + k, c) * h;
                }
                out.push(if norm != 0.0 {
                    (acc / norm) as f32
                } else {
                    0.0
                });
            }
            self.out_next += 1;
        }
        if finished {
            return;
        }

        // Drop input the window can no longer reach: needed min absolute index
        // is in_idx(next) - R + 1.
        let next_in = (self.out_next * self.in_rate / self.out_rate) as i64;
        let min_keep = next_in - R as i64 + 1;
        if min_keep > self.buf_base as i64 {
            let drop = (min_keep - self.buf_base as i64) as usize;
            self.buf.drain(..drop * self.channels);
            self.buf_base = min_keep as u64;
        }
    }
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.0
    } else {
        (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
    }
}

fn blackman(t: f64) -> f64 {
    if !(0.0..=1.0).contains(&t) {
        return 0.0;
    }
    0.42 - 0.5 * (2.0 * std::f64::consts::PI * t).cos()
        + 0.08 * (4.0 * std::f64::consts::PI * t).cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_count_44100_to_48000() {
        let mut r = StreamingResampler::new(44100, 48000, 1);
        let mut out = Vec::new();
        // 45 blocks of 1024 samples, like the MP4 fixture.
        for b in 0..45u64 {
            let pcm: Vec<f32> = (0..1024)
                .map(|i| ((b * 1024 + i) as f32 * 0.01).sin())
                .collect();
            r.push(&pcm, &mut out);
        }
        r.finish(&mut out);
        assert_eq!(out.len(), (46080 * 48000 + 22050) / 44100); // 50150
    }

    #[test]
    fn passthrough_when_equal_rates() {
        // Identity still runs the window; check count and near-identity.
        let mut r = StreamingResampler::new(48000, 48000, 1);
        let mut out = Vec::new();
        let pcm: Vec<f32> = (0..4096).map(|i| (i as f32 * 0.05).sin()).collect();
        r.push(&pcm, &mut out);
        r.finish(&mut out);
        assert_eq!(out.len(), 4096);
        // Interior samples within tolerance of the source (edges differ).
        for i in 64..4032 {
            assert!((out[i] - pcm[i]).abs() < 0.01, "mismatch at {i}");
        }
    }

    #[test]
    fn streaming_push_boundaries() {
        let mut r = StreamingResampler::new(44100, 48000, 2);
        let mut out = Vec::new();
        // Tiny pushes still produce output eventually.
        for _ in 0..400 {
            r.push(&[0.1f32; 256 * 2], &mut out);
        }
        r.finish(&mut out);
        assert_eq!(
            out.len(),
            ((102400u64 * 48000 + 22050) / 44100) as usize * 2
        );
    }
}
