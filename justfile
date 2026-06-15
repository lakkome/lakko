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
    cargo check --manifest-path crates/lakko-contacts/fuzz/Cargo.toml
    cargo check --manifest-path crates/lakko-channels/fuzz/Cargo.toml
    cargo check --manifest-path crates/lakko-transport/fuzz/Cargo.toml
    cargo check --manifest-path crates/lakko-tuf/fuzz/Cargo.toml
    cargo check --manifest-path crates/lakko-transparency/fuzz/Cargo.toml
    cargo check --manifest-path crates/lakko-call-jwt/fuzz/Cargo.toml
    cargo check --manifest-path crates/lakko-backup/fuzz/Cargo.toml
    cargo check --manifest-path crates/lakko-attest/fuzz/Cargo.toml
    cargo check --manifest-path services/sfu/fuzz/Cargo.toml

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
#   just fuzz crates/lakko-mls/fuzz mls_message
#   just fuzz services/sfu/fuzz       client_frame_decode
fuzz fuzz_dir target:
    cargo +nightly fuzz run --fuzz-dir {{fuzz_dir}} {{target}} -- -max_total_time=300

# Run every fuzz target for a bounded budget — what the
# nightly `fuzz-quick` workflow does in CI, but locally.
# Use this before pushing a parser-touching change.
# Optional filter: `just fuzz-quick lakko-tuf` runs only
# the lakko-tuf crate; `just fuzz-quick "" 30` runs every
# target for 30 s each.
fuzz-quick crate='' duration='60':
    ./tools/local-fuzz-run "{{crate}}" "{{duration}}"

# List all available fuzz targets across the 9 fuzz crates.
fuzz-list:
    @echo "lakko-mls:"
    @ls crates/lakko-mls/fuzz/fuzz_targets/ | sed 's/\.rs$//' | sed 's/^/  /'
    @echo "lakko-proto:"
    @ls crates/lakko-proto/fuzz/fuzz_targets/ | sed 's/\.rs$//' | sed 's/^/  /'
    @echo "lakko-contacts:"
    @ls crates/lakko-contacts/fuzz/fuzz_targets/ | sed 's/\.rs$//' | sed 's/^/  /'
    @echo "lakko-channels:"
    @ls crates/lakko-channels/fuzz/fuzz_targets/ | sed 's/\.rs$//' | sed 's/^/  /'
    @echo "lakko-transport:"
    @ls crates/lakko-transport/fuzz/fuzz_targets/ | sed 's/\.rs$//' | sed 's/^/  /'
    @echo "lakko-tuf:"
    @ls crates/lakko-tuf/fuzz/fuzz_targets/ | sed 's/\.rs$//' | sed 's/^/  /'
    @echo "lakko-transparency:"
    @ls crates/lakko-transparency/fuzz/fuzz_targets/ | sed 's/\.rs$//' | sed 's/^/  /'
    @echo "lakko-call-jwt:"
    @ls crates/lakko-call-jwt/fuzz/fuzz_targets/ | sed 's/\.rs$//' | sed 's/^/  /'
    @echo "lakko-backup:"
    @ls crates/lakko-backup/fuzz/fuzz_targets/ | sed 's/\.rs$//' | sed 's/^/  /'
    @echo "lakko-attest:"
    @ls crates/lakko-attest/fuzz/fuzz_targets/ | sed 's/\.rs$//' | sed 's/^/  /'
    @echo "lakko-svc-sfu:"
    @ls services/sfu/fuzz/fuzz_targets/ | sed 's/\.rs$//' | sed 's/^/  /'

# Audit-pack drift gate — every numbered invariant in
# docs/security/{calls,transport,mls-state-machine}.md
# must have a matching row in invariant-index.md, and
# vice versa.
audit-index-check:
    ./tools/audit-index-check

# Host-side validation of the SFrame native shim — runs the
# `apps/shared/sframe-bridge/build.sh` round-trip suite on the
# dev box. Builds the lakko-sframe-native staticlib + compiles
# the N-3 bridge + test harness, then exercises eight assertions:
# single-sender round-trip, AAD-binding, multi-sender cache,
# bad-seed rejection, ciphertext-body tamper, counter monotonicity
# with out-of-order delivery, per-call-key isolation, payload-
# size extremes (1 B / 80 B / 1500 B / 64 KiB). Closes the host-
# side half of the N-{4,5}-validate sub-sprint; the full dual-
# simulator / dual-emulator E2E lives in G3-4-mobile.
test-sframe-bridge:
    apps/shared/sframe-bridge/build.sh

