# Channel expansion: `Vec::push` → preallocated slice writes

**Date:** 2026-08-01
**Box:** Apple M4 Pro (aarch64-apple-darwin), `--release`, no `target-cpu=native`
**Harness:** zenbench 0.1.9 (interleaved, paired statistics), `benches/channel_expand.rs`
**Command:** `cargo bench -p zentiff --bench channel_expand` (~16 s)
**Crate:** zentiff 0.1.2 in `zenextras`

## What changed

`decode.rs`'s five channel-expansion loops filled their output with per-element
`Vec::push` into a `Vec::with_capacity`. They now write into a preallocated,
zero-filled buffer through `chunks_exact_mut`.

`push` re-checks capacity on every element and its possible reallocation blocks
vectorization, so LLVM cannot widen the stores. `chunks_exact_mut` gives it a
known-length destination. The zero-fill is not an extra pass on the infallible
path — `vec![T::default(); n]` for a zeroable `T` lowers to one `calloc`, served
from already-zeroed pages.

The fallible-allocation contract is preserved: `alloc_util::vec_zeroed` is the
length-returning counterpart of `vec_with_capacity`, with the same `AllocPref`
handling and the same `LimitExceeded` error on OOM.

Sites converted (all in `src/decode.rs`): CMYK→RGBA for U8, I8-signed, U16 and
F32, plus palette-index→RGB8.

## Measured

1 MP per cell, alpha present, 30 rounds:

| conversion | slice | push | slice is | throughput |
|---|---|---|---|---|
| cmyk8 → rgba8 | 2.0 ±0.0 ms | 2.3 ±0.0 ms | **1.15× faster** (push +14.8…+15.6 %) | 1.95 vs 1.69 GiB/s |
| cmyk16 → rgba16 | 2.1 ±0.0 ms | 2.3 ±0.0 ms | **1.12× faster** (push +11.5…+12.5 %) | 3.77 vs 3.37 GiB/s |
| cmykf32 → rgbaf32 | 644.4 ±9.3 µs | 2148.8 ±24.0 µs | **3.34× faster** (push +234…+237 %) | 24.2 vs 7.27 GiB/s |
| palette → rgb8 | 591.6 ±9.6 µs | 1459.9 ±6.3 µs | **2.47× faster** (push +146…+148 %) | 4.95 vs 2.01 GiB/s |

Ranges are zenbench's 95 % CI against the slice baseline; every interval excludes
zero by a wide margin.

**Why the spread is 1.12× to 3.34×, not uniform:** the u8 and u16 CMYK arms spend
most of their time in float division and the `+0.5` rounding conversion, so the
store pattern is a minority of the work. The f32 arm does two multiplies and no
division, and the palette arm does three table lookups and shifts — in both, the
fill dominates, and removing the capacity check per element is most of the run
time.

## Provenance, and a number NOT carried over

This work was replayed from `imazen/zentiff` commit `d8a52a1` (2026-07-28), whose
message claims **13-40×**. **That figure is not reproduced here and is not
claimed.** It was measured on the standalone `zentiff` repo (now archived) against
*different* functions — `expand_graya_to_rgba_u8` and `truncate_channels`, which
do not exist in this crate. The conversions here are arithmetic-heavier, so the
same transformation buys proportionally less. The numbers above are this crate,
this box, this run.

## Correctness

- Both arms are asserted **bit-identical** before any timing runs
  (`assert_arms_identical`, called at the top of the bench). It is an assertion
  rather than a `#[test]` deliberately: this target is `harness = false`, so a
  `#[test]` in a bench file is never executed — verified by observation, as
  `cargo test --bench channel_expand` reports no tests at all.
  Teeth-tested: perturbing one rounding constant to `+0.6` fails it with
  `assertion left == right failed: cmyk8 alpha`.
- Full crate suite green before and after: **69 passed / 0 failed**.

## Scope

aarch64 only; x86-64 not re-measured. Single-threaded. The `.chunks_exact` bound
is exact by construction (`pixel_count == data.len() / src_channels`), so no
pixels are dropped relative to the previous `0..pixel_count` loop.
