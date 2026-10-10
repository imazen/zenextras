# zenextras workspace tasks

# Default: show available recipes
default:
    @just --list

# Build all workspace crates
build:
    cargo build --workspace

# Run all workspace tests
test:
    cargo test --workspace

# Format check
fmt-check:
    cargo fmt --all -- --check

# Format
fmt:
    cargo fmt --all

# Clippy across workspace
clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# Regenerate the public-API surface snapshots (docs/public-api/, one set per
# member crate). Needs a nightly toolchain for rustdoc JSON; never run by CI.
api-doc:
    cargo test --manifest-path apidoc/Cargo.toml

# Verify the committed snapshots are current
api-doc-check:
    ZEN_API_DOC=check cargo test --manifest-path apidoc/Cargo.toml

# ── Fuzz corpus management (R2-backed) ──

# List all corpora in R2 with file counts and sizes
fuzz-corpus-list:
    @./scripts/fuzz-corpus.sh list

# Pull all corpora from R2 to local working dirs (one-way)
fuzz-corpus-pull crate="" target="":
    @./scripts/fuzz-corpus.sh pull {{crate}} {{target}}

# Push local corpora to R2 (one-way)
fuzz-corpus-push crate="" target="":
    @./scripts/fuzz-corpus.sh push {{crate}} {{target}}

# Pull then push — combines both sides
fuzz-corpus-merge crate="" target="":
    @./scripts/fuzz-corpus.sh merge {{crate}} {{target}}

# Show divergence between local and R2 file counts
fuzz-corpus-diff crate="" target="":
    @./scripts/fuzz-corpus.sh diff {{crate}} {{target}}

# Run cargo fuzz cmin then push the minimized corpus to R2
# Example: just fuzz-corpus-cmin zentiff fuzz_decode
fuzz-corpus-cmin crate target:
    @./scripts/fuzz-corpus.sh cmin {{crate}} {{target}}

# Run a fuzz target locally with the merged corpus
# Example: just fuzz zentiff fuzz_decode 60
fuzz crate target seconds="60":
    @./scripts/fuzz-corpus.sh pull {{crate}} {{target}}
    cd {{crate}} && cargo +nightly fuzz run {{target}} -- -max_total_time={{seconds}}
    @./scripts/fuzz-corpus.sh push {{crate}} {{target}}

# Absolute decode/render costs for TIFF, JP2, PDF and SVG; JP2 paths are explicit.
arm-decode-audit:
    CARGO_BUILD_JOBS=4 RAYON_NUM_THREADS=4 OMP_NUM_THREADS=4 TMPDIR="$HOME/tmp" nice -n 19 cargo bench --workspace --all-features --bench arm_decode

# Exact paired TIFF channel-expansion loops across four sizes.
arm-channel-audit:
    CARGO_BUILD_JOBS=4 RAYON_NUM_THREADS=4 OMP_NUM_THREADS=4 TMPDIR="$HOME/tmp" nice -n 19 cargo bench -p zentiff --bench channel_expand

# Generate lossless JP2 size controls from a caller-supplied photograph.
# Sizes larger than the source are upsampled controls, not native-resolution photos.
arm-jp2-fixtures source destination:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p "{{destination}}"
    for side in 64 256 1024 4096; do
        MAGICK_THREAD_LIMIT=4 nice -n 19 magick "{{source}}" -filter Mitchell -resize "${side}x${side}!" -depth 8 "{{destination}}/cid22-${side}.png"
        MAGICK_THREAD_LIMIT=4 nice -n 19 magick "{{destination}}/cid22-${side}.png" -alpha off -depth 8 "rgb:{{destination}}/cid22-${side}.rgb"
        nice -n 19 opj_compress -i "{{destination}}/cid22-${side}.png" -o "{{destination}}/cid22-${side}.jp2" -r 1 -threads 4
    done

# zenjp2 inventory: corpus conformance plus the exiftool cross-check.
# INVENTORY_ORACLE_DIR defaults to the OpenJPEG/serenity files listed in hayro-jpeg2000's
# manifests (codec-corpus has no JPEG 2000 set). Fetch them with zenjp2/scripts/fetch-jp2-corpus.py.
inventory-oracle dir="$HOME/tmp/jp2-corpus/hayro-assets":
    INVENTORY_ORACLE_DIR="{{dir}}" INVENTORY_ORACLE_EXIFTOOL="$(command -v exiftool)" TMPDIR="$HOME/tmp" \
        cargo test --release -p zenjp2 --test inventory_oracle -- --nocapture --test-threads=1

# zenjp2 inventory fuzz target (libFuzzer; seconds default 600)
fuzz-inventory seconds="600":
    cd zenjp2 && cargo +nightly fuzz run inventory -- -max_total_time={{seconds}}
