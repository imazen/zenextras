# zenvp8 provenance map

VP8 video decoder in safe Rust (`#![forbid(unsafe_code)]`).

Two source codebases, both license-compatible destinations:

- **libvpx** — https://github.com/webmproject/libvpx, BSD-3-Clause.
  Pinned to commit `deaac25491db2edc430c2a71031109b65c23d1f1`
  (`deaac2549`, "vp8: Validate ss_number_layers and factor den in
  validate_config"), checked out at `candidates/libvpx/` in the zenextras
  research tree. The C tree is read-only reference; any temporary
  instrumentation added for debugging is stashed, not committed.
- **zenwebp `src/decoder/vp8v2/`** — imazen-owned (AGPL/commercial
  dual-license; relicensing of extracted modules is an owner decision,
  confirmed 2026-09-28). The intra/still-image decode core seeded this
  crate; the video-decoder port then cross-checked every kernel against
  libvpx and diverges where libvpx semantics differ.

RFC 6386 is the normative spec; used to sanity-check both sources, not as
a port source. FFmpeg code was deliberately NOT used (native vp8 decoder
is LGPL) — ffmpeg appears only as a test oracle.

## File-level map

| zenvp8 file | origin | primary C reference (verification target) |
|---|---|---|
| `src/boold.rs` | libvpx port | `vp8/decoder/dboolhuff.{c,h}` — 64-bit `VP8_BD_VALUE`/`size_t` window, `vp8dx_bool_error` semantics. (vp8v2 has an equivalent reader; this file is a faithful dboolhuff port because the 32-bit variant is not bit-identical.) |
| `src/tables.rs` | vp8v2 seed + libvpx | spec constants (zigzag, bands, cat probs, kf mode probs, trees) from `zenwebp/vp8v2/tables.rs`; inter tables (ymode/uv/mv contexts, split-mv, coef-update probs) from `vp8/common/entropy.c`, `entropymv.c`, `modecont.c`, `treecoder.c` |
| `src/types.rs` | new + libvpx structs | `vp8/common/blockd.h` enum ordering (`MB_PREDICTION_MODE`, `MV_REFERENCE_FRAME`, `B_PREDICTION_MODE` numeric values), `modeinfo.h` |
| `src/idct.rs` | vp8v2 seed | `vp8/common/idct_blk.c`, `idctllm.c` (scalar); `inv_walsh4x1`/`inv_walsh4x4` |
| `src/predict.rs` | vp8v2 seed | `vp8/common/reconintra.c`, `reconintra4x4.c`, `vpx_dsp/intrapred.c` — incl. `intra_prediction_down_copy` TR context and the `b_hd` corner-average asymmetry |
| `src/loopfilter.rs` | vp8v2 seed | `vp8/common/loopfilter.c`, `vp8/decoder/…` row filtering — normal + simple filters, sharpness clamp, per-MB level derivation (`vp8_loop_filter_frame_init`, `loop_filter_level`) |
| `src/tokens.rs` | vp8v2 seed, verified vs libvpx | `vp8/decoder/detokenize.c` — `GetCoeffs`/`GetSigned`, EOB context, `vp8_reset_mb_tokens_context`, block-type selection, coefficient scratch-buffer zeroing semantics |
| `src/header.rs` | vp8v2 seed (keyframe) + libvpx port (inter) | `vp8/decoder/decodeframe.c` — frame tag, color space/version, segmentation, LF deltas, `multi_token_partition`, quantizer + `get_delta_q`, refresh/copy/sign-bias flag order, `refresh_entropy_probs`, coef-prob updates |
| `src/inter.rs` | libvpx port | `vp8/decoder/decodemv.c` — `vp8_decode_mode_mvs`, `decode_mb_mode_mvs`, `read_mb_modes_mv`, `decode_split_mv`, `read_kf_modes`, `read_mvcontexts`, `mb_mode_mv_init`; `vp8/common/findnearmv.h` (`cnt` buckets, sign-bias flip); `entropymv.h` trees |
| `src/mc.rs` | libvpx port | `vp8/common/filter.c` sixtap/bilinear predictor kernels, `vp8/common/reconinter.c` dispatch — per-subblock MC, `vp8_check_mv_bounds`, `vp8_clamp_mv2`, `fullpixel_mask`, `mv_bias` (incl. `BILINEAR_ONLY`/`FULLPIXEL_ONLY` behavior for versions 1 & 3) |
| `src/framebuf.rs` | new (libvpx semantics) | `vp8/common/alloccommon.c` — MB-padded YV12 buffers + 32px border; `vp8_setup_intra_recon*` border values (top=127, left=129 rewritten per MB row); `onyxd_if.c` frame-buffer roles (NEW/LAST/GOLDEN/ALTREF index layout) |
| `src/decoder.rs` | new orchestration | `vp8/decoder/decodeframe.c` `decode_mb_rows` (per-row partition cycle `mb_row % num_part`, recon order, deferred row filter, `mb_skip_coeff = eobtotal==0` rewrite) + `vp8/decoder/onyxd_if.c` `swap_frame_buffers` (copy order: ARF copy → GF copy → refresh gf → refresh arf → refresh last; `frame_to_show` = new when `!refresh_last`, last otherwise; `show_frame=0` suppresses output) |
| `src/error.rs`, `src/lib.rs` | new | API surface per `vp8-port-brief.md` (rusty_vp9-shaped push/pull) |

## Encoder file-level map (feature `encoder`, 2026-09-30)

The encoder shares the decoder's `tables.rs`/`predict.rs`/`idct.rs`/`mc.rs`/
`framebuf.rs`/`inter.rs` (`find_near_mvs` was extracted there for shared
use) so prediction, reconstruction, and context semantics cannot drift.

| zenvp8 file | origin | primary C reference |
|---|---|---|
| `src/enc/boolw.rs` | libvpx port | `vp8/encoder/boolhuff.{c,h}` — `vp8_encode_bool`, `vp8_start_encode`, `vp8_stop_encode` carry/flush semantics. Round-trip-tested against `BoolReader`. |
| `src/enc/dct.rs` | libvpx port | `vp8/encoder/dct.c` — `vp8_short_fdct4x4` (incl. the `if(a1!=0)` conditional), `vp8_short_walsh4x4` (in-place second stage). |
| `src/enc/quant.rs` | libvpx port | `vp8/encoder/quantize.c` — `vp8_regular_quantize_b_c` (zrun zbin boost, `invert_quant` i16 wrap), `vp8cx_init_quantizer`. Dequant tables come from `src/tables.rs` (`DC_QUANT`/`AC_QUANT`/`VP8_AC_TABLE2`) — the same verified tables the decoder uses, NOT a re-derived set. |
| `src/enc/tokens.rs` | libvpx port | `vp8/encoder/tokenize.c` — `vp8_tokenize_mb` into `TokRec`s then `vp8_pack_tokens` (EOB/`skip_eob_node` grammar identical to `detokenize.c`'s reader); `vp8/common/blockd.h` `BLOCK2ABOVE`/`BLOCK2LEFT` (pure raster order). |
| `src/enc/mv.rs` | libvpx port | `vp8/encoder/encodemv.{c,h}` — `vp8_encode_motion_vector`, `encode_component` (small/large trees, conditional bit-3), eighth-pel coded units. |
| `src/enc/adapt.rs` | libvpx port | `vp8/encoder/bitstream.c` — `vp8_update_coef_probs`, `update_mode`, `vp8_write_mvprobs`, `vp8_convert_rfct_to_prob`, `vp8_prob_from_total`, `vp8_tree_probs_from_distribution`/`prob_update_savings` (emit updates only when the savings test passes); per-frame symbol counts (`x->ymode_count`/`uv_mode_count`/`count_mb_ref_frame_usage`/`MVcount`). |
| `src/enc/lf.rs` | libvpx port | `vp8/encoder/picklpf.c` — `vp8cx_pick_filter_level_fast` (mid-frame strip probe) + `vp8_loop_filter_frame` driver on the reconstruction before it becomes LAST; luma-only `filter_*_y` helpers live in `src/loopfilter.rs`. |
| `src/enc/metrics.rs` | libvpx port (staged) | `vpx_dsp/variance.c` — `variance`/`MSE`/`VAR` family + `sixtap_16x16` fetch; staged for the planned RD mode-decision port, not on the live path. |
| `src/enc/costs.rs` | libvpx port (staged) | `boolhuff.c`/`treewriter.c`/`modecosts.c`/`rdopt.c`/`encodemv.c`/`mcomp.c`/`onyx_if.c` — rate-cost and Speed-5 thresh-map machinery for the planned mode-decision port, not on the live path. |
| `src/enc/encoder.rs` | new orchestration | Field order mirrors `decodeframe.c`'s parse order (it is the canonical emission order): tag/keyframe preamble, segmentation, LF params + LF deltas (dedup'd vs `last_*_lf_deltas`, reset to realtime defaults each keyframe like `setup_features`), `multi_token_partition`, quantizer deltas, refresh/copy/sign-bias flags, `refresh_entropy`, coef-prob updates, `mb_no_coeff_skip` + `prob_skip_false`, intra/inter prob literals, ymode/uv/MV prob updates, then per-MB modes; recon closes the loop through the same functions the decoder uses. |

### Encoder-specific deviations (v1 scope)

- One token partition (`multi_token_partition = 0`); the decoder handles
  all 1/2/4/8 counts but the encoder never emits them.
- No `SPLITMV`, no inter `B_PRED`, no GOLDEN/ALTREF prediction sources or
  refresh, no segmentation, no `update_mb_segmentation`. All are
  decodable features the encoder simply never chooses.
- Mode selection is a small SAD scan (4 intra modes + ZEROMV/NEARESTMV/
  NEARMV/NEWMV with a short diamond MV search) — not libvpx's RD pipeline.
  The ported Speed-5 cost/thresh machinery in `costs.rs`/`metrics.rs` is
  staged for a future faithful mode-decision port.
- NEWMV deltas are snapped to even (VP8 codes MV deltas `>>1`), so the
  searched vector round-trips exactly; edge-clamped odd candidates would
  otherwise desync.
- `keyframe_interval` forced keyframes and one-packet-per-frame pacing are
  conveniences layered on top; libvpx exposes them via encoder config.
- `qindex` is clamped to 0..=127 (the VP8 base-quantizer range).

## Behavioral notes / deliberate deviations

- **No error concealment.** libvpx's `error_concealment.c` is not ported
  (deferred per the brief). On corrupt input zenvp8 returns `Err` at the
  boundary libvpx's `vp8dx_bool_error` flags corruption; libvpx proceeds
  best-effort when built with concealment. Evidence: `corrupt_mid` case in
  `results-vp8/compare-vp8.json` decodes best-effort with a small pixel
  diff instead of filling a concealment region.
- **Resource cap.** `DecodeError::TooLarge` rejects frames whose
  macroblock-padded plane allocation would exceed an internal limit —
  libvpx allocates without a soft cap.
- **`copy_buffer_to_*` value 3.** libvpx raises `VPX_CODEC_CORRUPT_FRAME`;
  zenvp8 returns `DecodeError::Corrupt` — same refusal, typed.
- **Invisible packets** (`show_frame=0`) update reference state and emit
  no `DecodedFrame`, matching libvpx `frame_to_show` semantics.

## Oracle caveat discovered during qualification (2026-09-29)

ffmpeg's libvpx *decoder wrapper* cannot represent suppressed packets: for
a stream with `show_frame=0` packets it emits a **duplicate of the
previous shown frame** into the hidden packet's slot, shifting every
subsequent frame's alignment. ffmpeg's *native* vp8 decoder shows hidden
frames outright. Neither pipe is a valid reference for invisible-packet
streams — `tools/vp8-dec-raw.c` (direct `vpx_codec_decode()` +
`vpx_codec_get_frame()` loop) reproduces libvpx's real emitted sequence
and is the primary oracle for `v0_arf2p`. `gen_vp8_cases.py` selects it
automatically when a stream contains suppressed packets.
