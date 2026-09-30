# zenvp8 encoder evidence — 2026-09-29

Host: lilith (WSL). Encoder is `--features encoder`, release build.
Oracle: libvpx @ deaac254 (realtime, VPX_Q, min==max, kf disabled →
forced-interval KFs only, lag=0), ffmpeg 7.x (`vp8` native + `libvpx`).

## Conformance matrix (`check_enc.sh`)

Every case: encode with `examples/enc_raw`, decode the IVF with (a) zenvp8
(`dump`), (b) raw libvpx (`tools/vp8-dec-raw`), (c) ffmpeg native vp8 —
all three outputs byte-identical, plus ffmpeg `-c:v libvpx` identical on
the QCIF probe (4-way). 17/17 cases pass.

| case | dims | q | frames | kf | stream bytes |
|---|---|---|---|---|---|
| g176_q30 | 176x144 | 30 | 24 | — | 15506 |
| m176_q30 | 176x144 | 30 | 24 | — | 4231 |
| n176_q30 | 176x144 | 30 | 12 | — | 241543 |
| s176_q30 | 176x144 | 30 | 12 | — | 424 |
| g176_q08 | 176x144 | 8 | 24 | — | 29988 |
| g176_q90 | 176x144 | 90 | 24 | — | 8338 |
| m176_q100 | 176x144 | 100 | 24 | — | 2100 |
| g176_kf1 | 176x144 | 30 | 24 | 1 | 48078 |
| g176_kf5 | 176x144 | 30 | 24 | 5 | 21640 |
| g176_kf12 | 176x144 | 30 | 24 | 12 | 16393 |
| qcif_kf7 | 176x144 | 30 | 45 | 7 | 74221 |
| g48_q30 | 48x48 | 30 | 16 | — | 1358 |
| m50x30_q40 | 50x30 | 40 | 16 | — | 822 |
| g36x28_q60 | 36x28 | 60 | 12 | — | 582 |
| m33x17_kf4 | 33x17 (odd) | 30 | 12 | 4 | 725 |
| s16_q30 | 16x16 | 30 | 8 | — | 169 |
| g640_q30 | 640x360 | 30 | 10 | — | 51174 |

## Rate/quality vs libvpx (`rate_quality_enc.py`)

libvpx's API caps `rc_min/max_quantizer` at 63 — q>63 cases are
conformance-only. All-plane PSNR vs source; sizes are coded bytes.

| case | zenvp8 | libvpx | size ratio | dPSNR |
|---|---|---|---|---|
| g176_q30 | 15314B 42.95dB | 16050B 40.07dB | 0.95x | +2.88 |
| g176_q08 | 29796B 50.70dB | 25229B 48.35dB | 1.18x | +2.35 |
| m176_q30 | 4039B 55.17dB | 3667B 54.54dB | 1.10x | +0.64 |
| n176_q30 | 241447B 33.61dB | 221665B 31.51dB | 1.09x | +2.10 |
| s176_q30 | 328B 48.92dB | 656B 48.92dB | 0.50x | +0.00 |
| g176_kf5 | 21448B 42.95dB | 22664B 40.07dB | 0.95x | +2.88 |
| g176_kf1 | 47886B 42.95dB | 36421B 40.07dB | 1.31x | +2.88 |
| g48_q30 | 1230B 41.28dB | 1168B 42.44dB | 1.05x | -1.17 |
| m50x30_q40 | 694B 46.99dB | 705B 42.54dB | 0.98x | +4.45 |
| g36x28_q60 | 486B 38.57dB | 344B 34.70dB | 1.41x | +3.87 |
| m33x17_kf4 | 629B 54.54dB | 3790B 21.37dB | 0.17x | +33.18 |
| s16_q30 | 169B 48.92dB | 237B 48.92dB | 0.71x | +0.00 |
| g640_q30 | 51094B 42.64dB | 58391B 40.30dB | 0.88x | +2.33 |
| qcif_kf7 | 73861B 41.19dB | 62267B 39.52dB | 1.19x | +1.67 |

Read: zenvp8 spends more bits per MB (regular_quantize + no RD mode
decisions) but wins on several cases anyway because libvpx realtime at
fixed-Q under-spends on I16-only-friendly content. `m33x17_kf4` is a
libvpx realtime collapse on a tiny odd-size motion clip, not a zenvp8
strength claim. `g176_kf1` (all-keyframe) and `g36x28_q60` show the real
cost of missing B_PRED/SPLITMV decisions.

## Performance

176x144, 45 frames: zenvp8 ~149ms (~300 fps) vs libvpx ~27ms
(~1670 fps) — ~5.5x slower, scalar-only as documented.

## Regression gates (this session)

- `cargo test` (default): 11+7 pass, decoder unchanged.
- `cargo test --features encoder`: 12 encoder tests incl. new
  `too_large_rejected`/`short_stride_rejected` error paths.
- `cargo clippy --all-targets` ± `--features encoder`: clean.
- `cargo fmt --check`: clean.
- Default build exposes no encoder API (`#[cfg(feature = "encoder")]`).

## Known limits (unchanged from v1 scope)

- I16 intra only (no B_PRED), ZEROMV/NEAREST/NEAR/NEWMV vs LAST only
  (no SPLITMV, GOLDEN, ALTREF), one token partition, no segmentation.
- `mb_no_coeff_skip` always emitted 1.
- Mode pick is SAD+penalty, not libvpx RD; `costs.rs`/`metrics.rs` hold
  the staged Speed-5 machinery for that port.
- `qindex` clamps to 0..=127; libvpx's API caps user-visible Q at 63
  (zenvp8 emits conformant streams at 64..=127 regardless).
