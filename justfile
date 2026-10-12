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

# Both feature states must keep capability and operation results synchronized.
tiff-metadata-check:
    cargo test -p zentiff --features zencodec --test inventory_feature
    cargo test -p zentiff --features inventory --test inventory_feature --test inventory --test fuzz_regression
    cargo clippy -p zentiff --features inventory --all-targets -- -D warnings

tiff-inventory-oracle:
    INVENTORY_ORACLE_EXIFTOOL=exiftool cargo test -p zentiff --features inventory --test inventory exiftool_oracle_agrees -- --nocapture

# Validate the inventory target without starting a fuzz campaign.
tiff-inventory-fuzz-check:
    cargo check --manifest-path zentiff/fuzz/Cargo.toml --features inventory --bin inventory

tiff-metadata-target-check:
    cargo check -p zentiff --no-default-features --features inventory --target wasm32-wasip1
    cargo test -p zentiff --all-features --lib --test fuzz_regression

# Compare the adapter as well as native APIs with the published release.
tiff-metadata-semver:
    cargo semver-checks check-release -p zentiff --baseline-version 0.1.2 --default-features --features zencodec --current-features inventory

# Format check
fmt-check:
    cargo fmt --all -- --check

# Format
fmt:
    cargo fmt --all

# Clippy across workspace
clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

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

# Run the 32-bit corpus checks with an explicit caller-owned cache and runner.
tiff-inventory-i686-corpus runner="env":
    CODEC_CORPUS_CACHE=target/corpus-cache CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_RUNNER="{{runner}}" cargo test -p zentiff --features inventory --test inventory --target i686-unknown-linux-gnu corpus_inventories_pass_check_inventory

# Compare limited ICC retention and the complete inventory suite on both widths.
tiff-inventory-portability runner="env":
    CODEC_CORPUS_CACHE=target/corpus-cache cargo test -p zentiff --features inventory --test inventory
    CODEC_CORPUS_CACHE=target/corpus-cache CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_RUNNER="{{runner}}" cargo test -p zentiff --features inventory --test inventory --target i686-unknown-linux-gnu