# Everything CI does — run locally before pushing.
ci: fmt-check lint test audit reuse fuzz-check audit-index-check

# Build for all release targets (host platform only here; cross-builds via Nix).
build:
    cargo build --workspace --release

# ---- target/ housekeeping -------------------------------------------------
#
# `target/` accumulates incremental + cross-compile artifacts indefinitely.
# A workspace this size easily reaches 50+ GB after a few weeks of active
# work (debug + wasm + release + Android cross-targets + clippy + rustdoc all
# keep separate caches). The targets below clean specific subdirs so you
# don't have to remember the layout.
#
# After a full clean the next `cargo test --workspace` rebuilds ~10–15 GB
# (only what's actually exercised) instead of the 50+ GB the accumulated
# cache had grown to.
#
# Each target prints the size before + after so you see what was reclaimed.

# Nuke EVERYTHING under target/ (the heavy hammer).
#
# Tries `cargo clean` first (which walks the tree path-by-path and is
# friendlier to a running IDE — only deletes directories cargo itself
# owns). When that fails with "Directory not empty" — typically because
# RustRover / rust-analyzer holds inodes in target/debug/incremental —
# falls back to `rm -rf` on the whole tree, which is one atomic kernel
# unlink; open file handles in the IDE become orphaned (kernel keeps the
# pages until the IDE closes them) but the directory entry is gone.
clean-target:
    @echo "Before: $(du -sh target 2>/dev/null | cut -f1 || echo 'no target/')"
    @cargo clean 2>/dev/null || { \
        echo "cargo clean blocked (likely RustRover holds inodes) — falling back to rm -rf target/"; \
        rm -rf target; \
    }
    @echo "After: $(du -sh target 2>/dev/null | cut -f1 || echo 'no target/')"

# Clean only the wasm32 target tree — the second-largest cache after
# `debug/` and not exercised by `cargo test`. Run when you haven't touched
# the web client for a while.
clean-wasm:
    @echo "Before: $(du -sh target/wasm32-unknown-unknown 2>/dev/null | cut -f1 || echo 'absent')"
    rm -rf target/wasm32-unknown-unknown
    @echo "wasm32-unknown-unknown removed."

# Clean Android cross-compile targets. Only generated by a real Android
# build (Gradle or `cargo build --target aarch64-linux-android`), so this
# is safe to drop on a Mac that only runs the web + service backend.
clean-android:
    @echo "Before:"
    @du -sh target/aarch64-linux-android 2>/dev/null || true
    @du -sh target/x86_64-linux-android 2>/dev/null || true
    rm -rf target/aarch64-linux-android target/x86_64-linux-android
    @echo "Android cross-targets removed."

# Clean only the release/ tree. Useful when you just did `just regen-bindings`
# (which forces a release build of `uniffi-bindgen`) and don't plan to
# regenerate bindings again soon.
clean-release:
    @echo "Before: $(du -sh target/release 2>/dev/null | cut -f1 || echo 'absent')"
    rm -rf target/release
    @echo "release/ removed."

# Clean clippy + doc caches — they each keep separate incremental trees
# from the main debug build. Safe to drop; next `just lint` / `cargo doc`
# rebuilds.
clean-aux:
    @echo "Before:"
    @du -sh target/clippy 2>/dev/null || true
    @du -sh target/doc 2>/dev/null || true
    rm -rf target/clippy target/doc
    @echo "clippy/ + doc/ removed."

