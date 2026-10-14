#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Build + run the round-trip test for the N-3 sframe bridge.
#
# What happens:
#   1. `cargo build -p lakko-sframe-native --release` produces
#      target/release/liblakko_sframe_native.a + the cbindgen-
#      generated header under
#      target/release/build/lakko-sframe-native-<hash>/out/.
#   2. `c++` compiles sframe-bridge.cc + test_bridge.cc together,
#      links the staticlib, and writes ./build/test_bridge.
#   3. ./build/test_bridge runs the test functions listed in
#      test_bridge.cc's `main` (round trip, the ADR-0071 golden
#      frame, the clear prefix per codec, tampering, the refusal of
#      the header-first layout, the SetCodec / Encrypt race, the
#      mid-call rekey overlap, and the rest).
#
# The build runs offline once `target/release/` is populated; the
# only network round-trip is the one cargo does on first run.
#
# Platform: macOS + Linux. The same toolchain (Apple clang on
# macOS, gcc/clang on Linux) builds the staticlib; the iOS shim
# (N-4) and Android shim (N-5) will cross-compile through
# `cargo --target` later. This script is the "is the bridge
# code correct" gate, not the platform-build gate.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace_root="$(cd "${script_dir}/../../.." && pwd)"

echo "build.sh: workspace_root = ${workspace_root}"

# --- 1. Build the Rust staticlib ------------------------------
(
    cd "${workspace_root}"
    echo "build.sh: cargo build -p lakko-sframe-native --release"
    cargo build -p lakko-sframe-native --release
)

# --- 2. Locate the cbindgen-generated header ------------------
# The header path includes a cargo-managed hash; resolve at runtime.
header_dir=$(find "${workspace_root}/target/release/build" \
    -path "*lakko-sframe-native-*/out" -type d \
    | head -1)
if [[ -z "${header_dir}" || ! -f "${header_dir}/lakko_sframe_native.h" ]]; then
    echo "build.sh: cannot locate lakko_sframe_native.h under target/release/build/" >&2
    exit 1
fi
echo "build.sh: header_dir = ${header_dir}"

# --- 3. Locate the staticlib ----------------------------------
staticlib_path="${workspace_root}/target/release/liblakko_sframe_native.a"
if [[ ! -f "${staticlib_path}" ]]; then
    echo "build.sh: liblakko_sframe_native.a not found at ${staticlib_path}" >&2
    exit 1
fi
echo "build.sh: staticlib_path = ${staticlib_path}"

# --- 4. Build the C++ test binary -----------------------------
build_dir="${script_dir}/build"
mkdir -p "${build_dir}"

headers_dir="${workspace_root}/apps/shared/webrtc-cpp-headers/M125/include"
if [[ ! -d "${headers_dir}/api/crypto" ]]; then
    echo "build.sh: webrtc-cpp-headers not populated; run apps/shared/webrtc-cpp-headers/M125/fetch.sh first" >&2
    exit 1
fi

# Platform-specific link extras: macOS staticlibs from Rust have
# no required system frameworks for this surface; Linux needs
# -lpthread + -ldl + -lm for the standard Rust runtime symbols.
extra_libs=""
case "$(uname -s)" in
    Linux*)
        extra_libs="-lpthread -ldl -lm"
        ;;
    Darwin*)
        # libSystem covers everything. Rust's release staticlib
        # on macOS doesn't need extra link flags.
        extra_libs=""
        ;;
    *)
        echo "build.sh: unknown platform $(uname -s); attempting Linux defaults" >&2
        extra_libs="-lpthread -ldl -lm"
        ;;
esac

echo "build.sh: c++ -std=c++17 -O2 -Wall -Wextra -Wno-unused-parameter ..."
c++ -std=c++17 -O2 -Wall -Wextra -Wno-unused-parameter \
    -I "${headers_dir}" \
    -I "${header_dir}" \
    -I "${script_dir}" \
    "${script_dir}/sframe-bridge.cc" \
    "${script_dir}/test_bridge.cc" \
    "${script_dir}/test_checks_stub.cc" \
    "${staticlib_path}" \
    ${extra_libs} \
    -o "${build_dir}/test_bridge"

echo "build.sh: built ${build_dir}/test_bridge"

# --- 5. Run -----------------------------------------------------
echo "build.sh: running ${build_dir}/test_bridge"
"${build_dir}/test_bridge"
