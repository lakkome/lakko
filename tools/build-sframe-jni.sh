#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Build libsframe_jni.so per Android ABI for the lakko-sframe
# AAR (G3-sframe-native N-5).
#
# Called by the `cross-build-sframe-native-android` justfile
# recipe AFTER cargo-ndk has produced the per-ABI
# liblakko_sframe_native.a archives under target/.
#
# What this script does, per ABI:
#   1. Pick the right NDK toolchain (clang++ + sysroot + target
#      triple).
#   2. Compile apps/shared/sframe-bridge/sframe-bridge.cc +
#      apps/android/lakko-sframe/src/main/cpp/sframe_jni.cc.
#   3. Link against the cross-compiled
#      target/<triple>/release/liblakko_sframe_native.a.
#   4. Emit a single libsframe_jni.so under
#      apps/android/lakko-sframe/src/main/jniLibs/<abi>/.
#
# Why a hand-rolled shell script:
#
# AGP's externalNativeBuild { cmake } would also work, but it
# expects CMake to drive the build with cargo as a dep —
# whereas here cargo's output (the .a archive) is a prerequisite
# of the CMake compile/link pair. Inverting that requires either
# a brittle add_custom_command + IMPORTED target dance, or a
# top-level Make/Ninja layer above both. The plain shell path
# is ~50 LOC, has no project-side magic, and the next NDK bump
# touches one variable assignment.
#
# Pre-reqs:
#   * $ANDROID_NDK_HOME pointing at NDK r26+ (CLI tools that
#     ship the standalone toolchain layout under prebuilt/).
#   * The `cross-build-sframe-native-android` justfile recipe
#     has run cargo-ndk first so the .a archives exist.

set -euo pipefail

NDK="${ANDROID_NDK_HOME:?ANDROID_NDK_HOME unset}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# NDK r26+ ships a single prebuilt toolchain per host arch.
# Probe at runtime so the script works on both Apple Silicon
# (darwin-arm64) + Intel (darwin-x86_64) macs and Linux x86_64
# CI runners. The directory layout under prebuilt/ is the
# only one upstream supports.
case "$(uname -s)-$(uname -m)" in
    Darwin-arm64)  HOST_TAG="darwin-arm64"  ;;
    Darwin-x86_64) HOST_TAG="darwin-x86_64" ;;
    Linux-x86_64)  HOST_TAG="linux-x86_64"  ;;
    *)
        echo "ERROR: unsupported NDK host platform $(uname -s)-$(uname -m)" >&2
        exit 1
        ;;
esac

# `prebuilt/<host>-<arch>` was deprecated in r24+ — modern NDKs
# only ship `prebuilt/<host>-<arch>` for `darwin-x86_64` on
# Intel Macs; Apple Silicon falls back to running x86_64 under
# Rosetta translation if the arm64 build is absent. Check both
# and prefer the arch-native build.
TOOLCHAIN_DIR="$NDK/toolchains/llvm/prebuilt/$HOST_TAG"
if [[ ! -d "$TOOLCHAIN_DIR" ]]; then
    # Apple Silicon fallback — Rosetta-translatable Intel build.
    if [[ "$HOST_TAG" == "darwin-arm64" ]]; then
        TOOLCHAIN_DIR="$NDK/toolchains/llvm/prebuilt/darwin-x86_64"
    fi
fi
if [[ ! -d "$TOOLCHAIN_DIR" ]]; then
    echo "ERROR: NDK toolchain not found under $NDK/toolchains/llvm/prebuilt/" >&2
    exit 1
fi
CXX="$TOOLCHAIN_DIR/bin/clang++"
SYSROOT="$TOOLCHAIN_DIR/sysroot"

# `API_LEVEL` matches the lakko-core AAR's minSdk (libs.versions
# .toml :: android.min.sdk). 24 — same baseline as the rest of
# Lakko's Android target floor.
API_LEVEL=24

# Per-ABI configuration. Each entry is `abi:triple` where
# `abi` is what Gradle expects under jniLibs/ and `triple` is
# the Rust-target / NDK clang prefix.
ABIS=(
    "arm64-v8a:aarch64-linux-android"
    "x86_64:x86_64-linux-android"
)

# Header search paths shared across ABIs:
#   - apps/shared/webrtc-cpp-headers/M125/include   (vendored M125
#                                                    libwebrtc
#                                                    headers; N-2)
#   - apps/shared/sframe-bridge                     (N-3 bridge .h)
#   - target/lakko-include                          (cbindgen
#                                                    mirror,
#                                                    written by
#                                                    crates/lakko-
#                                                    sframe-native
#                                                    /build.rs)
COMMON_INCLUDES=(
    -I "$ROOT/apps/shared/webrtc-cpp-headers/M125/include"
    -I "$ROOT/apps/shared/sframe-bridge"
    -I "$ROOT/target/lakko-include"
)

# Source files compiled into each .so:
#   - sframe-bridge.cc — webrtc::FrameEncryptor / Decryptor
#                       subclass (N-3)
#   - sframe_jni.cc    — JNI trampoline (this sub-sprint)
SRC=(
    "$ROOT/apps/shared/sframe-bridge/sframe-bridge.cc"
    "$ROOT/apps/android/lakko-sframe/src/main/cpp/sframe_jni.cc"
)

OUT_BASE="$ROOT/apps/android/lakko-sframe/src/main/jniLibs"
mkdir -p "$OUT_BASE"

for entry in "${ABIS[@]}"; do
    abi="${entry%%:*}"
    triple="${entry##*:}"

    archive="$ROOT/target/$triple/release/liblakko_sframe_native.a"
    if [[ ! -f "$archive" ]]; then
        echo "ERROR: missing $archive — run cargo-ndk first" >&2
        exit 1
    fi

    out_dir="$OUT_BASE/$abi"
    mkdir -p "$out_dir"
    out_so="$out_dir/libsframe_jni.so"

    echo "build-sframe-jni: compiling for $abi ($triple)"

    # `-fPIC` mandatory for .so output. `-std=c++17` matches
    # N-3's bridge code (`std::lock_guard`, `[[nodiscard]]`).
    # `-Os` over `-O2` — JNI bridge code is tiny + we want code
    # size over micro-perf. The hot path goes straight to the
    # Rust .a which is already `--release`.
    # `-shared` produces .so.
    # `-Wl,--exclude-libs,ALL` hides the Rust archive symbols
    # from the .so's dynamic symbol table so we don't pollute
    # the AAR consumer's namespace with internal Rust symbols.
    "$CXX" \
        --target="${triple}${API_LEVEL}" \
        --sysroot="$SYSROOT" \
        -fPIC \
        -std=c++17 \
        -Os \
        -Wall -Wextra -Wno-unused-parameter \
        -shared \
        "${COMMON_INCLUDES[@]}" \
        "${SRC[@]}" \
        "$archive" \
        -Wl,--exclude-libs,ALL \
        -llog \
        -o "$out_so"

    echo "build-sframe-jni: wrote $out_so"
done

echo "build-sframe-jni: done"
