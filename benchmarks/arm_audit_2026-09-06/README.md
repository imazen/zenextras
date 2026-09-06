# ARM audit — 2026-09-06

Main baseline `e17bd6ca`, Apple M4 Pro, Rust 1.98. Builds and measurements
serialized under nice -n19 with four build/Rayon/OMP workers and no
`target-cpu=native`. These four wrappers have no independent archmage
runtime-tier entry points: TIFF delegates its core to image-tiff, JPEG 2000
to hayro-jpeg2000, PDF to hayro, and SVG to resvg. Absolute native costs below
must not be labeled NEON-versus-scalar speedups.

| Operation | 64 width | 256 width | 1024 width | 4096 width |
|---|---:|---:|---:|---:|
| JP2 decode | 1.0 ms | 17.6 ms | 94.7 ms | 820.1 ms |
| PDF render | 89.5 us | 314.8 us | 3.9 ms | 27.7 ms |
| SVG render | 77.7 us | 255.3 us | 2.0 ms | 18.6 ms |
| TIFF uncompressed RGB8 decode | 6.8 us | 21.6 us | 146.9 us | 2.8 ms |
| TIFF default LZW RGB8 encode | 328.4 us | 2.4 ms | 21.8 ms | 249.1 ms |

PDF outputs are 64×90, 256×362, 1024×1448 and 4096×5792, from the repository's
text PDF fixture. SVG uses the committed benchmark's geometric composition,
with system-font discovery explicitly disabled. TIFF uses a deterministic
RGB8 pattern and asserts exact uncompressed decode and LZW roundtrip pixels.
JP2 uses a CID22 photograph and upsampled size controls; see
[fixtures.pointer.md](fixtures.pointer.md). These fixtures do not establish
representative performance across photographs, screen content or codec modes.
No measured constants or production defaults are derived from them.

Full logs retain confidence intervals, round counts and noise warnings.
Several cells have CV above 20%; no extrapolation or backend ranking is made.
Workspace all-feature tests and strict library/bench clippy pass. Five
pre-existing ignored doctests remain; none were added by this audit.

Use `just arm-decode-audit` with explicit `JP2_BENCH_INPUTS`, and
`just arm-channel-audit` for the TIFF expansion comparison. Dependency changes
are dev-only zenbench requirements; the pinned image-tiff/hayro revisions did
not change. The lockfile was updated. No public API changed.

JPEG 2000 decoded RGB bytes match the independently retained RGB reference at
all four sizes, checked before timing. `zenjp2-reference-check.log` records the
check and a filtered timing run; the timed decode body is unchanged.

TIFF's existing slice-write conversions beat the retained push-loop comparisons
in all 16 measured cells. Both arms allocate their output. At 4096 squared,
slice/push means were 31.5/37.5 ms (CMYK8), 32.3/37.7 ms (CMYK16),
10.8/34.5 ms (CMYK float), and 10.1/24.4 ms (palette). The complete size grid and
paired intervals are in [zentiff-channel-audit.log](zentiff-channel-audit.log).
These compare loop structures already present in the benchmark; this audit
has not changed production TIFF conversion code. `convert_cmyk` and
`expand_palette` in `zentiff/src/decode.rs` already use the slice-write shape.

Assembly inspection confirms CMYK float slice writes auto-vectorize into
`fsub.4s`, `fmul.4s` and `st4.4s`, while push loops retain per-channel
capacity-growth call sites. See [assembly provenance](channel-expand.pointer.md).
No explicit magetypes port is justified by these measurements.

Remaining: integration validation.