# Show the target/ subdir breakdown so you can decide which `clean-*` to
# run. Mirrors what the macOS Storage panel shows.
target-size:
    @echo "Total: $(du -sh target 2>/dev/null | cut -f1 || echo 'no target/')"
    @echo "Top-level breakdown:"
    @du -sh target/* 2>/dev/null | sort -h -r | head -10 || echo "  (target/ is empty)"

# ---- Local dev (on your machine) ------------------------------------------

# Bring up the WHOLE backend stack (MariaDB + auth + ds + storage +
# discovery + push-gw + channels) via Docker Compose. One command, every
# service a client needs for the core flow. See infra/docker/README.md.
stack-up:
    docker compose -f infra/docker/docker-compose.yml up --build -d
    @echo ""
    @echo "Stack up. Services on 127.0.0.1: auth:8081 ds:8082 storage:8083"
    @echo "  discovery:8084 push-gw:8085 channels:8086 (db:3306)."
    @echo "  Logs: just stack-logs   Down: just stack-down"

# Tear the full stack down (keeps the DB + blob volumes).
stack-down:
    docker compose -f infra/docker/docker-compose.yml down

# Tear the full stack down AND wipe the DB + blob volumes.
stack-down-clean:
    docker compose -f infra/docker/docker-compose.yml down -v

# Follow logs from every service in the full stack.
stack-logs:
    docker compose -f infra/docker/docker-compose.yml logs -f

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

# Run the channels service locally. Reads
# `LAKKO_CHANNELS_DATABASE_URL` from `.env`; if unset the service
# falls back to the in-memory store (state is lost on restart —
# fine for dev / tests, not for the VPS).
dev-channels:
    cargo run -p lakko-svc-channels

# Run the Rust SFU locally (ADR-0011). Requires LAKKO_SFU_JWKS_URL
# in .env (point it at a running dev auth service's JWKS, or the
# fixture JWKS from ci/sfu-e2e/). Set LAKKO_SFU_MEDIA_BIND to a
# local UDP port (e.g. 0.0.0.0:43210) to exercise media; leave it
# unset for a signaling-only smoke.
dev-sfu:
    cargo run -p lakko-svc-sfu

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

# G3-sframe-native N-5 — cross-compile lakko-sframe-native for
# the Android NDK ABIs the lakko-sframe AAR ships, then build
# the JNI bridge .so by compiling apps/shared/sframe-bridge +
# apps/android/lakko-sframe/src/main/cpp against the cross-
# compiled Rust archive and the M125 vendored libwebrtc
# headers.
#
# Output layout:
#   apps/android/lakko-sframe/src/main/jniLibs/arm64-v8a/libsframe_jni.so
#   apps/android/lakko-sframe/src/main/jniLibs/x86_64/libsframe_jni.so
#
# Each .so statically links the cargo-built lakko_sframe_native
# archive for that ABI, plus the C++ N-3 bridge sources, into
# one self-contained shared object the AAR ships.
#
# Pre-reqs (same as build-android):
#   * $ANDROID_NDK_HOME pointing at NDK r26+
#   * cargo-ndk (`cargo install cargo-ndk`)
#   * rustup targets aarch64-linux-android + x86_64-linux-android
cross-build-sframe-native-android:
    @test -n "${ANDROID_NDK_HOME:-}" || (echo "ERROR: ANDROID_NDK_HOME unset — see apps/android/README.md" && exit 1)
    @rustup target list --installed | grep -q "aarch64-linux-android" || rustup target add aarch64-linux-android
    @rustup target list --installed | grep -q "x86_64-linux-android" || rustup target add x86_64-linux-android
    # 1. Cross-compile the lakko-sframe-native staticlib for
    #    both ABIs via cargo-ndk. The output goes under
    #    target/{aarch64-linux-android,x86_64-linux-android}/
    #    release/liblakko_sframe_native.a (cargo-ndk just sets
    #    CC + AR + linker env vars for the NDK toolchain and
    #    delegates to plain cargo).
    cargo ndk \
        --target arm64-v8a \
        --target x86_64 \
        build --release -p lakko-sframe-native
    # 2. Build the JNI bridge .so for each ABI by hand. AGP's
    #    `externalNativeBuild { cmake }` path would also work
    #    but has a brittle ordering with cargo-ndk's output;
    #    plain $(NDK_TOOLCHAIN)/bin/clang++ is simpler + the
    #    surface is small (two .cc files, one .a archive).
    tools/build-sframe-jni.sh

# G3-sframe-native N-5 — full Android build of both jniLibs
# layers (lakko-ffi-uniffi via build-android + the SFrame
# bridge via cross-build-sframe-native-android). Run this
# before any Gradle build of the app that depends on
# :lakko-sframe.
build-android-all: build-android cross-build-sframe-native-android

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
# G3-sframe-native N-4 — cross-compile the lakko-sframe-native
# Rust staticlib for every iOS slice the Xcode build needs to
# link, then lay the archives out under target/ios-link/ keyed
# by Xcode's $(PLATFORM_NAME) substitution. Run this before
# `just build-ios` whenever the lakko-sframe-native crate (or
# any of its transitive Rust deps) changes.
#
# Three slices:
#   - aarch64-apple-ios       — physical device builds
#   - aarch64-apple-ios-sim   — Apple Silicon Mac simulators
#   - x86_64-apple-ios        — Intel Mac simulators
#
# The two simulator slices are `lipo`-fused into one fat
# archive (libfoo.a in iphonesimulator/) — Xcode's linker picks
# the matching arch out of the fat archive automatically.
cross-build-sframe-native-ios:
    @command -v lipo >/dev/null 2>&1 || (echo "ERROR: lipo not found — install Xcode Command Line Tools via 'xcode-select --install'" && exit 1)
    @rustup target list --installed | grep -q "aarch64-apple-ios" || rustup target add aarch64-apple-ios
    @rustup target list --installed | grep -q "aarch64-apple-ios-sim" || rustup target add aarch64-apple-ios-sim
    @rustup target list --installed | grep -q "x86_64-apple-ios" || rustup target add x86_64-apple-ios
    cargo build -p lakko-sframe-native --release --target aarch64-apple-ios
    cargo build -p lakko-sframe-native --release --target aarch64-apple-ios-sim
    cargo build -p lakko-sframe-native --release --target x86_64-apple-ios
    mkdir -p target/ios-link/iphoneos target/ios-link/iphonesimulator
    cp target/aarch64-apple-ios/release/liblakko_sframe_native.a \
       target/ios-link/iphoneos/liblakko_sframe_native.a
    lipo -create \
        target/aarch64-apple-ios-sim/release/liblakko_sframe_native.a \
        target/x86_64-apple-ios/release/liblakko_sframe_native.a \
        -output target/ios-link/iphonesimulator/liblakko_sframe_native.a
    @echo "OK: target/ios-link/{iphoneos,iphonesimulator}/liblakko_sframe_native.a"

[macos]
build-ios:
    @command -v xcodebuild >/dev/null 2>&1 || (echo "ERROR: xcodebuild not found — install Xcode Command Line Tools via 'xcode-select --install'" && exit 1)
    cargo build --release --target aarch64-apple-ios -p lakko-ffi-uniffi
    cargo build --release --target aarch64-apple-ios-sim -p lakko-ffi-uniffi
    cargo build --release --target x86_64-apple-ios -p lakko-ffi-uniffi
    # Combine the two simulator slices into one fat archive
    # — `xcodebuild -create-xcframework` accepts exactly one
    # binary per `-library` argument, and Mac simulators come
    # in both arm64 + x86_64 flavours. (Note: the rustup
    # triple for the Intel simulator is `x86_64-apple-ios`,
    # NOT `x86_64-apple-ios-sim` — the `-sim` suffix only
    # exists for the aarch64 variant.)
    mkdir -p target/ios
    lipo -create \
        target/aarch64-apple-ios-sim/release/liblakko_ffi_uniffi.a \
        target/x86_64-apple-ios/release/liblakko_ffi_uniffi.a \
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

# Phase 1.14c — regenerate `apps/ios/Lakko.xcodeproj/` from the
# xcodegen spec at `apps/ios/project.yml`.
#
# The `.xcodeproj` is gitignored as a build artefact (Xcode's
# project format is a plist-like binary blob with per-developer
# absolute-path drift — reviewing diffs on it is a permanent
# papercut). `project.yml` is the reviewable source of truth;
# this recipe regenerates the binary blob from it.
#
# Pre-reqs:
#   * `brew install xcodegen` (or `mint install yonaskolb/XcodeGen`).
#   * `just build-ios` must have run at least once so the
#     `target/ios/LakkoCore.xcframework/` exists at the path
#     `apps/ios/Package.swift`'s `LakkoCoreFFI.binaryTarget`
#     resolves to — xcodegen will error otherwise on the SPM
#     dependency resolution step.
#
# Pre-flight `command -v xcodegen` check fails fast with a
# pointer to the install command if the dependency is missing.
[macos]
generate-ios-project:
    @command -v xcodegen >/dev/null 2>&1 || (echo "ERROR: xcodegen not found — install via 'brew install xcodegen'" && exit 1)
    cd apps/ios && xcodegen generate --spec project.yml

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

# G3-turn-deploy — separate VPS for coturn. Defaults match the
# `turn.lakko.me` DNS name pinned in `infra/coturn/turnserver.conf`
# + `infra/prometheus/prometheus.yml`. Override via env vars if
# the operator's fleet layout differs.
turn_user := env_var_or_default("LAKKO_TURN_USER", "root")
turn_host := env_var_or_default("LAKKO_TURN_HOST", "turn.lakko.me")

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

# One-time SFU setup on the MAIN VPS (ADR-0011 — Rust SFU,
# co-located). Issues the sfu.lakko.me cert, installs the nginx
# vhost, and opens the public UDP media port in the firewall. The
# BINARY itself rides the regular `just deploy` (deploy.sh now
# builds + installs lakko-svc-sfu, and lakko.target Wants it).
# Run this once after pointing DNS for sfu.lakko.me at the VPS and
# adding the LAKKO_SFU_* vars to /etc/lakko/lakko.env.
deploy-sfu:
    ssh {{vps_user}}@{{vps_host}} 'sudo bash ~/lakko/infra/scripts/deploy-sfu.sh'

# Restart only the SFU (drops live calls — there is no graceful
# drain yet). The systemd unit is `lakko-sfu`, not `lakko-svc-sfu`,
# so this can't go through `restart-svc`.
restart-sfu:
    ssh {{vps_user}}@{{vps_host}} 'sudo systemctl restart lakko-sfu'

# Tail SFU logs (journald — no separate file).
logs-sfu:
    ssh {{vps_user}}@{{vps_host}} 'journalctl -u lakko-sfu -f'

# G3-turn-deploy — bootstrap or refresh the coturn VPS.
# Idempotent — safe to re-run after editing turnserver.conf or
# the systemd override; the script reloads coturn at the end.
#
# Prereqs:
#   1. DNS A + AAAA records for turn.lakko.me → VPS IP.
#   2. Root SSH access to the VPS (`LAKKO_TURN_USER=root` by
#      default — Hetzner Ubuntu images ship root-only).
#   3. The maintainer's monitoring CA cert (`monitoring-ca.crt`)
#      pre-staged at `/etc/lakko/mtls/` on the VPS (see
#      `infra/README-calls.md` "First-time deploy" step 5).
#
# First-run also generates a fresh TURN_SHARED_SECRET — the
# script prints it; paste the same value into
# /etc/lakko/lakko.env on the auth VPS as
# LAKKO_TURN_SHARED_SECRET=... so services/auth can mint
# matching credentials.
deploy-coturn:
    @echo "[deploy-coturn] rsyncing repo to {{turn_host}} (root may prompt for sudo)"
    rsync -a --delete --exclude='target/' --exclude='.git/' \
        ./infra/ {{turn_user}}@{{turn_host}}:/root/lakko-infra/
    ssh {{turn_user}}@{{turn_host}} 'bash /root/lakko-infra/scripts/deploy-coturn.sh'

# Restart coturn (drops in-flight relay sessions — operators
# prefer `reload-coturn` below in 99 % of cases). Use this only
# when a config change requires a full re-init (e.g. swapping
# the listening IPs).
restart-coturn:
    ssh {{turn_user}}@{{turn_host}} 'sudo systemctl restart coturn'

# Reload coturn — SIGUSR2-equivalent. Re-reads turnserver.conf
# + the TLS cert files WITHOUT dropping live relay sessions.
# This is what the Let's Encrypt deploy-hook
# (`infra/letsencrypt/lakko-coturn-renew-hook.sh`) does on every
# renewal, and what the operator should use after editing
# `turnserver.conf` for everything except listening-port changes.
reload-coturn:
    ssh {{turn_user}}@{{turn_host}} 'sudo systemctl reload coturn'

# Tail coturn logs. Same posture as `just logs <service>` for the
# main fleet — `journalctl -u coturn -f` because the systemd
# override unit logs through the journal (no separate file).
logs-coturn:
    ssh {{turn_user}}@{{turn_host}} 'journalctl -u coturn -f'

# Local smoke probe — runs from the dev machine against the
# public TURN endpoint. Does NOT require ssh; verifies the
# TURN-over-TLS handshake + reachability of the metrics port.
# Optional first arg overrides the host (default turn_host).
turn-smoke host=turn_host:
    bash infra/scripts/turn-smoke.sh {{host}}

# Phase 1.22 — post-deploy end-to-end smoke probe. Runs from the
# dev machine against the public HTTPS surface; no ssh, no shell
# on the VPS. Covers:
#
#   * nginx layer (/health on both vhosts)
#   * each backend service via api.lakko.me/{auth,ds,storage,discovery,push-gw}
#   * same-origin /api/* proxies on lakko.me
#   * /version JSON on each service (confirms the right binary is
#     bound to the right port — a deploy regression check)
#
# Override the bases with positional args when probing a staging
# tunnel instead of the canonical lakko.me pair:
#   just smoke https://staging.lakko.me https://api-staging.lakko.me
smoke web="https://lakko.me" api="https://api.lakko.me":
    ./infra/scripts/staging-smoke.sh {{web}} {{api}}
