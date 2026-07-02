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
#   EMPTY by default (no filtered artifacts). The historical entry
#   `liblakko_ffi_uniffi.rlib` was non-reproducible under uniffi 0.28, whose
#   proc-macros serialised macro output via bincode (embedding
#   non-deterministic HashMap-ordering state). The uniffi 0.28 -> 0.31 bump
#   dropped bincode (see known-gaps G-25), and the rlib is now byte-identical
#   across two clean same-runner builds — verified firsthand (2026-06-30):
#   two `cargo build -p lakko-ffi-uniffi --release --locked` runs into the
#   SAME target dir with `cargo clean` between (the CI repro methodology) +
#   the repro RUSTFLAGS / SOURCE_DATE_EPOCH produced the identical SHA-256.
#   So the default is now empty — every artifact is compared. Override with
#   LAKKO_REPRO_KNOWN_FLAKY (an extended-regex alternation) if a new upstream
#   flake appears.

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

# Default known-flaky set is now EMPTY (the uniffi rlib became reproducible
# under uniffi 0.31 — see the header note). Override via env if a new upstream
# flake appears; empty string = filter nothing.
known_flaky="${LAKKO_REPRO_KNOWN_FLAKY-}"

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
