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
