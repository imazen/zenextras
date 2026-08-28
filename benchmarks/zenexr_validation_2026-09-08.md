# zenexr wrapper validation — September 8, 2026

`zenexr` is the EXR owner in this workspace, wrapping Rust `exr` 1.74.2. The
user explicitly selected this dependency after the imazen-26 HDR PNG source
correction. The unpushed custom zenbitmaps raw/PIZ port is retired from its
active checkout; verified checkpoint `f6140cb736e395cd70b8cf381caf50a3d6352cdf`
and its source archive remain preserved. No custom parser or PIZ implementation
was moved into this crate.

## Contract

The [crate README](../zenexr/README.md) owns the exact supported surface and its
pre-implementation API registration. One flat RGB/RGBA part, largest resolution,
scanline or tiled; f16 widens to f32 and f32 sample bits are retained for lossless
compression. The output covers the data window. Associated alpha and the source
header remain available, including origin, display window, chromaticities and
luminance attributes. No implicit exposure, tone mapping or conversion to nits.
Additional channels, UINT, subsampling, deep and multipart are explicit errors.

Input/output/pixel limits and cooperative cancellation are wrapper guarantees.
They do **not** bound upstream metadata/scratch allocations or total RSS, nor
interrupt the middle of a compression block. The default decoder is serial.

## Independent pixels and real references

The reference export example calls `ExrDecoderConfig::decode` and writes explicit
little-endian interleaved f32 pixels with dimensions/stride and the full source
metadata. All comparisons require equal dimensions, sample counts and bytes.

| Set | Files | Pixels | Bit-exact f32 samples |
|---|---:|---:|---:|
| Saved independent synthetic fixtures | 98 | 606,600 | 1,819,800 |
| Protected UPIQ HDR reference files | 30 | 40,930,048 | 122,790,144 |
| Total | 128 | 41,536,648 | 124,609,944 |

Synthetic expected pixels are the original input to the previously saved
OpenEXR 3.1.11 fixture generator, independent of `exr`. They cover half, float
and mixed channel types, raw/PIZ, increasing/decreasing scanline ordering,
positive/negative origins, negative values, signed zero and subnormals, from
1×1 through wide 1025×33 fields. The largest PIZ cases exercise full 16-bit
wavelet lookup tables. One small independent fixture is committed with
[its provenance](../zenexr/tests/fixtures/README.md).

The 30 real reference outputs match the prior custom port's saved results
exactly. That is cross-implementation agreement, not a second independent
ground-truth measurement of the real images. Source and expected-output hashes
were checked against the previous immutable result before comparison. The new
decoder retains complete metadata alongside each output.

## Local verification

- Eight new behavior tests pass, including alpha, metadata, lossless RLE/ZIP/PIZ
  scanline/tile layouts, malformed/truncated data, limits, cancellation before
  and during I/O, and refusal of UINT/extra-channel/multipart information loss.
- `cargo test --locked --workspace --all-features`: **277 passed, 0 failed**,
  with five pre-existing ignored documentation examples; no new ignored tests.
- Workspace Clippy passes with `--all-targets --all-features -- -D warnings`.
- Workspace `--no-default-features` check, zenexr all-target checks on Rust 1.93
  and i686, workspace formatting and `just api-doc-check` pass. The i686 result
  is a compile check, not execution on a 32-bit machine.
- API snapshots regenerated. Existing SVG/TIFF snapshots change only rustdoc's
  spelling of `std::io::Error`; no existing crate source or API changed.

The lockfile adds `zenexr`, `exr` and eight transitive packages. No previously
locked package or dependency metadata changed. Both pinned TIFF/PDF fork
patches remain unchanged.

## Reproduction and interpretation

Artifacts: `/mnt/v/output/zensim/zenexr-wrapper-2026-09-08/`. `SOURCE_INPUTS.json`
binds the reviewed source, lockfile and executable. `parity-final/RESULT.json`
records each source/expected/output SHA-256 and sample count. The artifact's
`validate.py` only launches the Rust export example and compares bytes; it does
not decode images. Run into a fresh destination:

```sh
cargo build --locked --release -p zenexr --example decode
python3 /mnt/v/output/zensim/zenexr-wrapper-2026-09-08/validate.py \
  /mnt/v/output/zensim/native-exr-port-2026-09-08 \
  "$PWD/target/release/examples/decode" /path/to/fresh-parity-directory
```

No throughput improvement is claimed. No image-data fitting, human-score read,
distorted-holdout scoring or model qualification occurred. UPIQ references remain
protected overlap-audit inputs; their fingerprinting/contextual review is still
required. HDR development uses the already available imazen-26 HDR PNG corpus.
