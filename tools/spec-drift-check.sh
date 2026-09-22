#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Codespeak spec-drift gate.
#
# A covered module `X.<ext>` has a sibling behavioral spec `X.sc.md`
# (see the `spec` skill). This gate fails when a covered source file
# changes in a changeset but its sibling spec does NOT — i.e. the spec
# has drifted out of lock-step with the code it describes.
#
# It only inspects modules that ALREADY have a `.sc.md`, so coverage can
# grow one module at a time without a big-bang migration: an uncovered
# module is simply never checked.
#
# Usage:
#   tools/spec-drift-check.sh                 # working tree + staged + untracked vs HEAD
#   tools/spec-drift-check.sh <git-range>     # e.g. origin/main...HEAD  (CI)
#
# Test hook (test-only): set SPEC_DRIFT_CHANGED to a whitespace-separated
# list of paths to bypass git and feed the changed set directly.

set -euo pipefail

# Source extensions that can carry a covered module.
SRC_EXT_RE='\.(rs|swift|kt|kts|ts|tsx)$'

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$repo_root"

collect_changed() {
  if [ -n "${SPEC_DRIFT_CHANGED:-}" ]; then
    printf '%s\n' $SPEC_DRIFT_CHANGED
    return
  fi
  if [ "$#" -ge 1 ] && [ -n "$1" ]; then
    git diff --name-only "$1"
  else
    # Tracked (staged + unstaged) changes vs HEAD, plus untracked files —
    # so a newly-written spec that has not been committed still counts.
    git diff --name-only HEAD
    git ls-files --others --exclude-standard
  fi
}

# Portable array fill (macOS ships bash 3.2 — no `mapfile`).
changed=()
while IFS= read -r line; do
  [ -n "$line" ] && changed+=("$line")
done < <(collect_changed "${1:-}" | sed '/^$/d' | sort -u)

# An empty array expands to an "unbound variable" error under `set -u` on
# bash 3.2, which macOS still ships — so every expansion below carries the
# `${arr[@]+...}` guard. Without it the gate fails precisely when there is
# nothing to check: a clean tree, which is the state right after a commit.

# Fast membership test over the changed set.
is_changed() {
  local needle="$1"
  local f
  for f in ${changed[@]+"${changed[@]}"}; do
    [ "$f" = "$needle" ] && return 0
  done
  return 1
}

drift=0
for f in ${changed[@]+"${changed[@]}"}; do
  # Only source files can drift a spec; skip the specs themselves + everything else.
  if ! printf '%s' "$f" | grep -Eq "$SRC_EXT_RE"; then
    continue
  fi
  spec="${f%.*}.sc.md"
  # Covered only if the sibling spec exists on disk.
  if [ -f "$spec" ]; then
    if ! is_changed "$spec"; then
      echo "SPEC DRIFT: '$f' changed but its spec '$spec' did not."
      drift=1
    fi
  fi
done

if [ "$drift" -ne 0 ]; then
  echo ""
  echo "A covered module changed without updating its .sc.md spec."
  echo "Update the spec in the same change (see the 'spec' skill), or remove the"
  echo ".sc.md if the module is no longer covered."
  exit 1
fi

echo "spec-drift: OK (no covered module changed without its spec)."
