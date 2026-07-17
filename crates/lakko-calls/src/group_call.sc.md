<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-calls/src/group_call.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if group_call.rs changes and this file does not. -->

# `group_call` — group-call participant ledger + key derivation (spec)

**Module:** `crates/lakko-calls/src/group_call.rs`
**One line:** In-memory ledger for one group call — who is on the call, which MLS-exporter seed keys it, and the seal/open participant keys — with the mid-call epoch-rotation overlap delegated to `CallKeyRing`.

## Why this module exists

A group call is a subset of an MLS group's members who are *actually on the call right now* (chat members can decline or never join). This module owns that who-is-on-the-call bookkeeping plus the per-participant SFrame key derivation, so the crypto layer never has to reason about roster churn. The crypto primitives are unchanged from the 1x1 path: the same MLS exporter (`lakko/call/v1`), the same per-leaf `sframe::ParticipantKey`. What changes is that N leaves each derive their own seal key and every other leaf's open key, and the roster mutates mid-call as people join/leave and as the MLS epoch advances.

It is deliberately a *ledger*, not a transport: it folds in signaling events but does not decide who notifies whom (that is `signaling`), and it holds no clock (the caller supplies `now_ms`).

## Public contract

- `GroupCallSession::create(scope, call_id, group, identity)` — derives the initial `CallKey` seed from the MLS exporter and opens with an empty roster. The caller then drives `set_joined` per remote leaf plus once for the local leaf.
- `set_joined(leaf, joined)` / `forget_participant(leaf)` — roster mutation. `set_joined(_, false)` keeps the row (so the UI can render "Alice left" without a mid-render disappearance); `forget_participant` wipes it (used when the leaf is fully removed from the MLS group). Any *effective* change flips `needs_rekey`.
- `apply_remote_signaling(body)` — folds a `SignalingBody` into the ledger and returns a `SignalingOutcome`. Routes only the group-control variants (`GroupStart`→`Announced`, `GroupJoin`→`Joined`, `GroupLeave`→`Left`); 1x1 variants (`Offer`/`Answer`/`IceCandidate`/`End`) and any `call_id` mismatch return `Ignored` with the ledger untouched. `GroupStart` does NOT auto-join the initiator — the initiator's own `GroupJoin` lands separately.
- `own_participant_key(stream_index)` / `remote_participant_key(kid)` — derive the SEAL key for one of the local leaf's outbound streams and the OPEN key for any inbound `(leaf, stream)`. The sender assigns `stream_index` in a stable local order (0 = first stream, 1..N = additional — e.g. camera + screen-share); the receiver passes the composite `kid` it read from the SFrame header (forwarded verbatim by the SFU), needing NO local media-type or track-kind signal. Keying on the full KID `(leaf << 8) | stream_index` (ADR-0035) keeps every one of a participant's concurrent streams on a distinct key + nonce-salt space so their independent counters cannot reuse a `(key, nonce)` pair. `remote_participant_key` returns regardless of `joined` state so the caller can pre-warm keys before a join event lands; the `joined` filter is purely a UI/dispatch concern.
- `refresh_call_key(group, identity)` / `refresh_call_key_at(group, identity, now_ms, overlap_ms)` — re-derive the seed after an MLS epoch advance and retain the prior seed for the overlap window. The `_at` form stamps an expiry so the core owns the 1-second decision (G-18); the no-clock form retains with no expiry and requires a manual `drop_previous_call_key`.
- `try_open_with_overlap(kid, aad, ciphertext, frame_counter)` — the receive path: try the current seed's key for the composite `kid` (ADR-0035), then (only inside an overlap window) fall back to the previous seed's key.
- `needs_rekey()` / `ack_rekey()` — the caller polls `needs_rekey` after every mutation; `true` means re-derive every per-participant key and cycle the SFrame transform on the live `RTCPeerConnection`, then `ack_rekey`.
- `prune_expired_previous(now_ms)` / `has_live_previous(now_ms)` / `drop_previous_call_key()` — overlap-window lifecycle, all delegated to `CallKeyRing`.

## State machine

Two axes of state, each with its own transitions:

- **Roster** — `leaf_index → Participant { joined }`, a `BTreeMap` (stable iteration order for UI snapshots and golden tests). `joined` toggles independently of MLS membership. A left participant's row survives until `forget_participant`.
- **Key ring** — current seed plus an optional retained previous seed. On `create`: current only, no previous. On `refresh_call_key*`: current becomes the freshly-derived seed, the old current moves to the previous slot (C-10: only the most-recent previous epoch is retained — a second rotation displaces the first). The previous slot is emptied by `drop_previous_call_key`, or by `prune_expired_previous` once `now_ms` passes the stamped expiry.

`needs_rekey` is a one-way latch flipped by any effective roster or key-ring change and cleared only by `ack_rekey`. An idempotent `set_joined` (same value) does NOT flip it.

