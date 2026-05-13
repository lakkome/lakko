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
#
# Phase 1.15a: drives the **crate-local** `uniffi-bindgen` bin
# (in `crates/lakko-ffi-uniffi/src/bin/uniffi-bindgen.rs`) instead
# of a floating `cargo install uniffi-bindgen`. The bin compiles
# against the same workspace `Cargo.lock` as the cdylib, so the
# bindgen and the runtime can never drift across a UniFFI minor
# release.
#
# `os()` resolves to `linux` / `macos` / `windows`; we pick the
# matching dynamic-library extension so the recipe runs on any
# of the three host targets.
lib_ext := if os() == "macos" { "dylib" } else if os() == "windows" { "dll" } else { "so" }

regen-bindings:
    cargo build -p lakko-ffi-uniffi --release
    cargo run -p lakko-ffi-uniffi --bin uniffi-bindgen --release -- \
        generate \
        --library target/release/liblakko_ffi_uniffi.{{lib_ext}} \
        --language swift \
        --out-dir target/bindings/swift
    cargo run -p lakko-ffi-uniffi --bin uniffi-bindgen --release -- \
        generate \
        --library target/release/liblakko_ffi_uniffi.{{lib_ext}} \
        --language kotlin \
        --out-dir target/bindings/kotlin
    # Phase 1.15b — fan the Kotlin tree out into the Gradle
    # module's source set so `:lakko-core:assembleDebug` picks
    # it up. The generator drops files under
    # `target/bindings/kotlin/uniffi/lakko_ffi_uniffi/`; mirror
    # that path under `apps/android/lakko-core/src/main/kotlin/`
    # (gitignored on the Android side — regenerated on every
    # `regen-bindings`).
    @mkdir -p apps/android/lakko-core/src/main/kotlin
    @rm -rf apps/android/lakko-core/src/main/kotlin/uniffi
    @cp -R target/bindings/kotlin/uniffi apps/android/lakko-core/src/main/kotlin/uniffi

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

# Phase 1.15b — cross-compile `lakko-ffi-uniffi` (cdylib) for
# Android NDK targets via `cargo-ndk`, drop the `.so` files into
# `apps/android/lakko-core/src/main/jniLibs/{abi}/` where the
# Gradle module's `assembleDebug` picks them up. JNA / UniFFI
# load them at runtime via `System.loadLibrary("lakko_ffi_uniffi")`.
#
# Two targets cover the entire Apple-Silicon dev loop:
#   * arm64-v8a       — modern phones + Apple-Silicon emulator
#                       (qemu-system-aarch64 with KVM-like accel)
#   * x86_64          — Intel emulator on Intel Macs
#
# 32-bit Android (armv7-linux-androideabi / i686-linux-android)
# is intentionally NOT in the matrix — current Play Store
# minimum is API 21 and the long-tail of armv7-only devices is
# below 5% by 2026. Bring back as a separate target list if a
# real distribution gate flags it.
#
# Pre-reqs:
#   * `rustup target add aarch64-linux-android x86_64-linux-android`
#   * `cargo install cargo-ndk`
#   * `$ANDROID_NDK_HOME` pointing at an NDK r26+ install
#
# `--release` keeps optimisations on by default — debug builds
# of OpenMLS + RustCrypto on Android are slow enough to matter.
# Flip to `--profile dev` only for active debugging.
build-android:
    @test -n "${ANDROID_NDK_HOME:-}" || (echo "ERROR: ANDROID_NDK_HOME unset — see apps/android/README.md" && exit 1)
    cargo ndk \
        --target arm64-v8a \
        --target x86_64 \
        --output-dir apps/android/lakko-core/src/main/jniLibs \
        build --release -p lakko-ffi-uniffi

