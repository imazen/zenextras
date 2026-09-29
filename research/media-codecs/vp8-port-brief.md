# VP8 decoder port brief — libvpx → pure Rust

Handoff for a new agent. Scope decided 2026-09-28 after the P3 audit
(`evidence/vp9-scoping-2026-09-28.md`): VP8 *video* decode is the only codec in
the stack with zero usable Rust coverage — zenwebp's `vp8v2` rejects inter
frames (WebP stills only) and rusty_vp9 is VP9-only. Encode is explicitly OUT
of scope for round 1.

## Mission

Build a pure-Rust VP8 **video decoder**, scalar-correctness-first, behind
a push/pull API compatible with `zencodec-media`'s `VideoDecoder` session
contract. Byte-exact YUV420 output vs libvpx is the acceptance bar; ffmpeg's
native `vp8` decoder is the independent oracle for triangulation (the same
role wedeo played for VP9).

**Structure: seed from zenwebp's `vp8v2`, port the inter half from libvpx.**
See "zenwebp reuse" below — the previous assessment underestimated it: vp8v2
already implements ~60–70% of a video decoder (full intra pipeline, loop
filter incl. per-MB-row filtering, token partitions, segmentation, sharpness)
as bit-exact-tested safe no_std Rust. The port from libvpx covers only what
vp8v2 does not have.

## Sources of truth

- **libvpx** https://github.com/webmproject/libvpx — BSD-3-Clause. Pin tag
  v1.16.0 (matches installed libvpx12 runtime). Porting BSD code is
  license-clean; retain copyright headers per file and write a
  `PORTED-FROM.md` provenance record (source URL, tag, commit, file map).
  Do NOT port FFmpeg's native vp8 decoder instead — an FFmpeg derivative is
  LGPL (see wedeo audit) and cannot ship in this stack.
- **zenwebp `decoder/vp8v2/`** — imazen-owned (AGPL/commercial dual-license is
  the owner's; reuse by copying is an owner decision — confirmed 2026-09-28
  that imazen owns all imazen/ projects outright, so relicensing extracted
  modules is available; still record it in the provenance map).
- **RFC 6386** is the normative spec; use it to sanity-check both sources,
  not as the port source.

## What to port from libvpx (inter half only — intra is seeded from vp8v2)

| libvpx path | role |
|---|---|
| `vp8/decoder/decodeframe.c` (inter portions) | non-keyframe frame tag/header, refresh flags, quant/LF deltas |
| `vp8/decoder/detokenize.c` (MV section) + `vp8/common/mv.h`, `findnearmv` | MV trees, clamping, near/nearest/zero/new, splitmv sub-block modes |
| `vp8/common/reconinter.c` + `vpx_dsp/vpx_convolve*` | 1/4-pel 6-tap + bilinear subpixel MC, edge extension |
| `vp8/common/alloccommon.c` (ref-frame mgmt) | last/golden/altref buffers, refresh semantics |
| `vp8/common/entropy.c` (prob-update paths) | entropy persistence / `refresh_entropy` handling |
| `vpx_mem`, `vpx_ports` | endian loads/alignment → Rust equivalents |

Already covered by the vp8v2 seed (do NOT re-port): bool decoder, token/
coefficient decode, partitions, segmentation, intra prediction, loop filter.

Skip: `vp8/encoder/`, `vp9/**`, `vpx_scale/`, all SIMD (x86/arm — scalar C
reference only), frame-parallel threading, postproc, `error_concealment.c`
(mark deferred, not dropped — corrupt-input concealment behavior differs from
libvpx without it; see rusty_vp9's concealment-prefix precedent for how to
document the divergence).

## Bitstream features that MUST be covered

- keyframe + inter frames; golden/altref refresh flags; invisible
  `show_frame=0` frames (altref updates) — packets ≠ displayed pictures
- token partitions (multiple bool streams per frame)
- segmentation: 4 segments, abs/delta feature data, per-segment quantizer and
  loop-filter strength, `update_mb_segmentation_map`
- loop filter: normal + simple (`version` field selects), sharpness level,
  mode/ref LF delta adjustments (`mb_lf_adjustments`)
- keyframe-carried resolution changes mid-stream (reinit path)
- all intra modes; MV trees with clamping; 1/4-pel interpolation
- output is always 8-bit 4:2:0 — no HBD, no alpha (WebP alpha plane lives in
  the container, not the VP8 bitstream; out of scope)

## API shape

Mirror rusty_vp9's proven shape so the session adapter is mechanical:

```rust
dec.push(&frame_payload);          // one VP8 frame per push (WebM = raw payload)
while let Some(f) = dec.next_frame() { .. }   // shown frames only
dec.flush();                       // drain
```

zencodec-media side: `Codec::Vp8` already exists in `track.rs` ("vp8" WebM id)
and `VideoDecoder` is a push-packet/pull-frame trait in `session.rs`. IVF works
for vectors; WebM demux already parses. Each `MediaPacket` is one raw VP8 frame
(no container-level per-packet header).

## Verification (reuse the existing harness)

- Corpus generator: clone `gen_cases.py` → `gen_vp8.py`, `ffmpeg -c:v libvpx`
  encodes (testsrc2, counter, noise; CRF/VBR; a resolution-change case; an
  invisible-frame/altref case via libvpx options; corrupted-packet case).