## Invariants upheld

- **C-1 (cross-party seed agreement):** two parties in the same MLS group at the same epoch derive byte-identical `CallKey` seeds — so A can seal with its own key and B opens with A's remote key, no shared secret on the wire. Verified by the cross-party and three-party full-mesh round-trip tests.
- **C-8 (epoch advance rotates the seed):** an MLS Add/Remove commit changes the exporter output, so `refresh_call_key*` produces a different seed; frames sealed under the old seed do not authenticate under the new one (seed-boundary forward secrecy).
- **C-9 (overlap absorbs in-flight frames):** the retained previous seed lets a late frame sealed under the old key still open during the window; the window is clamp-bounded (`[OVERLAP_MIN_MS, OVERLAP_MAX_MS]`) so it cannot linger long enough to weaken forward secrecy.
- **C-10 (only the most-recent previous epoch retained):** the ring overwrites any earlier stash on each rotation; a frame two epochs stale no longer decodes.
- All of the above are *delegated* to `CallKeyRing` — this module holds the roster and the exporter derivation and forwards the overlap logic to the ring so it lives in exactly one place (shared with the FFI-exposed platform media layers per G-18 / ADR-0029).

## Failure modes

- `create` / `refresh_call_key*` surface `Error::Exporter` if the MLS exporter rejects the label/length (practically unreachable with valid inputs).
- `own_participant_key` / `remote_participant_key` surface `Error::Crypto` on a key-derivation failure (practically unreachable with valid leaf indices).
- `try_open_with_overlap` returns `Error::Crypto` when the frame fails to authenticate under BOTH the current and (if present) previous keys — the overlap is transparent to the error type. Outside an overlap window the error message carries a "no overlap window" hint so the platform can distinguish a missed rotation from wire corruption.
- No `panic!`, no `unwrap`/`expect` in the module body — every fallible path returns via `?`. (The `unwrap`s in this file are confined to `#[cfg(test)]`.)

## What this module does NOT do

- **No wall clock.** The core is time-source-free; the caller passes `now_ms` on rekey/prune. The overlap *decision* (the 1-second policy + clamp) is owned here-via-the-ring, but the tick is the platform's.
- **No signaling transport.** It folds `SignalingBody` events in but does not send them or decide routing; the wire shape lives in `signaling`.
- **No auto-join.** `GroupStart` announces; it never marks anyone joined. `set_joined` is the only path that flips a participant on.
- **No counter or replay state.** The receive path authenticates a frame against a key; monotonic-counter / replay tracking is `sframe`/`ParticipantKey`'s concern, not the ledger's.
- **No silent downgrade.** A frame that authenticates under no held key returns an error — it is never surfaced as plaintext.

## Attacker exposure

`try_open_with_overlap` runs on `(sender_leaf, aad, ciphertext, frame_counter)` handed over by an untrusted SFU or peer, but every path terminates in an AEAD open — a forged or tampered frame (wrong AAD, flipped ciphertext byte, stale-under-new-key) fails authentication and returns `Error::Crypto`, proven by the cross-party wrong-AAD and post-rotation stale-frame tests. `apply_remote_signaling` gates on `call_id` equality before mutating, so a body for another call is `Ignored`. This module parses no bytes of its own (the SFrame header codec is `header.rs`), so there is no in-module wire parser and no dedicated fuzz target; the attacker-controlled byte-parsing surface it depends on (`header::decode`) is fuzzed by `crates/lakko-calls/fuzz/fuzz_targets/header_decode.rs`.

## References

- ADR-0001 (MLS keys everything, incl. call media), ADR-0008 (`CallJwtClaims` / overlap-window lineage), ADR-0013 (SFrame AAD = header bytes), ADR-0029 + G-18 (mid-call rekey, core-via-FFI overlap keyer), ADR-0035 (per-`(leaf, stream)` KID key separation — the `stream_index` / `kid` params on the participant-key derivation; supersedes the ADR-0034 per-media-TYPE param)
- Invariants C-1 (cross-party seed agreement), C-8 (epoch advance rotates seed), C-9 (overlap window), C-10 (most-recent-previous only) — see `docs/security/calls.md`
- Delegate: `crate::call_key_ring::CallKeyRing` (the overlap logic + clamp constants)
- Tests (in-module `#[cfg(test)]`): `cross_party_seal_open_round_trips`, `cross_party_round_trip_survives_epoch_advance`, `three_party_full_mesh_seal_open`, `overlap_window_accepts_stale_frame_until_dropped`, `chained_rotations_retain_only_most_recent_previous`, `cross_party_wrong_aad_rejects`, `refresh_at_retains_previous_until_expiry_then_prune_drops_it`, `refresh_at_clamps_the_overlap_window`
