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

# Preserve the cancellation cause captured during inventory traversal.
tiff-inventory-cancellation:
    cargo test -p zentiff --features inventory --test inventory mid_walk_cancellation_preserves_the_first_reason

# Reused JPEG 2000 structural, mutation and crash tests with both feature states.
jp2-metadata-check:
    cargo test -p zenjp2 --test inventory_feature
    cargo test -p zenjp2 --features inventory --lib --test inventory --test inventory_review --test inventory_feature --test fuzz_regression
    cargo clippy -p zenjp2 --features inventory --all-targets -- -D warnings
    cargo check --manifest-path zenjp2/fuzz/Cargo.toml --bin inventory

jp2-metadata-validation corpus:
    cargo clippy -p zenjp2 --features inventory --all-targets -- -D warnings
    cargo check --manifest-path zenjp2/fuzz/Cargo.toml --bin inventory
    CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_RUNNER=env cargo test -p zenjp2 --features inventory --target i686-unknown-linux-gnu --lib --test inventory --test inventory_review --test inventory_feature --test fuzz_regression
    INVENTORY_ORACLE_DIR="{{corpus}}" INVENTORY_ORACLE_EXIFTOOL=exiftool cargo test -p zenjp2 --features inventory --test inventory_oracle -- --nocapture

# No registry zenjp2 exists yet; snapshot the current public surface.
jp2-metadata-api:
    cargo test -p zenjp2 --doc --features inventory
    cargo check -p zenjp2 --no-default-features
    just api-doc

svg-metadata-check:
    cargo test -p zensvg --no-default-features --test inventory_feature
    cargo test -p zensvg --features inventory --lib --test inventory_feature --test integration --test review_adversarial --test fuzz_regression
    cargo clippy -p zensvg --features inventory --all-targets -- -D warnings
    cargo check --manifest-path zensvg/fuzz/Cargo.toml --bin inventory

# Reuse the structural assertions on 32-bit and an independent XML reader.
svg-metadata-validation corpus:
    cargo check --manifest-path zensvg/fuzz/Cargo.toml --bin inventory
    CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_RUNNER=env cargo test -p zensvg --features inventory --target i686-unknown-linux-gnu --lib --test inventory_feature --test integration --test review_adversarial --test fuzz_regression
    INVENTORY_ORACLE_XMLLINT=xmllint INVENTORY_ORACLE_SVG_DIR="{{corpus}}" cargo test -p zensvg --features inventory --test integration inventory::oracle_xmllint -- --nocapture
    cargo test -p zensvg --all-features --doc
    just api-doc

pdf-metadata-check:
    cargo test -p zenpdf --lib --tests
    cargo test -p zenpdf --features inventory --lib --test inventory --test inventory_feature --test fuzz_regression
    cargo clippy -p zenpdf --all-targets --all-features -- -D warnings
    cargo check --manifest-path zenpdf/fuzz/Cargo.toml --bin inventory

pdf-metadata-validation corpus report:
    CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_RUNNER=env cargo test -p zenpdf --features inventory --target i686-unknown-linux-gnu --lib --test inventory --test inventory_feature --test fuzz_regression
    INVENTORY_ORACLE_MUTOOL=mutool INVENTORY_ORACLE_EXIFTOOL=exiftool INVENTORY_ORACLE_PDF_DIR="{{corpus}}" INVENTORY_ORACLE_REPORT="{{report}}" cargo test -p zenpdf --features inventory --test inventory oracle_mutool_exiftool -- --nocapture
    cargo test -p zenpdf --all-features --doc
    just api-doc

pdf-metadata-api:
    #!/usr/bin/env bash
    set -euo pipefail
    baseline=$(cargo read zenpdf==0.2.0 --path-only)
    RUSTC_BOOTSTRAP=1 cargo rustdoc -p zenpdf --lib --all-features -- -Zunstable-options --output-format json
    RUSTC_BOOTSTRAP=1 cargo rustdoc --manifest-path "$baseline/Cargo.toml" --target-dir "$PWD/target/published-api/zenpdf-0.2.0" --lib --all-features -- -Zunstable-options --output-format json
    cargo semver-checks check-release -p zenpdf --current-rustdoc target/doc/zenpdf.json --baseline-rustdoc target/published-api/zenpdf-0.2.0/doc/zenpdf.json --release-type patch
