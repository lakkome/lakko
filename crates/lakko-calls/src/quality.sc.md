<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-calls/src/quality.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if quality.rs changes and this file does not. -->

# `quality` — call-quality estimation + adaptive-bitrate hint (spec)

**Module:** `crates/lakko-calls/src/quality.rs`
**One line:** Turn a network/RTP stats sample (RTT, jitter, packet loss, codec) into a MOS/R-factor score and a discrete UX quality bucket, and recommend an encoder bitrate step — pure math, no I/O, no clock.

## Why this module exists

A call UI needs to show "how good is this call right now" and the encoder needs to know whether to back off or ramp up. Both derive from the same network sample. This module is the single place that maps that sample to (1) a `QualityScore` — an E-model R-factor, a Mean Opinion Score, and a coarse `QualityLevel` bucket — and (2) an adaptive-bitrate recommendation. It is deliberately platform-neutral: every client samples WebRTC stats (`RTCStatsReport` / `RTCStatistics` / `RTCStatsCollector`) on its own cadence and feeds `QualitySample`s in, so the scoring logic can be tested once in Rust and cannot drift across the three clients.

**Honest scope caveat.** The `QualityLevel` buckets (Excellent/Good/Fair/Poor/Bad) and their MOS thresholds are **UX display bands, not measured SLOs** — a user-facing indicator, not a network guarantee. This mirrors the same posture the client-side connection-quality tiers already carry (per CLAUDE.md). The E-model here is a deliberately **simplified** ITU-T G.107, not a calibrated reference implementation; it is tuned for monotonicity, sane bounds, and codec-awareness, not for absolute-MOS accuracy.

## Public contract

- `estimate_quality(QualitySample) -> QualityScore` — the core mapping. Guarantees, all exercised by in-module tests:
  - **Bounds:** `1.0 <= mos <= 4.5` and `0.0 <= r_factor <= 100.0` for any input (both go through `clamp`). 4.5 is the theoretical MOS ceiling under the simplified mapping; a real toll-quality Opus call lands ~4.4.
  - **Monotonicity:** increasing any one of `rtt_ms`, `jitter_ms`, or `loss_pct` (others fixed) never *raises* MOS. Verified across sweeps (`mos_monotonic_in_loss`, `..._in_rtt`, `..._in_jitter`).
  - **Codec-awareness:** at equal loss, `AudioCodec::Opus` scores `>=` `AudioCodec::Other`, because Opus's FEC is modelled as a flatter loss curve (a large `Bpl`), not because its zero-loss impairment differs — both codecs use `Ie = 0` at zero loss.
- `recommend_bitrate(current_bps, QualitySample) -> u32` — a one-sample step controller. `current_bps` is clamped into `[OPUS_MIN_BITRATE_BPS, OPUS_MAX_BITRATE_BPS]` *before* stepping and the result is clamped again. Bands: loss > 5% → ×0.8 (back off); loss < 1% AND MOS > 4.0 → ×1.1 (ramp up); otherwise return the (clamped) current bitrate unchanged. Smoothing across samples is explicitly the caller's job.
- `QualitySampler` — an optional rolling-window smoother so platforms don't roll their own. `record(sample)` pushes and returns a smoothed `QualityScore`; `latest()` re-reads it without pushing (`None` when empty); `clear()` resets for a new call/handoff; `window_size()`/`count()` are honest accessors. Smoothing: **median** R-factor and MOS (robust against a single loss spike), **majority-vote** `level` with ties broken toward the *better* bucket (anti-flicker — a pessimistic tie-break would alarm on every transient).
- `QualitySample::ideal(codec)` — a zero-impairment baseline (used as the pre-RTP initial value and in tests).
- Public constants: `OPUS_MIN_BITRATE_BPS` (8 kbps), `OPUS_MAX_BITRATE_BPS` (510 kbps), `DEFAULT_SAMPLE_WINDOW` (5), `MAX_SAMPLE_WINDOW` (60).

## Model (simplified ITU-T G.107)

