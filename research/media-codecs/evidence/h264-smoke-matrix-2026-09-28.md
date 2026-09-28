# H.264 candidate qualification matrix — 2026-09-28

Harness: `research/media-codecs/harness` (`h264-harness`, one child process
per decoder×case — crash isolation by construction). Corpus: `gen_cases.py`
(25 cases, all libx264 through ffmpeg 8.0.1, generated once, shared across
candidates, SHA-256 recorded in `results/cases.json`). Compare: byte-exact
against `ffmpeg -f rawvideo` reference (`compare_all.py`).

Statuses: `exact` byte-identical output · `rejected` clean
FeatureNotSupported/Unsupported error · `mismatch` decoded but bytes differ ·
`crash` panic/abort · `error` other failure · `reinit_ok` handled mid-stream
geometry change (flat rawvideo reference cannot express it — ffmpeg emits
first-SPS geometry throughout, so the reference is inapplicable there).

## Matrix (25 cases)

| case            | rust_h264       | rusty_h264     | hibernia      | oxideav   | wedeo          |
|-----------------|-----------------|----------------|---------------|-----------|----------------|
| baseline        | exact           | exact          | exact         | exact     | exact          |
| main_b          | exact           | exact          | exact         | exact     | exact          |
| high_b          | exact           | exact          | exact         | exact     | exact          |
| high_multi_slice| exact           | exact          | exact         | exact     | exact          |
| cropped         | mismatch 316320 | exact          | exact         | exact     | exact          |
| open_gop        | mismatch 976    | exact          | exact         | exact     | exact          |
| 360p            | mismatch 4950   | exact          | exact         | exact     | exact          |
| mbaff           | mismatch 2633   | rejected       | rejected      | exact     | mismatch 123611|
| high10          | mismatch 2198725| rejected       | **crash**     | exact     | mismatch 2151817|
| cavlc_b         | mismatch 3112   | exact          | exact         | exact     | exact          |
| weightp         | exact           | exact          | exact         | exact     | exact          |
| ref5            | exact           | exact          | exact         | exact     | exact          |
| h422            | mismatch 1470044| rejected       | rejected      | exact     | mismatch 1470346|
| h444            | mismatch 2208963| rejected       | rejected      | exact     | mismatch 2208812|
| nodeblock       | mismatch 4552   | exact          | exact         | exact     | exact          |
| screen360       | exact           | exact          | exact         | exact     | exact          |
| noisy160        | exact           | exact          | exact         | exact     | exact          |
| hq160           | exact           | exact          | exact         | exact     | exact          |
| 720p            | mismatch 439567 | exact          | exact         | exact     | exact          |
| 1080p           | mismatch 10928  | exact          | exact         | exact     | exact          |
| 4k              | mismatch 339    | exact          | exact         | exact     | exact          |
| seek_idr2       | exact           | exact          | exact         | exact     | exact          |
| trunc60         | mismatch 8704   | mismatch 8704  | error         | mismatch 23040 | mismatch 8704 |
| corrupt1        | exact           | exact          | error         | exact     | exact          |
| sps_switch      | reinit_ok       | reinit_ok      | reinit_ok     | reinit_ok | **crash**      |

## Scorecard

| decoder    | exact | reinit_ok | rejected | mismatch | error | crash |
|------------|-------|-----------|----------|----------|-------|-------|
| oxideav    | 23    | 1         | 0        | 1        | 0     | 0     |
| wedeo      | 19    | 0         | 0        | 5        | 0     | 1     |
| rusty_h264 | 17    | 1         | 4        | 1        | 0     | 0     |
| hibernia   | 16    | 1         | 3        | 0        | 2     | 1     |
| rust_h264  | 11    | 1         | 0        | 13       | 0     | 0     |

## Findings per candidate

- **oxideav-h264 0.1.8** (`552f988`): only candidate byte-exact on every
  supported case including MBAFF and High-10 (real `yuv420p10le` output).
  trunc60: emits 25 frames vs ffmpeg's 26 — drops the damaged final AU
  instead of emitting it partially; a concealment-policy difference, not
  corruption. Emits coded-size planes; crop must be applied caller-side via
  `active_sps().frame_cropping` (CropUnit math verified against §7.4.2.1.1).
- **rusty_h264** (`e8de4d2`, safe-core `default-features=false,features=std`):
  exact on every 8-bit 4:2:0 progressive case; clean `Unsupported` on
  interlace, >8-bit, non-4:2:0 — the reference model for honest rejection.
  No silent wrong output observed anywhere.
