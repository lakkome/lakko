#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Synthesises Lakko's call-side sound cues + fans them out to
# the three client resource trees.
#
# Pipeline:
#   1. synth-sounds.py emits five .wav files into a temp dir
#      (mono 16-bit PCM 44.1 kHz — deterministic from the
#      sine / chirp parameters in the script).
#   2. ffmpeg re-encodes each .wav to AAC LC, 64 kbps mono,
#      packed in an .m4a container — the format both iOS
#      (AVAudioPlayer / AVAudioEngine) and Android
#      (MediaPlayer / SoundPool) play natively.
#   3. The .m4a files land in three destination dirs:
#         apps/web/public/sounds/
#         apps/ios/Lakko/Resources/Sounds/
#         apps/android/app/src/main/res/raw/
#      The web copy is served straight from the public folder;
#      the iOS copy is bundled via xcodegen's `path: Lakko`
#      recursion; the Android copy is loaded by resource id
#      (`R.raw.<name>`).
#
# Idempotent — running again produces byte-identical .wav
# inputs (we synthesise from fixed parameters) and overwrites
# any previous .m4a outputs. The reproducibility check in CI
# diffs the .wav stage; the .m4a stage is encoder-dependent
# (ffmpeg version), so CI pins the ffmpeg version in the
# repro-build runner image.
#
# Usage:
#   tools/audio/synth-sounds.sh
#
# Requires:
#   python3 (stdlib only — `wave`, `struct`, `math`)
#   ffmpeg  (any 4+ build with libfdk_aac OR built-in aac)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TOOLS_DIR="$REPO_ROOT/tools/audio"

WEB_OUT="$REPO_ROOT/apps/web/public/sounds"
IOS_OUT="$REPO_ROOT/apps/ios/Lakko/Resources/Sounds"
AND_OUT="$REPO_ROOT/apps/android/app/src/main/res/raw"

note() { printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31mERR:\033[0m %s\n' "$*" >&2; exit 1; }

command -v python3 >/dev/null 2>&1 || fail "python3 not on PATH"
command -v ffmpeg  >/dev/null 2>&1 || fail "ffmpeg not on PATH"

note "synthesising .wav stage (deterministic from script params)"
WAV_DIR="$(mktemp -d -t lakko-sounds.XXXXXX)"
trap 'rm -rf "$WAV_DIR"' EXIT

python3 "$TOOLS_DIR/synth-sounds.py" "$WAV_DIR"

mkdir -p "$WEB_OUT" "$IOS_OUT" "$AND_OUT"

note "encoding .m4a + distributing to client resource trees"
for src in "$WAV_DIR"/*.wav; do
  name="$(basename "${src%.wav}")"

  # ffmpeg flags:
  #   -y               overwrite without prompting
  #   -hide_banner     less noise in CI logs
  #   -loglevel error  only surface real failures
  #   -i $src          input wav
  #   -c:a aac         native AAC encoder (no libfdk_aac dependency)
  #   -b:a 64k         64 kbps — these are short cues, not music
  #   -ac 1            mono
  #   -ar 44100        match the source rate, no resampling drift
  #   -movflags +faststart
  #                    moov atom up front — needed for streaming
  #                    playback on the web client without a seek
  # Android raw resource ids must match [a-z_][a-z0-9_]* —
  # dashes are not allowed by aapt. Use the underscored
  # variant for the Android copy only; web + iOS keep the
  # canonical kebab-case name.
  android_name="${name//-/_}"

  for dest in "$WEB_OUT" "$IOS_OUT"; do
    out="$dest/$name.m4a"
    ffmpeg -y -hide_banner -loglevel error \
      -i "$src" \
      -c:a aac -b:a 64k -ac 1 -ar 44100 \
      -movflags +faststart \
      "$out"
  done
  ffmpeg -y -hide_banner -loglevel error \
    -i "$src" \
    -c:a aac -b:a 64k -ac 1 -ar 44100 \
    -movflags +faststart \
    "$AND_OUT/$android_name.m4a"

  printf '  %-24s -> %s\n' "$name.m4a" "(web | ios | android:${android_name})"
done

note "done. files:"
ls -la "$WEB_OUT" "$IOS_OUT" "$AND_OUT"
