<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-calls/src/call_key_ring.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if call_key_ring.rs changes and this file does not. -->

# `call_key_ring` — SFrame epoch-overlap keyring (spec)

**Module:** `crates/lakko-calls/src/call_key_ring.rs`
**One line:** Hold the current call key plus, during a mid-call rotation, the previous one for a short overlap window, so in-flight frames sealed under the old epoch still decode without ever falling back to plaintext.

## Why this module exists

When a group's MLS epoch advances during a live call (a participant Add/Remove, or a forced rotation), the per-call SFrame seed changes (C-8). Frames already in flight were sealed under the old seed and arrive after the receiver has switched to the new one. Without an overlap window those frames would fail authentication and drop. This module is the single source of truth for the overlap **decision**: retain the outgoing key for a bounded window, try current-then-previous on the receive path, and drop the previous key once the window closes.

It exists as a **seed-based** type — not the MLS-group-based `group_call::GroupCallSession` — because the client media layers that actually seal/open frames (the web `RTCRtpScriptTransform` worker, the iOS/Android native SFrame shims) run off the main thread and hold only the 32-byte seed handed across the FFI/worker boundary. They do not have the MLS group. `GroupCallSession` delegates its overlap fields to this type so the retain/clamp/fall-back logic lives in exactly one place rather than being re-implemented in three platform timers (G-18, mid-call rekey; ADR-0029).

The core is **time-source-free**: the caller supplies `now_ms` on every rotate and prune; the ring never reads a clock. This keeps the overlap decision deterministic and testable, and lets each platform own its own timer.

## Public contract

- `CallKeyRing::new(current)` / `from_seed(seed)` — build a ring with a current key and no previous key retained. `from_seed` is the shape the FFI-wrapped media workers use.
- `rekey(next, expires_at_ms)` — rotate to `next`, stashing the outgoing current key as the previous key with an absolute expiry. Any earlier previous key is **overwritten** — only one previous epoch is ever retained (C-10). `u64::MAX` means "never auto-expire; the caller owns the drop".
- `rekey_at(next, now_ms, overlap_ms)` / `rekey_from_seed_at(seed, now_ms, overlap_ms)` — clock-aware rotate: the previous key expires at `now_ms + clamp(overlap_ms)`. The overlap window is the ADR-0008 default (`OVERLAP_DEFAULT_MS = 1000`) unless the caller overrides it, and is always clamped to `[OVERLAP_MIN_MS, OVERLAP_MAX_MS]` (50 ms .. 10 s).
- `participant_key(sender_leaf)` — derive the seal-side per-participant key from the **current** key. Seal always uses the current epoch; there is no overlap on the send side (already-sealed frames are just bytes).
- `try_open_with_overlap(sender_leaf, aad, ciphertext, frame_counter)` — open a raw SFrame with the overlap fall-back: try the current per-participant key; on AEAD failure, and only while a previous key is retained, try the previous one. Returns plaintext on either path.
- `try_open_framed_with_overlap(sender_leaf, wire)` — same behaviour for an RFC 9605 framed SFrame that carries its own KID+CTR header. The header is derived from the sender leaf + counter (not the seed), so the previous key parses the identical header and can open a late old-epoch frame.
- `prune_expired(now_ms)` — drop the previous key once `now_ms >= expires_at_ms`. Idempotent; a no-op inside the window, with no previous key, or when retained with `u64::MAX`.
- `has_live_previous(now_ms)` — whether a previous key is retained AND still inside its window.
- `drop_previous()` — unconditional drop, the manual-timer path. Idempotent.
- `previous()` — diagnostic/test accessor; production receive paths funnel through the two `try_open_*` methods.

## State machine

The ring is a two-slot key holder (`current`, `Option<previous>`) with a clock-parameterised transition set:

- **Steady state** — only `current` populated. `try_open_*` collapses to "current key only"; a frame that fails AEAD under the current key is a hard `Error::Crypto` (no overlap window active).
- **Rotate** (`rekey*`) — the old `current` moves into the `previous` slot with an expiry; the new key becomes `current`. A pre-existing `previous` is displaced (C-10: single slot, most-recent-only).
- **Overlap window open** — both slots live. Seal uses `current`; open tries `current` then `previous`. Old-epoch and new-epoch frames both decode.
- **Window close** — either `prune_expired(now_ms)` past the expiry, or `drop_previous()`. The `previous` slot is emptied; old-epoch frames stop decoding, restoring the seed-boundary forward-secrecy the rotation was for (C-9 / C-10).

The window boundary is a `now_ms >= expires_at_ms` comparison, so `has_live_previous` and `prune_expired` agree exactly on when a key is live.