`R = clamp(R_MAX − Id − Ie_eff, 0, 100)`, then `MOS = r_to_mos(R)`.
- `R_MAX = 93.2` (AAC-LC baseline; Opus matches after loss-curve compensation).
- `Id` = delay impairment from effective one-way delay (`RTT/2 + jitter`), with a per-ms slope and an extra penalty past a `177.3 ms` knee.
- `Ie_eff = Ie + (95 − Ie)·Ppl/(Ppl + Bpl)` (G.107 §A.2.1.2). Opus `Bpl = 25` (FEC-resilient), non-Opus `Bpl = 4.3` (G.711-like, no FEC).
- `r_to_mos`: 1.0 when `R < 6.5`, 4.5 when `R > 100`, else the cubic G.107 mapping clamped to `[1.0, 4.5]`.

## Invariants upheld

This module carries **no crypto/security invariant ids** — it touches no keys, no ciphertext, no wire format, no server state. Its guarantees are numeric, not security invariants: the bounds, monotonicity, and codec-ordering above, plus the sampler's bounded memory (`O(window_size)`, capped at `MAX_SAMPLE_WINDOW = 60`) and no-panic posture.

## Failure modes

- **No `Result`, no error type, no panic path.** Every function is total. `estimate_quality` and `recommend_bitrate` return a value for any input.
- Wide `rtt_ms`/`jitter_ms` (`u32`) are saturated to `u16::MAX` before the float conversion, so an absurd RTT can't produce a nonsense delay term — it saturates the impairment instead.
- Float comparisons that could see NaN (`median_f32`'s sort, `mos_to_level`'s threshold) fall back to `Ordering::Less` rather than panicking; NaN is not expected because every R-factor/MOS is `clamp`ed first, but the fallback keeps the code panic-free regardless.
- `QualitySampler::new` clamps any window size into `[1, MAX_SAMPLE_WINDOW]` (0 → 1), so there is no zero-window or unbounded-window state. `smoothed()` carries a `debug_assert!` that it is never called on an empty buffer; the public `latest()` guards that with an `Option` and `record()` always pushes first.

## What this module does NOT do (boundary)

- **No I/O, no wall clock, no sampling.** It never reads WebRTC stats itself and never tracks sample age — the caller samples on its schedule and must `clear()` the sampler on call end / network handoff. A stale window is the caller's responsibility, not this module's.
- **No security decision.** It gates nothing. A "Bad" score does not tear down or downgrade a call — invariant #4 (no non-E2EE fallback) lives entirely elsewhere; quality scoring never influences whether media is E2EE.
- **Not a calibrated MOS.** Do not treat the absolute MOS as a network SLA. The buckets are UX bands (see the scope caveat).
- **No multi-sample smoothing inside `recommend_bitrate`.** It is one-sample with one-sample hysteresis; production stacks are expected to wrap it (EWMA / vote across N) — the module says so and does not hide it.

## Notes for a reviewer

- The docstrings call `QualitySampler` an "N-wide ring buffer", but it is a `Vec` with `remove(0)` on overflow — an `O(N)` shift, not a true ring. This is called out honestly in the code (`N <= 60` so it is bounded and fine at 1–2 Hz); it is a naming imprecision, not a correctness bug.
- `OPUS_IE` and `OTHER_IE` are both `0.0`, so the codec choice affects *only* the loss curve (`Bpl`), never the zero-loss score — which is why `ideal(Other)` is also Excellent. Intended, documented, and pinned by `ideal_other_codec_also_excellent`.

## References

- ITU-T G.107 (E-model; simplified here), G.113 Appendix I (`Bpl` reference values), G.107 §A.2.1.2 (`Ie_eff` formula) — cited in code comments; the model is a deliberate simplification, not a conformance implementation.
- RFC 6716 (Opus; codec assumptions, bitrate range).
- Tests (in-module `#[cfg(test)]`): `ideal_opus_sample_is_excellent`, `mos_within_bounds_across_loss_sweep`, `mos_monotonic_in_{loss,rtt,jitter}`, `opus_more_resilient_than_other_codec_at_same_loss`, `recommend_bitrate_{decreases_on_high_loss,increases_on_clean_channel,holds_when_mediocre,clamps_to_floor,clamps_to_ceiling}`, and the `sampler_*` suite (majority vote, tie-prefers-higher, median-resists-spike, ring rotation, clear, window-of-one).
- No fuzz target — the module has no byte parser and no attacker-controlled input surface.
