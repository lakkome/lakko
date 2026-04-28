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
# (Implementation will land with `lakko-proto` build script in Phase 1.)
regen-proto:
    @echo "TODO: wire up prost-build invocation in crates/lakko-proto/build.rs"

# Run a single fuzz target (cargo-fuzz). Example: `just fuzz mls-parse`.
fuzz target:
    cargo fuzz run {{target}} -- -max_total_time=300

# Everything CI does — run locally before pushing.
ci: fmt-check lint test audit reuse

# Build for all release targets (host platform only here; cross-builds via Nix).
build:
    cargo build --workspace --release