- **hibernia 0.2.0** (`0687cce`): exact on the same set as rusty_h264;
  clean `FeatureNotSupported` on interlace + non-4:2:0; **panics on High-10**
  (`deblocking.rs:77` index OOB — crash on an in-scope stream). Strictest
  damage handling: returns MisformedData on truncated/corrupted input
  rather than emitting partial frames. API traps found: `decode()` packets
  must carry Annex-B start codes (internal re-split; stripped NAL payloads
  are silently dropped — no error), `PlaneView` windows are coded-size
  (display crop lives in `StreamFormat`), `PlaneView.data` omits the last
  row's stride padding.
- **wedeo-codec-h264** (`9cacd90`): exact on all 8-bit 4:2:0 cases incl.
  perf sizes; **panics on mid-stream resolution change**
  (`decoder.rs:2487` range OOB). MBAFF decodes but is not bit-exact.
  High-10 and 4:2:2/4:4:4 silently emit 8-bit/420-shaped output —
  silent downconversion, not rejection. `send_packet` swallows per-NAL
  errors to `warn!` (decode "errors" surface only as missing frames).
- **rust_h264 0.4.0** (`c9987ca`): exact only on small 4:2:0 progressive
  basics + weightp/ref5/seek/corrupt1. Silent wrong output on: crop,
  open-GOP, CAVLC+B, no-deblock, all sizes ≥360p, MBAFF, High-10
  (silently emits 8-bit), 4:2:2, 4:4:4. No clean-reject path observed —
  unsupported inputs produce wrong pixels silently.

## Timing — median fps, 8 iterations, exact cases only

Same-input decode speed (testsrc2-derived corpus, release opt-level=3,
single process, includes decoder construction; `first_ns` = latency to
first output frame). A fast failing profile is not a speed datum —
missing cells are cases the decoder did not pass.

| case      | rust_h264 | rusty_h264 | hibernia | oxideav | wedeo |
|-----------|-----------|------------|----------|---------|-------|
| baseline  | 6490      | **11582**  | 9856     | 2072    | 3591  |
| high_b    | 5153      | **8702**   | 6421     | 2117    | 3367  |
| 360p      | -         | **1321**   | 871      | 187     | 482   |
| 720p      | -         | **312**    | 199      | 44      | 145   |
| 1080p     | -         | **144**    | 90       | 19      | 68    |
| 4k        | -         | **34**     | 21       | 5       | 17    |
| high10    | -         | -          | crash    | 2041    | -     |
| mbaff     | -         | -          | -        | 1662    | -     |

rusty_h264 safe-core is ~1.5–2.7× the field on shared cases; oxideav is
the slowest (its default config is the stable chunked-array path — the
`nightly` portable-SIMD feature was not enabled). These are smoke-input
numbers, not a selection benchmark.

## `forbid(unsafe_code)` compile check (`cargo rustc -p <crate> -- -F unsafe-code`)

| candidate (config)              | x86_64 | aarch64 | wasm32 |
|---------------------------------|--------|---------|--------|
| rust_h264                       | clean  | FAIL(6 NEON unsafe sites) | clean |
| rusty_h264-decoder safe-core    | clean  | clean   | clean  |
| hibernia                        | FAIL (9 errors: allocator `NonNull`, frame) | — | — |
| oxideav-h264 (default, no nightly) | clean | clean | clean |
| wedeo-codec-h264                | FAIL (38 errors: asm FFI + internals) | — | — |

## Damage-handling semantics (trunc60, corrupt1)

- ffmpeg reference emits 26 frames on trunc60 (last one partial).
  rust_h264/rusty/wedeo emit 26 with ~8.7 KB concealed-pixel diff.
  oxideav emits 25 (drops damaged AU entirely). hibernia rejects.
- corrupt1 (1-byte flip): hibernia rejects with MisformedData; the other
  four emit output byte-identical to ffmpeg's concealment (the flip landed
  in a region the reference conceals identically).

## Repro

```
python3 gen_cases.py                       # corpus + refs + cases.json
cargo build --release --manifest-path harness/Cargo.toml   # all adapters
python3 compare_all.py                     # matrix → results/compare_report.json
python3 compare_all.py --iterations 8      # + timing lines per case
```

All through `~/work/zen/scripts/run-heavy --jobs 4`.
