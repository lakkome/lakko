<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# `tools/audio` — synthesised call sound cues

Five short sound effects used by the in-call UI on web /
iOS / Android:

| Name | Use |
|---|---|
| `ringtone-incoming` | Loop while an incoming call is awaiting the user's answer |
| `ringback-outgoing` | Loop while waiting for the callee to pick up |
| `join` | Fire-and-forget when a peer joins a multi-party call |
| `leave` | Fire-and-forget when a peer leaves a multi-party call |
| `error` | Fire-and-forget on connection failure / reconnect-failed |

## Provenance

These files are **fully Lakko-owned**. No third-party
samples, no royalty-free libraries with attribution
clauses, no unattested freesound borrows. Every sample
is a deterministic function of the sine / chirp
parameters in [`synth-sounds.py`](./synth-sounds.py) —
running the script twice produces byte-identical `.wav`
output. The bash wrapper at
[`synth-sounds.sh`](./synth-sounds.sh) re-encodes each
`.wav` into AAC LC `.m4a` (mono, 44.1 kHz, 64 kbps) and
distributes copies to:

- `apps/web/public/sounds/`
- `apps/ios/Lakko/Resources/Sounds/`
- `apps/android/app/src/main/res/raw/` (with underscored
  filenames — `R.raw.<name>` requires `[a-z_][a-z0-9_]*`)

## Regenerating

```bash
# From repo root.
tools/audio/synth-sounds.sh
```

Requires `python3` (stdlib only) and `ffmpeg` (any 4.0+
build). Both are present in the project's Nix flake;
the CI runner picks them up automatically.

## Why synthesise instead of buying / borrowing

1. **License clarity.** Lakko is AGPLv3. Any third-party
   sample with a different licence forces an exception
   into the SBOM. Synthesis from sine tones produces
   bits we own outright.
2. **Reproducibility.** The Nix flake pins toolchain
   versions; the `.wav` stage is bit-exact given the
   script's parameters; the `.m4a` stage is encoder-
   dependent but the CI runner pins `ffmpeg` for
   reproducible builds (see ADR-0006 / SLSA L3 posture).
3. **Audit hygiene.** Every byte of every cue has a
   visible mathematical provenance in
   [`synth-sounds.py`](./synth-sounds.py) — `sine_burst`
   sums sines, `chirp` walks a phase accumulator linearly,
   `silence` zeros. No black-box dataset.

## What lives where

- **`synth-sounds.py`** — pure-Python `.wav` generator.
  Reads no arguments beyond an output directory; every
  cue's parameters are hard-coded.
- **`synth-sounds.sh`** — `bash` driver that runs the
  Python emitter, then loops `ffmpeg` over the `.wav`
  files to produce `.m4a` copies in the three client
  resource trees.
- **The five `.m4a` files** under each platform's
  resource tree — checked in. They're tiny (≤ 19 kB
  each) and avoiding a CI-side generate-on-build keeps
  the deploy artefact deterministic.

## Player wiring

Each platform has a `CallSoundController` that loads the
cues by name and plays / loops them on lifecycle events:

- Web: [`apps/web/src/calls/CallSoundController.ts`](../../apps/web/src/calls/CallSoundController.ts)
- iOS: [`apps/ios/Lakko/Calls/CallSoundController.swift`](../../apps/ios/Lakko/Calls/CallSoundController.swift)
- Android: [`apps/android/app/src/main/kotlin/me/lakko/app/calls/CallSoundController.kt`](../../apps/android/app/src/main/kotlin/me/lakko/app/calls/CallSoundController.kt)

The three controllers share identical method shapes
(`playOnce`, `startLoop`, `stopLoop`, `stopAll`) and
identical cue-name strings — so trigger logic can be
expressed once per platform without per-cue branching.