# Phase 1.14a — cross-compile `lakko-ffi-uniffi` (staticlib) for
# the three iOS targets that cover both physical devices + the
# simulator on every Mac architecture, then bundle the per-target
# `.a` archives into a single `LakkoCore.xcframework` consumable
# by the SwiftUI app `apps/ios/` (lands in 1.14b).
#
# **Targets:**
#   * `aarch64-apple-ios`           — modern physical iPhones + iPads
#                                     (every device since A7, 2013).
#                                     Built standalone — Apple
#                                     doesn't ship 32-bit iOS any
#                                     more; armv7 / armv7s are gone.
#   * `aarch64-apple-ios-sim`       — Apple-Silicon Mac simulator.
#   * `x86_64-apple-ios-sim`        — Intel Mac simulator (still
#                                     common on Intel-era 2015-2020
#                                     hardware; cheap to keep in
#                                     the matrix until the Intel-Mac
#                                     install base is < 5%).
#
# **Pre-reqs:**
#   * macOS host (no other OS can run `xcodebuild`).
#   * `xcode-select --install` (Xcode Command Line Tools).
#     The full Xcode app is not required to BUILD the XCFramework
#     — only to ship the eventual SwiftUI app via `xcodebuild`'s
#     archive / export flows in 1.14b/c.
#   * `rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios-sim`
#
# **Output:**
#   `target/ios/LakkoCore.xcframework/` — the directory the Xcode
#   project (1.14b) imports via `Add Files…` or as a Swift Package
#   product. Gitignored under the existing `target/` rule.
#
# **modulemap shape:** UniFFI's swift bindgen emits both the
# `.swift` source and the C-ABI `*FFI.h` header. The XCFramework
# format expects a sibling `module.modulemap` to expose the C
# symbols to Swift; we generate it inline here (5-line template
# pinned at the head of the recipe). Doing it inline is simpler
# than maintaining a `tools/ios/modulemap.template` for a single
# consumer.
#
# **Why `staticlib` not `cdylib`:** iOS embeds frameworks
# differently from Android — the codesign + entitlement story
# for dynamic libraries on iOS adds friction that pays off only
# at scale (faster app updates without re-shipping the binary).
# For a single-app `.xcframework` consumed by one Xcode target,
# static linking is the canonical shape + the App Store reviewer
# friction is zero.
[macos]
build-ios:
    @command -v xcodebuild >/dev/null 2>&1 || (echo "ERROR: xcodebuild not found — install Xcode Command Line Tools via 'xcode-select --install'" && exit 1)
    cargo build --release --target aarch64-apple-ios -p lakko-ffi-uniffi
    cargo build --release --target aarch64-apple-ios-sim -p lakko-ffi-uniffi
    cargo build --release --target x86_64-apple-ios-sim -p lakko-ffi-uniffi
    # Combine the two simulator slices into one fat archive
    # — `xcodebuild -create-xcframework` accepts exactly one
    # binary per `-library` argument, and Mac simulators come
    # in both arm64 + x86_64 flavours.
    mkdir -p target/ios
    lipo -create \
        target/aarch64-apple-ios-sim/release/liblakko_ffi_uniffi.a \
        target/x86_64-apple-ios-sim/release/liblakko_ffi_uniffi.a \
        -output target/ios/liblakko_ffi_uniffi-simulator.a
    # Lay out the headers + modulemap directory the
    # `-headers` argument expects: a single dir with the C
    # header + the `module.modulemap` referencing it. The
    # `LakkoCoreFFI` framework name must match what the
    # Swift binding's `import LakkoCoreFFI` line expects (the
    # uniffi generator emits the import for us in
    # `lakko_ffi_uniffi.swift`).
    mkdir -p target/ios/headers
    @cp target/bindings/swift/lakko_ffi_uniffiFFI.h target/ios/headers/
    @printf 'module lakko_ffi_uniffiFFI {\n    header "lakko_ffi_uniffiFFI.h"\n    export *\n}\n' \
        > target/ios/headers/module.modulemap
    # `xcodebuild -create-xcframework` is the canonical
    # path — alternatives (`carton`, hand-rolled `Info.plist`)
    # all proxy through this command eventually.
    rm -rf target/ios/LakkoCore.xcframework
    xcodebuild -create-xcframework \
        -library target/aarch64-apple-ios/release/liblakko_ffi_uniffi.a \
        -headers target/ios/headers \
        -library target/ios/liblakko_ffi_uniffi-simulator.a \
        -headers target/ios/headers \
        -output target/ios/LakkoCore.xcframework
    # Phase 1.14b — fan the UniFFI-emitted Swift binding into
    # the SPM target's source dir so `apps/ios/Package.swift`
    # picks it up as part of the `LakkoCore` target's source
    # set. Mirror of `regen-bindings` copying the Kotlin tree
    # into `apps/android/lakko-core/src/main/kotlin/uniffi/`.
    # The file is gitignored on the iOS side (build artefact;
    # source of truth lives in the `#[uniffi::export]`
    # annotations on `crates/lakko-ffi-uniffi/`).
    @mkdir -p apps/ios/Sources/LakkoCore
    @cp target/bindings/swift/lakko_ffi_uniffi.swift apps/ios/Sources/LakkoCore/

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
