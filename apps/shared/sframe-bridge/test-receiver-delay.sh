#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Build + run the host test of receiver-delay.h (ADR-0070, note of
# 2026-10-12). No Rust archive is involved: the header depends on the
# vendored libwebrtc headers only.
#
# The test is built with optimisation on and off, and on macOS for x86_64 as
# well as the host architecture when the x86_64 slice can run (Rosetta): the
# argument's calling convention differs between the two, and the header's
# claim is about both.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace_root="$(cd "${script_dir}/../../.." && pwd)"
headers_dir="${workspace_root}/apps/shared/webrtc-cpp-headers/M125/include"
if [[ ! -d "${headers_dir}/api/crypto" ]]; then
    echo "test-receiver-delay.sh: webrtc-cpp-headers not populated; run apps/shared/webrtc-cpp-headers/M125/fetch.sh first" >&2
    exit 1
fi

build_dir="${script_dir}/build"
mkdir -p "${build_dir}"

run_one() {
    local name="$1"
    shift
    echo "test-receiver-delay.sh: building ${name}"
    c++ -std=c++17 -Wall -Wextra -Wno-unused-parameter "$@" \
        -I "${headers_dir}" \
        -I "${script_dir}" \
        "${script_dir}/test_receiver_delay.cc" \
        "${script_dir}/test_checks_stub.cc" \
        -o "${build_dir}/${name}"
    "${build_dir}/${name}"
}

run_one test_receiver_delay_o2 -O2
run_one test_receiver_delay_o0 -O0

if [[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "arm64" ]]; then
    if arch -x86_64 /usr/bin/true 2>/dev/null; then
        run_one test_receiver_delay_x86_64 -O2 -arch x86_64
    else
        echo "test-receiver-delay.sh: no x86_64 translation on this host; x86_64 slice not run"
    fi
fi
