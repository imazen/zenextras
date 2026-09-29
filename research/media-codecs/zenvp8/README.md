# zenvp8

Pure-Rust VP8 **video** decoder. `#![forbid(unsafe_code)]`, zero external
dependencies, `no_std`-shaped (alloc only, not yet marked `no_std`).

Ported from **libvpx** `deaac25491db2edc430c2a71031109b65c23d1f1`
(BSD-3-Clause) with the intra core seeded from imazen `zenwebp`'s
`decoder/vp8v2`. See `PORTED-FROM.md` for the per-file provenance map.
Encoding is out of scope by design decision (`vp8-port-brief.md`).

## Status (2026-09-29)

**Byte-exact against libvpx on every clean stream in the corpus** —
12/12 cases, including a two-pass encode with real invisible
alternate-reference packets (`v0_arf2p`: 68 packets, 4 suppressed,
64 shown, byte-identical vs raw libvpx decode).

Scalar, correctness-first performance (single thread, x86_64):
~8.5k fps on 128×96, ~1.1–5.4k fps on 176×144 / 640×360 (see
`results-vp8/compare-vp8.json`). Not tuned; no SIMD.

## API

```rust
use zenvp8::{Vp8Decoder, DecodedFrame};

let mut dec = Vp8Decoder::new();
dec.decode(&pkt)?;                    // one raw VP8 frame payload per call
while let Some(f) = dec.next_frame() { /* DecodedFrame {y,u,v,...} */ }
```

- `decode(&[u8])` takes **one raw VP8 frame payload** (IVF/WebM packet —
  no container headers). One call ≦ one `DecodedFrame`; invisible
  (`show_frame=0`) packets update reference state and emit nothing.
- `next_frame()` drains the output queue (at most one pending frame;
  `decode` also returns it eagerly).
- `dimensions()` reports current frame size; mid-stream keyframe-carried
  resolution changes reinit transparently and surface via `DecodedFrame`.
- `reset()` drops all decoder state (references, entropy context, dims).
- Output is tightly-packed planar I420 (`y`,`u`,`v` `Vec<u8>`), 8-bit —
  the only format VP8 produces.

`DecodedFrame.corrupted` mirrors libvpx's per-frame corruption flag (bool
decoder overran its partition); decoding continues best-effort exactly as
libvpx does.

## Covered bitstream features

- Key + inter frames; golden/altref references; all refresh/copy flag
  combinations (`refresh_golden/altref/last`, `copy_buffer_to_gf/arf`,
  sign bias) in libvpx's exact ordering
- **Invisible altref packets** — reference state updates, output suppressed
- Multiple coefficient token partitions (1/2/4/8 bool streams,
  row-strided assignment)
- Segmentation: 4 segments, abs/delta feature data, per-segment quantizer
  and loop-filter strength, `update_mb_segmentation_map`
- Loop filter: normal + simple (version-selected), sharpness level,
  mode/ref LF deltas — deferred per-row application like libvpx
- All intra modes (incl. B_PRED context + `intra_prediction_down_copy`),
  full inter mode/MV tree (NEARESTMV/NEARMV/ZEROMV/NEWMV/SPLITMV),
  MV clamping + sign bias, 1/8-pel-coded vectors
- Subpixel MC: 6-tap and bilinear (version 1 / 3 encoder paths incl.
  full-pixel MVs)
- Mid-stream resolution changes at keyframe boundaries

## Known limitations

- No error concealment (`error_concealment.c` not ported): corrupt input
  yields `Err`/marked-corrupt frames at the same point libvpx flags it,
  not concealment-filled output. Documented in `PORTED-FROM.md`.
- Soft size cap (`DecodeError::TooLarge`) where libvpx allocates freely.
- Scalar only — no SIMD dispatch yet.

## Testing

- `cargo test` — unit tests (bool reader, idct, predictors, tables) +
  integration robustness suite (`tests/robustness.rs`: tag parse, dim cap,
  garbage/truncation fuzz, `MissingKeyframe`).
- Differential harness: `../harness` (`zenvp8` adapter) +
  `../gen_vp8_cases.py` / `../compare_vp8.py` — 16-case corpus vs libvpx
  (primary) and ffmpeg-native-vp8 (triangulation). For streams with
  `show_frame=0` packets the primary oracle is `../tools/vp8-dec-raw`
  (raw `vpx_codec_decode` loop) — **ffmpeg's libvpx pipe inserts
  duplicate frames for suppressed packets and is not a valid reference
  there** (see `PORTED-FROM.md`).