- Reference decoders — TWO available locally: ffmpeg native `vp8` and ffmpeg's
  `libvpx` decoder wrapper. Require byte-exact vs libvpx; native-vp8 diffs
  triangulate ambiguity (libvpx is authoritative).
- Conformance spot-checks: libvpx test-data VP8 vectors
  (`vp80-00-comprehensive`, `vp80-05-sharpness`, etc.) if fetchable.
- Evidence files: `evidence/vp8-matrix-<date>.json` + `.md` findings doc —
  same columns as `vp9-matrix`: exact/mismatch/error per cell + timing.
- `forbid(unsafe_code)` audit on x86_64/aarch64/wasm32; zero-dep target like
  rusty_vp9 (num-traits-level deps acceptable, avoid image/C crates).

## Placement: zenextras, not candidates/ or a new repo

`~/work/zenmedia-resume-20260928/candidates/` is **unversioned scratch** (its
members are standalone clones/tarballs, no enclosing repo) — a port there is
not tracked. `imazen/zenvpx` does not exist (404 confirmed in the P3 audit);
creating it now would publish an empty scaffold publicly and needs org repo
rights. Correct home: **`zenextras/research/media-codecs/zenvp8/`** — a
workspace-member crate sibling to `transcode/` on
`research/media-codec-qualification` (versioned, pushed per phase). Promote to
a standalone `imazen/zenvpx` repo *after* qualification, matching how other
candidates were evaluated before earning a real crate.

## zenwebp reuse: copy the intra core, port the inter half

Owner confirmed imazen owns zenwebp outright — relicensing extracted modules
is available by owner decision, so license is **not** the blocker the earlier
audit implied. The real question was engineering, and a fresh read shows vp8v2
is far more complete than "stills keyframes":

**vp8v2 already has** (`zenwebp/src/decoder/vp8v2/`, 6.8k lines, alloc-only
no_std, `archmage` SIMD dispatch with scalar/SSE/NEON/wasm128 paths):

- bool decoder + token/coefficient decode (`coefficients.rs`, `context.rs`,
  `tables.rs` — spec tables are spec constants regardless of source)
- **token partitions** — `header.rs` parses `num_partitions` (1/2/4/8) and
  `init_partitions` wires multiple bool streams
- **segmentation** — `segments_enabled` + `read_segment_updates`
- **sharpness + loop filter** — `sharpness_level` parsed; `pipeline.rs` has
  `filter_mb_row` with scalar + SSE2/NEON/wasm dispatch
- intra recon (`predict_fused.rs` + `common/prediction.rs`)

**What vp8v2 lacks — the actual port surface** (from libvpx `vp8/`):

- inter frame header path (frame tag differs: no dims/sync on non-keyframes;
  `header.rs:38` currently rejects them)
- MB inter types: INTER + SPLITMV with sub-block MVs (new/nearest/near/zero
  MV trees, `sign_bias_golden`/`sign_bias_alternate`, MV clamping) —
  `decodeframe.c`/`detokenize.c` MV section + `mv.h`/`findnearmv`
- subpixel MC: 1/4-pel 6-tap + bilinear convolve (`reconinter.c` +
  `vpx_dsp/vpx_convolve*`) with edge extension
- ref-frame state: last/golden/altref buffers + per-frame refresh flags,
  entropy-prob persistence across frames (`refresh_entropy`), `show_frame=0`
  invisible frames
- streaming reshape: vp8v2 is one-shot stills API; the video decoder needs
  push-packet/pull-frame + per-stream persistent state

**Not reusable**: `yuv_exact.rs`/`dither` (fused RGB output for stills — a
video decoder emits YUV planes), `alloc_util`/`api` coupling (light, re-shim).

Keep a dual provenance map: files `vp8v2-`-seeded vs `libvpx`-ported. Both are
license-compatible destinations (owner-controlled + BSD); clarity is for
review, not law.

## Rules of the house

- Worktree discipline: `.workongoing` marker before touching a repo,
  `~/work/scripts/`-style `run-heavy` for builds (zencodec workspace is huge —
  always scope cargo commands to the member crate).
- Commits land on `research/media-codec-qualification` (zenextras, jj
  colocated — `jj git push --bookmark`); the new crate lives at
  `research/media-codecs/zenvp8/` (versioned, not the unversioned
  `candidates/` scratch). Per-commit provenance notes.
- Do NOT reproduce FFmpeg code — only libvpx (BSD). Keep a file-level
  provenance map so review can diff C↔Rust.
- Deviations from libvpx behavior (e.g. concealment) get documented in the
  findings doc with evidence, not silently.

## Known precedents in this workspace

- `rusty_vp9` (candidates/rusty_vp9): the structural model — FFmpeg-style
  push/pull decoder, zero deps. Read its superframe/reference handling for
  how it papers over one-packet-≠-one-picture.
- `oxideav-h264` (candidates/oxideav-h264): the scale model — clean-room
  decoder + 25-case matrix methodology (`evidence/h264-smoke-matrix-*.md`).
- Session/pump contract: `zencodec/media/src/session.rs`, container demux:
  `mp4.rs`, `webm.rs` (VP8 path untested end-to-end but plumbed).
