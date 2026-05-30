#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Fetch the pinned subset of libwebrtc M125 C++ headers from the
# chromium googlesource mirror, verify each file's sha256 against
# the committed SHA256SUMS pin, write to `include/`.
#
# This is the ONLY sanctioned way to populate `include/`. The
# directory is gitignored; a contributor who hand-edits a file
# under `include/` bypasses the upstream-truth invariant the
# audit pack relies on (see ../README.md "Why `include/` is
# gitignored").
#
# Usage:
#   ./fetch.sh                 # fetch all FILES, verify against
#                                committed SHA256SUMS, abort on
#                                mismatch.
#   ./fetch.sh --refresh-pins  # fetch + write a fresh
#                                SHA256SUMS instead of verifying.
#                                ONLY for milestone-bump PRs; the
#                                resulting SHA256SUMS diff is what
#                                reviewers audit.
#
# Requires: bash >= 4, curl, sha256sum (linux) / shasum -a 256
# (macOS), base64.

set -euo pipefail

# --- Pin ------------------------------------------------------
#
# `branch-heads/6422` is the M125 release branch upstream
# chromium maintains for the same milestone stasel/WebRTC
# 125.0.0 + io.github.webrtc-sdk:android 125.6422.07 statically
# link. Bumping requires a fresh `apps/shared/webrtc-cpp-headers/MNNN/`
# directory and a coordinated stasel/WebRTC + io.github.webrtc-sdk
# bump on the platform side — see ../README.md "Milestone bumps".
WEBRTC_TAG="branch-heads/6422"
WEBRTC_BASE_URL="https://chromium.googlesource.com/external/webrtc/+/${WEBRTC_TAG}"

# --- File list ------------------------------------------------
#
# Every entry is the path relative to `webrtc/src/`, which is
# also the include path the upstream headers use internally
# (`#include "api/array_view.h"` etc.). We mirror the same
# layout under `include/` so the C++ subclasses can compile
# against the bundle by passing `-I include/` and writing
# `#include "api/crypto/frame_encryptor_interface.h"` verbatim
# the same way upstream code does.
#
# Why this exact set:
#   - frame_encryptor_interface.h        — what we subclass.
#   - frame_decryptor_interface.h        — what we subclass.
#   - array_view.h                       — rtc::ArrayView<T> parameter type.
#   - media_types.h                      — webrtc::MediaType enum (Audio / Video).
#   - ref_count.h                        — rtc::RefCountInterface base.
#   - scoped_refptr.h                    — rtc::scoped_refptr<T> return type.
#   - checks.h                           — RTC_DCHECK / RTC_CHECK macros transitively
#                                           pulled by array_view.h.
#
# A bump that introduces a new transitive include surfaces as a
# compile error in N-3 — at that point the bumper adds the new
# path here, re-runs the script, and commits the new
# SHA256SUMS entry.
FILES=(
    "api/crypto/frame_encryptor_interface.h"
    "api/crypto/frame_decryptor_interface.h"
    "api/array_view.h"
    "api/media_types.h"
    "api/scoped_refptr.h"
    "api/ref_counted_base.h"
    "rtc_base/ref_count.h"
    "rtc_base/checks.h"
    "rtc_base/numerics/safe_compare.h"
    "rtc_base/type_traits.h"
    "rtc_base/system/inline.h"
    "api/ref_count.h"
    "rtc_base/system/rtc_export.h"
)

# --- sha256 helper (macOS + linux) ----------------------------
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        echo "fetch.sh: need sha256sum (linux) or shasum (macOS)" >&2
        exit 1
    fi
}

# --- Mode -----------------------------------------------------
REFRESH_PINS="no"
if [[ "${1:-}" == "--refresh-pins" ]]; then
    REFRESH_PINS="yes"
fi

# --- Setup ----------------------------------------------------
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
include_dir="${script_dir}/include"
shims_dir="${script_dir}/include-shims"
sums_file="${script_dir}/SHA256SUMS"
mkdir -p "${include_dir}"

