# exr-decode-bench

Decode speed of the EXR lossy compressions (B44, B44A, DWAA, DWAB, with NONE,
ZIP and PIZ as lossless references) in zenexr / the `exr` crate against the
OpenEXR C++ library. Test tooling only: a standalone Cargo workspace plus a C++
binary, so the zenextras workspace and CI never build or link OpenEXR.

Results: [`benchmarks/exr_decode_2026-10-22.md`](../../benchmarks/exr_decode_2026-10-22.md).

## What makes the comparison fair

- **Same bytes.** Every file is decoded by every decoder. Inputs are written by
  the OpenEXR encoder (the bitstreams found in the wild); the default-level
  B44/B44A/DWAA/DWAB files are also written by the `exr` encoder, to separate
  bitstream effects from decoder effects.
- **Same pixels out.** `verify` checks every decoder against OpenEXR bit for
  bit (half planes and f32 RGB) and checks the lossless files against the
  source. A mismatch fails the run, because a wrong decoder's speed means
  nothing.
- **Same output layouts.** `*-f16-planar` arms write R, G, B half planes;
  `*-f32-rgb` arms write interleaved RGB f32, the layout zenexr returns.
  Output allocation is inside the timed call on both sides.
- **Same input path.** Both decode from an in-memory buffer. The C++ stream
  implements OpenEXR's stateless positional reads, the path its own file
  reader takes.
- **Same threads.** `threads=1`: OpenEXR thread count 0 (calling thread) vs
  `exr`'s `non_parallel()`. `threads=N`: OpenEXR global pool of N vs rayon
  with N threads. `exr`'s parallel reader builds a new rayon pool on every
  call, so the `exr-blocks-f16-planar-pool` arm decompresses `exr` chunks on a
  persistent pool to separate codec cost from that per-call cost.
- **Same timing protocol.** One warm-up call per arm, then rounds in shuffled
  arm order until each arm has 5 samples and 200 ms (or 5000 samples). The
  driver alternates the C++ and Rust processes per file, flipping order each
  round, and the summary takes each arm's lower round median. Processes are
  pinned with `taskset`; a watcher logs any other process over 15% CPU and the
  summary drops files measured during that time.
- **Generic builds.** Rust default `--release`, no `target-cpu=native`.
  OpenEXR CMake `Release` (no `-march`); its DWA path picks F16C/AVX at run
  time.

## Arms

| arm | what it measures |
|---|---|
| `openexr-f16-planar` | `Imf::InputFile` + `FrameBuffer` of HALF slices |
| `openexr-f32-rgb` | same, FLOAT slices, interleaved RGB |
| `exr-blocks-f16-planar` | `exr` chunk decompression (`UncompressedBlock::decompress_chunk`) + copy to planes; codec cost |
| `exr-blocks-f16-planar-pool` | the same on a persistent rayon pool (threads > 1) |
| `exr-f16-planar` | `exr` high-level `all_channels()` read (per-call pool when threads > 1) |
| `exr-f32-rgb` | `exr` high-level `rgb_channels()` into interleaved f32 |
| `zenexr-f32-rgb` | `zenexr::ExrDecoderConfig::decode` as shipped (always single-threaded) |

## Running

```sh
# OpenEXR from source (the version under test), then the C++ side
cmake -S openexr -B build -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=$PREFIX \
  -DOPENEXR_FORCE_INTERNAL_IMATH=ON -DOPENEXR_FORCE_INTERNAL_DEFLATE=ON && ninja -C build install
cmake -S cpp -B cpp-build -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_PREFIX_PATH=$PREFIX \
  -DCMAKE_BUILD_RPATH=$PREFIX/lib && ninja -C cpp-build
cargo build --release

B=cpp-build/openexr-bench
exr-decode-bench prepare <dir of RGB(A) source EXRs> prepared $B   # crops 64², 256², 1024², full
exr-decode-bench verify prepared $B verify.csv
taskset -c 2   exr-decode-bench run prepared $B 1 2 200 results-t1.csv
taskset -c 0-7 exr-decode-bench run prepared $B 8 2 200 results-t8.csv
python3 summarize.py results-t1.csv contention-t1.log
```

`watch-contention.sh <log> <stop-file>` produces the contention log.