## Invariants upheld

- **C-8 (epoch advance rotates the seed):** rotation is driven by `rekey_from_seed_at` with the new exporter-derived seed; the previous seed is retained only transiently.
- **C-9 (overlap absorbs in-flight and only in-flight frames):** the previous key opens a frame only while inside its clamped window; after `prune_expired` / `drop_previous` the frame ceases to decode. The window is clamped to `[50 ms, 10 s]` so it can neither be too brief to catch in-flight frames nor lingered long enough to meaningfully weaken forward secrecy.
- **C-10 (only the most-recent previous epoch retained):** a single `previous` slot; `rekey` overwrites via `std::mem::replace`, so two rapid rotations keep only the latest previous key.
- **No plaintext fallback (security invariant #4):** every open path returns plaintext only after a genuine AEAD success under current OR previous key. A frame that authenticates under neither is `Error::Crypto` — never returned as cleartext.
- **Bounded time arithmetic:** `rekey_at` uses `saturating_add`, so a near-`u64::MAX` `now_ms` plus the overlap cannot overflow into a premature-expiry wraparound.

## Failure modes

- `Error::Crypto("... no overlap window active")` — a frame failed AEAD under the current key and no previous key is retained. Distinct wording from the framed path so platform debug logs can tell "missed a rotation" from "wire corruption".
- `Error::Crypto` (post-fall-back) — the previous key was retained but the frame authenticated under neither key; the previous key's own `open`/`open_framed` result is returned as-is.
- **Non-AEAD errors are propagated immediately, not treated as an overlap case.** In `try_open_with_overlap` and `try_open_framed_with_overlap`, only `Err(Error::Crypto(_))` triggers the previous-key fall-back; any other error (e.g. a malformed RFC 9605 header parse, participant-key derivation failure) short-circuits. This is deliberate — a header-parse failure is not "wrong epoch".
- **No panics.** No `unwrap`/`expect`/`panic!` in the module body (the `expect` calls are confined to `#[cfg(test)]`). Slot access is via `Option` combinators; the only arithmetic is `saturating_add` and a `clamp`.

## What this module does NOT do

- **No cryptography of its own.** All sealing/opening/derivation is delegated to `sframe::{CallKey, ParticipantKey}`. This module only chooses which key to try.
- **No clock.** It never reads wall-clock time; the caller supplies `now_ms` on rotate and prune. The 1-second timer lives in the platform layer.
- **No automatic pruning.** The previous key is dropped only when the caller invokes `prune_expired` or `drop_previous`; the ring does not self-expire. `u64::MAX` opts out of auto-expiry entirely, leaving the drop to the platform's own timer.
- **No send-side overlap.** Seal is always current-epoch. There is no attempt to re-seal or keep an old sealing key.
- **No MLS state.** It does not derive seeds from a group, verify commits, or advance epochs — the seed is handed to it already derived.

## Attacker exposure

`try_open_framed_with_overlap` runs on `wire` bytes handed over by an **untrusted SFU or peer** before authentication. The bound is: the RFC 9605 header parse (in `sframe`) must stay panic-free and bounded, and a frame that authenticates under no held key must fail closed with `Error::Crypto` rather than yield plaintext — both hold here. Note this module owns the *keyring* logic, not the header parser: the attacker-controlled wire-header decode is exercised by the `header_decode` fuzz target (`crates/lakko-calls/fuzz/fuzz_targets/header_decode.rs`); there is no dedicated `call_key_ring` fuzz target — the overlap logic is covered by the in-module property tests below rather than a fuzzer.

The residual availability consideration is C-10's chained-rotation bound: the ring keeps only one previous slot, so a burst of rotations displaces older previous keys. Frames from a two-rotations-ago epoch cease to decode; this is by design (bounded memory, matches the SFU's ≤ 1 commit/sec debouncing) and is a drop, never a downgrade.

## References

- ADR-0008 (overlap-window origin; the `OVERLAP_DEFAULT_MS` default)
- ADR-0029 (G-18 mid-call key rotation; the "core via FFI" seed-based extraction)
- Invariants C-8, C-9, C-10 (`docs/security/calls.md`); security invariant #4 (no non-E2EE fallback)
- Tests (in-module `#[cfg(test)]`): `framed_round_trip_no_overlap`, `overlap_opens_both_old_and_new_epoch_frames`, `prune_after_expiry_closes_the_overlap_window`, `overlap_window_is_clamped_to_bounds`, `raw_open_with_overlap_round_trips_current_and_previous`, `no_expiry_previous_survives_prune_and_closes_on_manual_drop`
