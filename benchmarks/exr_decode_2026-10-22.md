# EXR lossy decode speed: B44, B44A, DWAA, DWAB (2026-10-22)

zenexr / `exr` 1.74.2 against OpenEXR 3.5.2, single thread. Harness and method:
[`tools/exr-decode-bench`](../tools/exr-decode-bench/README.md). Full tables:
[`exr_decode_2026-10-22_t1_summary.md`](exr_decode_2026-10-22_t1_summary.md).
Raw data: [`exr_decode_2026-10-22.pointer.md`](exr_decode_2026-10-22.pointer.md).

## Setup

- Host `i270`: Core Ultra 7 270K Plus (8 P-cores 0-7, 16 E-cores; AVX2, F16C,
  no AVX-512), 30 GiB, Ubuntu, governor `powersave` (HWP). Every timed process
  was pinned to P-core 2. A watcher logged other processes; none reached half a
  core during the run, so no file was dropped.
- OpenEXR 3.5.2 (`69b2604f`), CMake Release, internal Imath 3.2.3 and
  libdeflate, g++ 15.2.0, no `-march`. `exr` 1.74.2 and zenexr at zenextras
  `e014062c`, rustc 1.99.0, default `--release`, no `target-cpu`.
- Corpus: 12 RGB(A) scanline/tiled images from
  [openexr-images](https://github.com/AcademySoftwareFoundation/openexr-images)
  (ScanLines, Tiles, Beachball; 0.24-1.1 MP) and 4 Poly Haven 4K HDRIs
  ([kloofendal_48d_partly_cloudy_puresky](https://polyhaven.com/a/kloofendal_48d_partly_cloudy_puresky),
  [venice_sunset](https://polyhaven.com/a/venice_sunset),
  [studio_small_08](https://polyhaven.com/a/studio_small_08),
  [brown_photostudio_02](https://polyhaven.com/a/brown_photostudio_02); 4096x2048, f32 → half).
  The v2 LeftView images are deep data, which `exr` rejects, so they are not
  included. Each image gives centre crops of 64², 256² and 1024² (where it fits)
  plus its full frame: 53 R,G,B HALF inputs, re-encoded with each compression.
- 793 encoded files. Every decoder matched OpenEXR bit for bit on every file
  (half planes and f32 RGB), and the lossless files matched their sources.
- Two rounds per file, C++ and Rust processes alternating; each value is the
  lower round median. Rounds agreed to 0.3% (median) and 1.8% (p90).

## Results, single thread (MP/s, geomean over images, OpenEXR-encoded files)

`*-f16-planar` decodes to half planes; `*-f32-rgb` to interleaved RGB f32,
the layout zenexr returns. `exr-blocks` is `exr`'s chunk decompression with a
plain copy out: the codec without `exr`'s image assembly.

Medium, 0.24-1.1 MP (17 images):

| compression | OpenEXR f16 | OpenEXR f32 | exr-blocks f16 | exr f16 | exr f32 | zenexr f32 | bpp |
|---|---|---|---|---|---|---|---|
| B44 | 421 | 248 | 251 | 236 | 178 | 90 | 21.2 |
| B44A | 431 | 252 | 255 | 239 | 180 | 91 | 19.8 |
| DWAA 45 | 153 | 120 | 45 | 44 | 40 | 34 | 9.8 |
| DWAB 45 | 154 | 121 | 57 | 56 | 50 | 42 | 8.9 |
| *ZIP (ref)* | 127 | 105 | 125 | 118 | 101 | 64 | 21.8 |
| *PIZ (ref)* | 103 | 87 | 42 | 41 | 38 | 32 | 22.8 |

Large, 4096x2048 (4 images):

| compression | OpenEXR f16 | OpenEXR f32 | exr-blocks f16 | exr f16 | exr f32 | zenexr f32 |
|---|---|---|---|---|---|---|
| B44 | 231 | 142 | 184 | 139 | 116 | 63 |
| B44A | 232 | 142 | 185 | 139 | 116 | 63 |
| DWAA 45 | 146 | 105 | 61 | 53 | 51 | 37 |
| DWAB 45 | 143 | 100 | 65 | 55 | 52 | 38 |

Per-call time at 64x64 (µs, 16 images), which is mostly fixed cost:

| compression | OpenEXR f16 | exr-blocks f16 | zenexr f32 |
|---|---|---|---|
| B44 / B44A | 13 / 13 | 20 / 20 | 36 / 35 |
| DWAA 15 / 45 / 150 | 40 / 36 / 31 | 842 / 825 / 811 | 861 / 843 / 841 |
| DWAB 15 / 45 / 150 | 34 / 30 / 24 | 445 / 437 / 423 | 472 / 464 / 452 |
| PIZ | 133 | 834 | 858 |

DWA level (medium, OpenEXR f16 / exr-blocks f16): DWAA 15 → 150 is 122 → 215 /
39 → 56 MP/s; DWAB 15 → 150 is 124 → 218 / 47 → 73 MP/s. Lower levels keep
more AC coefficients and decode more slowly.

## Findings

1. **Within each decoder:** B44 and B44A decode 2.8x faster than DWA in OpenEXR
   at medium sizes (421-431 vs 153-154 MP/s) and 1.6x at 4096x2048, at about
   twice DWA's size (B44 21.2 bpp vs DWAA 45 9.8). B44A matches B44 or beats it
   by about 2% (flat blocks are cheaper), and files are 7% smaller. In OpenEXR,
   DWAA and DWAB run at the same speed at 0.24 MP and above. DWAB is 1.2x faster
   at 64² and 1.1x at 256², and 10% smaller at the same level.
2. **In `exr`, DWAB beats DWAA** (57 vs 45 MP/s at medium, 2x at 64²): every
   chunk carries about 0.4 ms of setup, and DWAA has 8x as many chunks (32 vs
   256 lines). Callgrind over the four Rust arms at 64x64 DWAA puts 77% of instructions in the PIZ
   Huffman table build `exr` reuses for the AC stream (`build_decoding_table`
   41%, `build_canonical_table` 18%, then memsets and `Vec<Code>` fills of the
   65,537-entry tables), rebuilt per chunk. PIZ shows the same cost (834 µs at
   64²). At 1 MP that setup is about 26% of DWAA's instructions and 5% of
   DWAB's; the rest of the gap is in `decode_lossy_dct_group`, Huffman decode,
   `from_half_zigzag` and `write_scanlines`.
3. **OpenEXR vs `exr`, decoding the same bitstream to the same half planes:**
   1.7x faster on B44/B44A at medium (1.25x at 4096x2048), 3.4x on DWAA 45 and
   2.7x on DWAB 45 at medium (2.2-2.4x at 4096x2048), and 13-26x on DWA at 64²
   because of finding 2. ZIP is level (1.02x).
4. **zenexr adds a large conversion cost on top of `exr`**: it reads through
   `exr`'s per-pixel `rgba_channels` callback into a `PixelBuffer`, looking up
   the row on every pixel. Uncompressed medium frames decode at 126 MP/s against
   `exr`'s own f32 read at 495 and OpenEXR f32 at 499. For B44 at medium,
   zenexr is 2.7x slower than OpenEXR f32 output; for DWA 45 it is 2.9-3.6x.
5. **The bitstream doesn't matter:** files from the `exr` encoder and the
   OpenEXR encoder decode at the same speed in every decoder (0.99-1.02x), and
   sizes agree within 1%.
6. **The cache cliff is real.** At 4096x2048 the 50 MB half output (100 MB as
   f32) leaves L3, and the allocation is inside the timed call. Uncompressed
   OpenEXR f16 drops from 2614 to 478 MP/s and B44 from 421 to 231. A single
   `alpha + beta * pixels` fit does not describe these decoders, so the summary
   reports per-size tables and the 64² per-call time instead.

## Encoder bug found while preparing inputs

`exr` 1.74.2's B44 and B44A encoder panics when the last scanline block holds
exactly one row (PrismsLenses, 1200x865: 865 = 27·32 + 1): `range end index
7210 out of range for slice of length 7200` at `compression/b44/mod.rs:675`.
The output buffer is sized to the block's uncompressed bytes (1 row: 7,200),
but B44 pads that row to 4x4 blocks (300 blocks x 3 channels x 14 bytes =
12,600). Those two files are missing from the `exr`-encoder set (52 instead of
53). Decoding is unaffected.

## Not covered

- Multi-threaded decode: the 8-thread run on i270 lost 607 of 793 files to a
  concurrent 24-core training job; the re-run waits for an idle box.
- Alpha, tiled outputs and mip levels: inputs are R,G,B HALF scanline files.
- Zen 5 / AVX-512: not measured.
- OpenEXR 3.1.12 (the Ubuntu package): not measured. Its Core C symbols clash
  with 3.5's, so it needs a separate build of the C++ side.
