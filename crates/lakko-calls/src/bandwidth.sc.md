<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-calls/src/bandwidth.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if bandwidth.rs changes and this file does not. -->

# `bandwidth` — receive-side simulcast layer selection (spec)

**Module:** `crates/lakko-calls/src/bandwidth.rs`
**One line:** A per-participant hysteresis state machine that folds `quality::QualityScore` observations into `LayerSwitch` recommendations — which simulcast/SVC layer the receiver should subscribe to for a given remote peer.

## Why this module exists

In a group call every publisher sends video at three resolutions (a standard WebRTC simulcast ladder), and each receiver picks ONE layer per remote peer to actually pull from the SFU. Naively re-picking the layer on every `QualityScore` would ping-pong the choice at threshold boundaries (4.05 MOS → Medium, 4.02 → Low, 4.06 → Medium…), and each switch costs an SFU subscription round-trip plus a decode discontinuity. This module is the pure-data policy layer that decides *when* to switch, damping the jitter so a stable choice holds for tens of seconds rather than flapping every sample. It emits a recommendation; it never talks to the SFU.

## The ladder

`SimulcastLayer` is a three-rung ordered enum whose discriminants ARE the SFU's canonical numeric encoding (`Low = 0`, `Medium = 1`, `High = 2`), so `PartialOrd`/`Ord` rank the layers directly. Per-layer target bitrates (`target_bps`) are pinned data the SFU uses to size receive buffers: Low 150 kbps, Medium 750 kbps, High 2.5 Mbps. `step_down`/`step_up` walk one rung and return `None` at the floor/ceiling — the caller reads `None` as "stay put".

## Public contract

- `BandwidthController::new(initial)` — one controller per remote participant; the caller seeds the starting layer from UI intent (pinned tile → `High`, gallery tile → `Medium`). Streaks start at zero, unpinned.
- `observe(&mut self, score) -> LayerSwitch` — the sole state-advancing entry point. Folds one quality observation in and returns `Hold` / `DownTo(layer)` / `UpTo(layer)`. The common case is `Hold`. When it returns `DownTo`/`UpTo`, the controller's `current` layer has ALREADY moved to that layer and the confirming streak has reset to zero — the return value and internal state agree, so the caller just issues the SFU subscription for the named layer.
- `set_pin(layer)` — freeze this participant at `layer` (the "active speaker" / pinned-tile UX). Immediately sets `current = layer` and clears both streaks. While pinned, `observe` short-circuits to `Hold` before inspecting the score.
- `clear_pin()` — drop the pin; subsequent `observe` calls resume adaptation from wherever `current` now sits.
- `current_layer()` / `is_pinned()` — read-only accessors.
- `CONFIRMATION_STREAK = 2` — public constant; the number of consecutive confirming samples required before a switch fires.

## State machine (the hysteresis rule)

`observe` dispatches on `score.level` (the `QualityLevel` band), NOT the raw MOS:

- **Bad or Poor** → increment `bad_streak` (saturating), reset `good_streak`. When `bad_streak` reaches `CONFIRMATION_STREAK`, step the layer down one rung and reset `bad_streak`; if already at `Low`, reset `bad_streak` anyway (so a later recovery can act immediately) and `Hold`.
- **Excellent** → increment `good_streak` (saturating), reset `bad_streak`. When `good_streak` reaches `CONFIRMATION_STREAK`, step up one rung and reset `good_streak`; if already at `High`, reset and `Hold`.
- **Good or Fair (mid-band)** → reset BOTH streaks to zero and `Hold`. A mid-band sample is treated as a clean baseline, so a switch always needs two *consecutive* extreme samples with no mid-band interruption between them.

Net effect: a single blip never moves the layer, and one good/fair sample in the middle of a bad run cancels the accumulated downgrade pressure.

## Invariants upheld

This is receive-side UX policy with no cryptographic or wire role, so no `I-`/`C-`/`T-` security invariant applies. The behavioral guarantees are:

- **Two-consecutive-sample confirmation** — no `DownTo`/`UpTo` is ever returned off a single sample; the streak must reach `CONFIRMATION_STREAK` with no resetting sample in between.
- **Bounded within the ladder** — `step_down`/`step_up` return `None` at the extremes, so the controller never recommends a layer outside `Low..=High`.
- **Pin dominates network** — while `pinned.is_some()`, `observe` returns `Hold` unconditionally and the layer stays at the pinned value.
- **Return value matches state** — when `observe` returns a switch, `current_layer()` already equals the switched-to layer.
- **No overflow** — streak counters use `saturating_add`, so an unbounded run of same-band samples cannot wrap the `u8` counters.

## Failure modes

None. Every entry point is total: `observe` always returns a `LayerSwitch`, the accessors are infallible, and there is no error type, `Result`, `unwrap`, `panic`, or allocation in the module. Saturating arithmetic makes the counters immune to overflow.

## What this module does NOT do

- **No SFU communication.** The `LayerSwitch` is a recommendation; the platform's WebRTC pod makes the actual subscription RPC.
- **No encode-side adaptation.** The publisher's own bitrate is chosen by `quality::recommend_bitrate`; this module only adapts the RECEIVE side's layer subscription.
- **No SVC-specific layer math.** SVC (AV1/VP9) reuses these tier names; the equivalent selective forwarding for temporal/spatial layers lives in the SFU, not here.
- **No timing or clock.** It counts samples, not wall-clock time; the ~2-second reaction window is a consequence of the caller's ~1 Hz sampling cadence, an assumption stated in the doc comment but not enforced here.

## Attacker exposure

None directly. Inputs are `QualityScore` values produced locally by `crate::quality` from the receiver's own WebRTC stats, not attacker-controlled bytes off the wire. A misbehaving SFU could degrade a peer's measured quality and nudge the layer choice, but the only outcome is a lower-resolution local subscription — a UX/bandwidth effect, never a confidentiality or integrity break (media stays SFrame-sealed regardless of layer). There is no fuzz target and none is warranted.

## References

- Module doc comment (Phase 3c7): the three-tier ladder, hysteresis rationale, and pinning policy.
- Peer module `crate::quality` — `QualityScore` / `QualityLevel` inputs and `recommend_bitrate` (the encode-side counterpart).
- Tests (in-module `#[cfg(test)]`): `two_consecutive_bad_samples_trigger_downgrade`, `single_bad_sample_does_not_downgrade`, `good_sample_between_bad_resets_streak`, `downgrade_chain_stops_at_low`, `upgrade_chain_stops_at_high`, `pinned_participant_ignores_bad_samples`, `clear_pin_resumes_adaptation`, `target_bps_documented`, `confirmation_streak_constant_is_two`.
