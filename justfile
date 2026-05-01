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

# ---- Local dev (на твоей машине) ------------------------------------------

# Поднять ephemeral MariaDB через docker/podman для local dev.
# Останавливается через `just dev-mariadb-stop`. Данные не персистятся.
# Нужен docker ИЛИ podman в PATH.
dev-mariadb:
    @command -v docker >/dev/null 2>&1 && CONTAINER=docker || CONTAINER=podman; \
    $CONTAINER run -d --rm --name lakko-mariadb-dev \
        -p 3306:3306 \
        -e MARIADB_ROOT_PASSWORD=dev \
        -e MARIADB_DATABASE=lakko \
        -e MARIADB_USER=lakko \
        -e MARIADB_PASSWORD=devpass \
        docker.io/library/mariadb:11
    @echo ""
    @echo "MariaDB stating on 127.0.0.1:3306. Жди ~5s, потом:"
    @echo '  export DATABASE_URL="mysql://lakko:devpass@127.0.0.1:3306/lakko"'
    @echo "  just dev-auth"

# Остановить локальный dev-MariaDB.
dev-mariadb-stop:
    @command -v docker >/dev/null 2>&1 && CONTAINER=docker || CONTAINER=podman; \
    $CONTAINER stop lakko-mariadb-dev || true

# Запустить auth service локально (cargo run, не сборка binary).
# Требует DATABASE_URL — либо из shell env, либо подхватит default ниже.
dev-auth:
    DATABASE_URL="${DATABASE_URL:-mysql://lakko:devpass@127.0.0.1:3306/lakko}" \
    LAKKO_BIND="${LAKKO_BIND:-127.0.0.1:8081}" \
    RUST_LOG="${RUST_LOG:-info,sqlx=warn,lakko_svc_auth=debug}" \
    cargo run -p lakko-svc-auth

# ---- Deployment (systemd + system-nginx + system-MariaDB на Hetzner) -----
# See infra/README.md для полного migration / deploy workflow-а.

# Default deploy target — переменные среды переопределяют:
#   LAKKO_VPS_USER (default: lakkome)
#   LAKKO_VPS_HOST (default: lakko.me)
vps_user := env_var_or_default("LAKKO_VPS_USER", "lakkome")
vps_host := env_var_or_default("LAKKO_VPS_HOST", "lakko.me")

# Deploy: ssh на VPS и запустить infra/scripts/deploy.sh.
# Скрипт делает git pull + cargo build --release + install в /usr/local/bin
# + systemctl restart lakko.target.
deploy:
    ssh {{vps_user}}@{{vps_host}} '~/lakko/infra/scripts/deploy.sh'

# Restart всех Lakko services через systemd target.
restart:
    ssh {{vps_user}}@{{vps_host}} 'sudo systemctl restart lakko.target'

# Restart одного service. Example: just restart-svc auth
restart-svc service:
    ssh {{vps_user}}@{{vps_host}} 'sudo systemctl restart lakko-svc-{{service}}'

# Tail логов одного service. Example: just logs auth
logs service:
    ssh {{vps_user}}@{{vps_host}} 'journalctl -u lakko-svc-{{service}} -f'

# Status всего.
status:
    ssh {{vps_user}}@{{vps_host}} 'sudo systemctl status lakko.target --no-pager'
