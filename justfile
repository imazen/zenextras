default:
    @just --list

# Run all tests
test:
    cargo test --all-targets
    cargo test --all-targets --features zencodec

# Run clippy
clippy:
    cargo clippy --all-targets -- -D warnings
    cargo clippy --all-targets --features zencodec -- -D warnings

# Format code
fmt:
    cargo fmt

# Check formatting
fmt-check:
    cargo fmt --check

# Feature permutation checks
feature-check:
    cargo test --all-targets
    cargo test --all-targets --features zencodec
    cargo check --all-targets --all-features

# Build release
build:
    cargo build --release --all-features

# Run all CI checks locally
ci: fmt-check clippy test feature-check
