#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Reproducible-build hash comparator (GA-4). Single source of truth for the
# byte-identicality diff shared by every repro job in
# `.github/workflows/repro.yml` (same-runner double-build, cross-runner matrix)
# AND the local verify in `ci/repro/README.md`, so the known-flaky filter +
# the diff semantics live in exactly one place.
#
# Usage:
#   compare-build-hashes.sh <hashes-a.txt> <hashes-b.txt> [label]
#
# Each input is a `sha256sum` manifest (one "<hex>  <name>" line per artifact,
# sorted). Exit 0 if the two manifests are byte-identical after filtering the
# known-non-reproducible artifacts; exit 1 (printing the diff) otherwise.
#
# Known-flaky filter:
#   liblakko_ffi_uniffi.rlib — uniffi 0.28's proc-macros embed
#   non-deterministic state across independent builds (tracked upstream).
#   Override the set with LAKKO_REPRO_KNOWN_FLAKY (an extended-regex
#   alternation) if a new upstream flake appears; pass an empty string to
#   compare every artifact.

set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <hashes-a.txt> <hashes-b.txt> [label]" >&2
  exit 2
fi

a="$1"
b="$2"
label="${3:-artifacts}"

for f in "$a" "$b"; do
  if [[ ! -f "$f" ]]; then
    echo "ERR: hash manifest not found: $f" >&2
    exit 2
  fi
done

# Default known-flaky set; override via env (empty string = filter nothing).
known_flaky="${LAKKO_REPRO_KNOWN_FLAKY-liblakko_ffi_uniffi\.rlib}"

a_filtered="$(mktemp)"
b_filtered="$(mktemp)"
trap 'rm -f "$a_filtered" "$b_filtered"' EXIT

if [[ -n "$known_flaky" ]]; then
  grep -v -E "$known_flaky" "$a" > "$a_filtered" || true
  grep -v -E "$known_flaky" "$b" > "$b_filtered" || true
else
  cp "$a" "$a_filtered"
  cp "$b" "$b_filtered"
fi

if diff -u "$a_filtered" "$b_filtered"; then
  if [[ -n "$known_flaky" ]]; then
    echo "OK reproducible: byte-identical $label (excluding known flaky: $known_flaky)"
  else
    echo "OK reproducible: byte-identical $label"
  fi
  exit 0
else
  echo "WARN non-reproducible: $label differ between the two builds"
  exit 1
fi