# --- Lay down committed shims ---------------------------------
#
# include-shims/ holds Lakko-authored stubs for the small set of
# abseil headers libwebrtc's checks.h transitively requires
# (`absl::string_view`, `absl::enable_if_t`, etc.). Vendoring
# the upstream abseil-cpp transitive closure would multiply the
# bundle by ~30x for symbols that boil down to C++17 std::
# aliases at the API level. See include-shims/absl/meta/
# type_traits.h for the security posture (pure alias templates,
# zero runtime artefacts).
#
# The shims are committed alongside fetch.sh + SHA256SUMS as the
# source of truth at PR-review time. We copy them into include/
# every run so a contributor who hand-edits a copy under include/
# loses the edit on the next fetch — same upstream-truth posture
# as the libwebrtc bundle itself.
if [[ -d "${shims_dir}" ]]; then
    echo "fetch.sh: laying down committed shims from ${shims_dir}/"
    # Portable across macOS + linux: walk the shims tree and
    # `cp -p` each file into the parallel path under include/.
    # GNU `cp -aT` would be cleaner but BSD cp on macOS does
    # not accept `-T`.
    (
        cd "${shims_dir}" && find . -type f -print0 \
            | while IFS= read -r -d '' rel; do
                dst="${include_dir}/${rel#./}"
                mkdir -p "$(dirname "${dst}")"
                cp -p "${rel}" "${dst}"
            done
    )
fi

if [[ "${REFRESH_PINS}" == "yes" ]]; then
    : > "${sums_file}.new"
fi

# --- Fetch loop -----------------------------------------------
for rel in "${FILES[@]}"; do
    # googlesource's `+` endpoint serves base64-encoded content
    # when you append `?format=TEXT`. Decode + write atomically
    # to avoid leaving half-written files on a flaky network.
    src_url="${WEBRTC_BASE_URL}/${rel}?format=TEXT"
    dst_path="${include_dir}/${rel}"
    mkdir -p "$(dirname "${dst_path}")"

    tmp_path="${dst_path}.tmp.$$"
    echo "fetch.sh: GET ${src_url}"
    # `-f` makes curl exit non-zero on 4xx/5xx, `-sSL` is quiet
    # + follow redirects + show errors.
    curl -fsSL "${src_url}" | base64 --decode > "${tmp_path}"

    got_sha=$(sha256_of "${tmp_path}")

    if [[ "${REFRESH_PINS}" == "yes" ]]; then
        # Append the fresh pin. The committed `SHA256SUMS` is
        # then a deliberate review surface for the bumper.
        printf '%s  %s\n' "${got_sha}" "${rel}" >> "${sums_file}.new"
        mv -f "${tmp_path}" "${dst_path}"
        echo "fetch.sh: refreshed pin for ${rel} -> ${got_sha}"
    else
        # Verify against committed pin.
        expected=$(grep -E "  ${rel}\$" "${sums_file}" | awk '{print $1}' || true)
        if [[ -z "${expected}" ]]; then
            rm -f "${tmp_path}"
            echo "fetch.sh: no pin for ${rel} in ${sums_file}." >&2
            echo "fetch.sh: add the file via --refresh-pins, then commit SHA256SUMS." >&2
            exit 1
        fi
        if [[ "${got_sha}" != "${expected}" ]]; then
            rm -f "${tmp_path}"
            echo "fetch.sh: SHA MISMATCH for ${rel}" >&2
            echo "  expected: ${expected}" >&2
            echo "  got:      ${got_sha}" >&2
            echo "fetch.sh: refusing to write; the upstream URL may have" >&2
            echo "          been tampered with or the milestone has been" >&2
            echo "          force-pushed. Investigate before re-running." >&2
            exit 1
        fi
        mv -f "${tmp_path}" "${dst_path}"
        echo "fetch.sh: verified ${rel}"
    fi
done

if [[ "${REFRESH_PINS}" == "yes" ]]; then
    mv -f "${sums_file}.new" "${sums_file}"
    echo "fetch.sh: wrote fresh ${sums_file}"
    echo "fetch.sh: review the diff and commit alongside the fetch.sh change."
fi

echo "fetch.sh: done — ${#FILES[@]} files in ${include_dir}/"
