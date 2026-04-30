# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors

# Default target — list available recipes.
default:
    @just --list

# Cargo check across the entire workspace, all targets.
check:
    cargo check --workspace --all-targets --all-features

# Run all tests across the workspace.
test:
    cargo test --workspace --all-features

# Run clippy with our lint config and treat warnings as errors.
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Auto-format the entire codebase.
fmt:
    cargo fmt --all

# Verify formatting in CI mode (does not modify files).
fmt-check:
    cargo fmt --all --check

# Run cargo-deny for license / advisory / source policy checks.
audit:
    cargo deny check

# Verify SPDX headers on every file in the repo (REUSE 3.0 compliance).
reuse:
    reuse lint

# Regenerate prost-generated Rust types from `schemas/`.
# Wired via `crates/lakko-proto/build.rs` (uses `protox`, no protoc required).
regen-proto:
    cargo build -p lakko-proto

# Regenerate Swift + Kotlin bindings from lakko-ffi-uniffi.
# Requires uniffi-bindgen — install once with: cargo install uniffi-bindgen
regen-bindings:
    cargo build -p lakko-ffi-uniffi --release
    uniffi-bindgen generate \
        --library target/release/liblakko_ffi_uniffi.dylib \
        --language swift \
        --out-dir target/bindings/swift
    uniffi-bindgen generate \
        --library target/release/liblakko_ffi_uniffi.dylib \
        --language kotlin \
        --out-dir target/bindings/kotlin

# Smoke-build all fuzz targets on stable (verifies harnesses compile).
# Real fuzzing requires nightly + cargo-fuzz; see `just fuzz <crate> <target>`.
fuzz-check:
    cargo check --manifest-path crates/lakko-mls/fuzz/Cargo.toml
    cargo check --manifest-path crates/lakko-proto/fuzz/Cargo.toml

# Run a single fuzz target with cargo-fuzz on nightly.
# Prerequisites (one-time):
#   rustup toolchain install nightly
#   cargo install cargo-fuzz
# Examples:
#   just fuzz lakko-mls mls_message
#   just fuzz lakko-proto wire_envelope
fuzz crate target:
    cd crates/{{crate}}/fuzz && cargo +nightly fuzz run {{target}} -- -max_total_time=300

# List all available fuzz targets.
fuzz-list:
    @echo "lakko-mls:"
    @ls crates/lakko-mls/fuzz/fuzz_targets/ | sed 's/\.rs$//' | sed 's/^/  /'
    @echo "lakko-proto:"
    @ls crates/lakko-proto/fuzz/fuzz_targets/ | sed 's/\.rs$//' | sed 's/^/  /'

# Everything CI does — run locally before pushing.
ci: fmt-check lint test audit reuse fuzz-check

# Build for all release targets (host platform only here; cross-builds via Nix).
build:
    cargo build --workspace --release
