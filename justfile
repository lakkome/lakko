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

# Build `lakko-ffi-wasm` for `wasm32-unknown-unknown`. CI runs this
# too; keep it locally for fast feedback when changing the
# wasm-bindgen surface or any dep in the wasm graph.
# Requires `rustup target add wasm32-unknown-unknown` once.
wasm-build:
    cargo build -p lakko-ffi-wasm --target wasm32-unknown-unknown

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

# ---- Local dev (on your machine) ------------------------------------------

# Bring up an ephemeral MariaDB via docker/podman for local dev. Stop it
# with `just dev-mariadb-stop`. Data is not persisted across stops.
# Requires `docker` OR `podman` on PATH.
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
    @echo "MariaDB starting on 127.0.0.1:3306. Wait ~5s, then:"
    @echo '  export DATABASE_URL="mysql://lakko:devpass@127.0.0.1:3306/lakko"'
    @echo "  just dev-auth"

# Stop the local dev-MariaDB container.
dev-mariadb-stop:
    @command -v docker >/dev/null 2>&1 && CONTAINER=docker || CONTAINER=podman; \
    $CONTAINER stop lakko-mariadb-dev || true

# Run auth service locally via `cargo run` (not the release binary).
# Env vars come from `.env` in the workspace root via dotenvy — copy
# `.env.example` → `.env` before the first run.
dev-auth:
    cargo run -p lakko-svc-auth

# Run the Delivery Service locally. Reads `LAKKO_DS_DATABASE_URL` from
# `.env` (separate database `lakko_ds` — see `.env.example`).
dev-ds:
    cargo run -p lakko-svc-ds

# Run the storage service locally. Filesystem-backed; default blob root
# is `./target/dev-blobs` (override via `LAKKO_BLOB_DIR`).
dev-storage:
    cargo run -p lakko-svc-storage

# Run the discovery service locally. Reads
# `LAKKO_DISCOVERY_DATABASE_URL` from `.env` (separate database
# `lakko_discovery` — see `.env.example`).
dev-discovery:
    cargo run -p lakko-svc-discovery

# Run the push gateway locally. Reads `LAKKO_PUSH_GW_DATABASE_URL`
# from `.env` (separate database `lakko_push_gw`). Phase 1.6 ships
# only `LogOnlyProvider` — no real APNs / FCM / VAPID delivery yet.
dev-push-gw:
    cargo run -p lakko-svc-push-gw

# Run the web client (apps/web) dev server. Drives `wasm-pack` in
# `--dev` mode against `crates/lakko-ffi-wasm` first, then starts
# Vite on http://127.0.0.1:5173.
# Prerequisites (one-time):
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-pack
#   cd apps/web && npm install
dev-web:
    cd apps/web && npm run dev

# Production build of apps/web — wasm-pack release + tsc --noEmit +
# `vite build`. Output lands in `apps/web/dist/`.
build-web:
    cd apps/web && npm run build

# Phase 1.16f3 — wasm-bindgen-test smoke suite under Node. Tests
# in `crates/lakko-ffi-wasm/src/wasm_tests.rs` exercise WASM-shape
# regressions the host `cargo test` cannot reach: getrandom under
# wasm32, JsError surface, the snapshot/restore round-trip via
# the actual WASM bridge. No browser driver required — V8 + the
# wasm-pack-test Node runner are enough.
#
# For browser-only paths (IndexedDB structured-clone, the SW
# offline cold-start) swap `--node` for `--headless --chrome` /
# `--firefox` / `--safari` once a driver is on PATH.
test-wasm:
    wasm-pack test --node crates/lakko-ffi-wasm

# Phase 1.16f1 — full production build + per-file gzip / brotli
# size table for `apps/web/dist/`. Useful before / after a
# dependency bump or feature switch to catch bundle drift.
bundle-report:
    cd apps/web && npm run report

# Same report against an existing `dist/` (skip the rebuild).
# Cheap iteration when you're tweaking the report script itself.
bundle-report-only:
    cd apps/web && npm run report:no-build

# ---- Deployment (systemd + system-nginx + system-MariaDB on Hetzner) -----
# See infra/README.md for the full migration / deploy workflow.

# Deploy target — override via env vars:
#   LAKKO_VPS_USER (default: lakkome)
#   LAKKO_VPS_HOST (default: lakko.me)
vps_user := env_var_or_default("LAKKO_VPS_USER", "lakkome")
vps_host := env_var_or_default("LAKKO_VPS_HOST", "lakko.me")

# Deploy: ssh into the VPS and run infra/scripts/deploy.sh.
# That script does `git pull` + `cargo build --release` + install into
# /usr/local/bin + `systemctl restart lakko.target`.
deploy:
    ssh {{vps_user}}@{{vps_host}} '~/lakko/infra/scripts/deploy.sh'

# Phase 1.17c — apps/web deploy fallback for the dev box.
#
# **Primary path is the GitHub Actions workflow** (manual
# dispatch under Actions → "Deploy apps/web"). The CI runner
# builds in a controlled env, ssh-rsyncs to /var/www/lakko-web/,
# reloads nginx. This recipe is the **fallback** for when CI is
# down, you're on a flight, or you want to ship an unmerged
# branch fast.
#
# Both paths converge on the same VPS-side contract:
#
#   rsync local-dist/ → /tmp/lakko-web-stage/    (over ssh)
#   sudo rsync /tmp/staging/ → /var/www/lakko-web/   (atomic)
#   sudo systemctl reload nginx
#
# Pre-reqs:
#   * Local: Node 20 + wasm-pack ^0.13 in $PATH (same as CI).
#   * VPS: sudoers entry for the rsync + reload pair (see
#     infra/README.md::Web deploy bootstrap).
#   * VPS: /var/www/lakko-web/ exists.
#   * Your ssh agent has the key matching lakkome's
#     authorized_keys (the same ssh setup as `just deploy`).
deploy-web:
    cd apps/web && npm ci && npm run build
    # Sanity: post-build SRI/CSP pass + build-sw.mjs must have
    # touched the artefacts. Refuse to push a half-baked tree.
    grep -q 'integrity="sha384-' apps/web/dist/index.html
    ! grep -q '__BUILD_VERSION__\|__PRECACHE_MANIFEST__' apps/web/dist/sw.js
    rsync -a --delete apps/web/dist/ {{vps_user}}@{{vps_host}}:/tmp/lakko-web-stage/
    ssh {{vps_user}}@{{vps_host}} 'set -euo pipefail; \
        sudo /usr/bin/rsync -a --delete /tmp/lakko-web-stage/ /var/www/lakko-web/; \
        sudo /usr/bin/systemctl reload nginx'
    # Smoke probe — cache-busting query string forces a hit on
    # the freshly-deployed origin.
    curl -fsS --max-time 10 "https://{{vps_host}}/health?cb=$(date +%s)"

# Restart all Lakko services via the systemd target.
restart:
    ssh {{vps_user}}@{{vps_host}} 'sudo systemctl restart lakko.target'

# Restart a single service. Example: just restart-svc auth
restart-svc service:
    ssh {{vps_user}}@{{vps_host}} 'sudo systemctl restart lakko-svc-{{service}}'

# Tail a single service's logs. Example: just logs auth
logs service:
    ssh {{vps_user}}@{{vps_host}} 'journalctl -u lakko-svc-{{service}} -f'

# Status of the whole stack.
status:
    ssh {{vps_user}}@{{vps_host}} 'sudo systemctl status lakko.target --no-pager'
