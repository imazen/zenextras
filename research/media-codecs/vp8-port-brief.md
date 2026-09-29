# VP8 decoder port brief — libvpx → pure Rust

Handoff for a new agent. Scope decided 2026-09-28 after the P3 audit
(`evidence/vp9-scoping-2026-09-28.md`): VP8 *video* decode is the only codec in
the stack with zero usable Rust coverage — zenwebp's `vp8v2` rejects inter
frames (WebP stills only) and rusty_vp9 is VP9-only. Encode is explicitly OUT
of scope for round 1.

## Mission

Port libvpx's VP8 **decoder** to pure Rust, scalar-correctness-first, behind
a push/pull API compatible with `zencodec-media`'s `VideoDecoder` session
contract. Byte-exact YUV420 output vs libvpx is the acceptance bar; ffmpeg's
native `vp8` decoder is the independent oracle for triangulation (the same
role wedeo played for VP9).

## Source of truth

- **libvpx** https://github.com/webmproject/libvpx — BSD-3-Clause. Pin tag
  v1.16.0 (matches installed libvpx12 runtime). Porting BSD code is
  license-clean; retain copyright headers per file and write a
  `PORTED-FROM.md` provenance record (source URL, tag, commit, file map).
  Do NOT port FFmpeg's native vp8 decoder instead — an FFmpeg derivative is
  LGPL (see wedeo audit) and cannot ship in this stack.
- **RFC 6386** is the normative spec; use it to sanity-check libvpx behavior,
  not as the port source.

## What to port (libvpx file map, decoder only)

| libvpx path | role |
|---|---|
| `vp8/common/treecoder.*`, `dboolhuff.*` | boolean arithmetic decoder |
| `vp8/decoder/decodeframe.c` | frame header, mb feature data, token partitions |
| `vp8/decoder/detokenize.c` + `vp8/common/entropy.c` | coef/token tree decode + prob tables |
| `vp8/common/reconintra.c` | B_PRED (10× 4x4), 16x16 DC/V/H/TM, UV modes |
| `vp8/common/reconinter.c`, `vpx_dsp/vpx_convolve*` | inter pred, 1/4-pel 6-tap + bilinear |
| `vp8/common/idct_blk.c`, `dequantize.c` | IDCT + WHT (I4x4 second-order DC) + dequant |
| `vp8/common/loopfilter*.c` | normal filter (edge/sharpness variants) + simple filter |
| `vp8/common/alloccommon.c`, `modecont.c`, `findnearmv` paths | ref frames (last/golden/altref), mode contexts, MV trees |
| `vpx_mem`, `vpx_ports` | endian loads/alignment → Rust equivalents |

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

## zenwebp reuse: read it, don't link it

- **License**: zenwebp is AGPL-3.0/commercial. Copying `vp8v2`/`encoder/vp8`
  code into the port makes the port AGPL-derived unless the owner relicenses
  the extracted parts — that is the owner's call, not the agent's. A libvpx
  port wants one upstream provenance anyway; do not mix streams.
- **Surface**: extraction is not cheap — `vp8v2` drags in
  bit_reader/loop_filter/dither/yuv/`#[arcane]`/zensim machinery (per the
  scoping audit).
- **Coverage**: the reusable overlap is only the intra third — bool decoder,
  token decode, intra recon, loop filter. The inter half that makes this a
  video decoder (MV trees, ref management, subpixel MC, partitions, entropy
  refresh) doesn't exist in zenwebp at all, so reuse doesn't skip the hard
  part.
- **Right use**: keep `vp8v2` open as a *Rust-idiom reference* — how to shape
  a bool decoder / IDCT / loop filter in safe Rust. Zero cost, zero license
  exposure, same owner's code.

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
