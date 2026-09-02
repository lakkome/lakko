<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0029: Mid-call call-key rotation (overlap keyring)

- **Status:** Accepted (maintainer chose Variant 2; implemented end-to-end on web + iOS + Android, 2026-07)
- **Date:** 2026-07-04
- **Context:** Phase 3 closeout — G-18 (overlap-window timer / mid-call rekey)

## Context

A group call derives its SFrame media key from the MLS group exporter:
`CallKey = group.export_secret("lakko/call/v1", call_id, 32)`, deterministic per
MLS epoch (audit-pack C-1). When the group's epoch **advances during a live
call** — a member is added or removed, or an Update commit rotates keys for
forward secrecy — that derived seed changes. The intended handling
([ADR-0008](0008-livekit-fork-strategy.md); call invariants C-8 / C-9 / C-10) is
a short **overlap window**: after re-deriving, the previous-epoch key is retained
for ~1 s so in-flight frames sealed under it still open, then dropped so
seed-boundary forward secrecy takes full effect.

The overlap logic already existed in the core
(`lakko_calls::group_call::GroupCallSession`: `refresh_call_key_at`,
`try_open_with_overlap`, `prune_expired_previous`, `has_live_previous`,
5 property tests) — but a code reconnaissance for G-18 found it is **entirely
dormant**: ~~no client instantiates `GroupCallSession`~~ **no client holds a
`GroupCallSession` handle**. The three clients seal /
open at a lower level — the main thread derives the 32-byte seed
(`deriveCallKey`) and hands it to the media layer, which holds one `CallKey` and
seals / opens per frame off the main thread (the web `RTCRtpScriptTransform`
worker; the iOS / Android native SFrame shims). No client re-derives the seed
mid-call, so a group whose epoch advances during a call keeps using the
start-of-call key.

> **Precision correction, 2026-09-02.** The original wording ("no client
> instantiates `GroupCallSession`") is over-broad and, read literally, false:
> `lakko_calls::accept_call_invite` **does** construct one, it is FFI-exposed on
> both surfaces, and the web acceptor calls it — as do `found_call` and
> `join_call_from_welcome`. What is true is that **nothing keeps the handle**:
> each FFI wrapper builds a session, drains `call_key()` / `call_id` /
> `own_leaf_index` into flat fields, and drops it. The uniffi wrapper says so at
> the call site — the seed is surfaced flat "without holding a
> `GroupCallSession` handle". The **conclusion is unaffected**: no live object
> carried the overlap state across a mid-call epoch change, which is exactly why
> the overlap logic had to be extracted into the seed-based `CallKeyRing` this
> ADR builds.

Consequences: a member added mid-call cannot key into the
call's media, and forward secrecy on a long call is not refreshed. G-18 is
therefore not "wire an existing call" — it is "build mid-call rekey on the level
the clients actually run at".

The load-bearing constraint: `GroupCallSession::refresh_call_key_at` re-derives
from the live MLS `Group` + `Identity`, but the media layers run off-thread and
hold only the **seed bytes** handed across the FFI / worker boundary — they do
not have the MLS group. So the overlap keyer that the clients drive must be
**seed-based**, not group-based.

## Decision

Expose the overlap logic as a **seed-based core type, `CallKeyRing`, driven via
FFI** — the "core via FFI" option (Variant 2, chosen by the maintainer over a
media-layer reimplementation).

`crates/lakko-calls/src/call_key_ring.rs` holds `CallKeyRing { current: CallKey,
previous: Option<RetainedCallKey> }` with the whole overlap surface: `from_seed`,
`rekey` / `rekey_at` / `rekey_from_seed_at`, `participant_key` (seal side, always
current), `try_open_with_overlap` + `try_open_framed_with_overlap` (receive side,
current-then-previous fall-back on an AEAD failure), `prune_expired`,
`has_live_previous`, `drop_previous`. The retain-until-expiry decision, the
`[OVERLAP_MIN_MS, OVERLAP_MAX_MS]` clamp, and the try-both control flow live here
**once**; `GroupCallSession` is refactored to delegate to a `CallKeyRing` so its
5 existing property tests stay green and the logic is not duplicated. The core
stays **time-source-free**: the caller supplies `now_ms` on rekey + prune.

The client media layers then hold a `CallKeyRing` (not a bare `CallKey`): seal
under the current key; open via `try_open_framed_with_overlap`; on a mid-call
epoch change the client re-derives the new seed on the main thread and calls
`rekey_from_seed_at(newSeed, now, OVERLAP_DEFAULT_MS)` on the ring, then ticks
`prune_expired` to close the window. The trigger is the existing chat/group
receive loop: when it processes a commit that advances the epoch during a live
call, it hands the freshly-derived seed to the active `SfuCallSession`.

Seal always uses the current epoch; the overlap is receive-only — so a rotation
never produces a frame the group cannot open, and forward secrecy is bounded by
the clamp (a previous key never lingers past `OVERLAP_MAX_MS`).

## Alternatives

- **Variant 1 — overlap in the media layer.** Teach the web worker + native
  shims to hold `{current, previous}` seeds and try-both themselves, reusing the
  already-FFI-exposed `CallKey` / `ParticipantKey`. Smaller blast radius, but the
  try-both + clamp + expiry logic gets reimplemented in TypeScript **and** C++,
  and the property-tested core `GroupCallSession` overlap stays dead. Rejected in
  favour of a single source of truth.
- **Retrofit clients onto `GroupCallSession` directly.** Impossible without
  change: `GroupCallSession` re-derives from the MLS group, which the off-thread
  media layers do not hold. `CallKeyRing` is the seed-based slice of exactly that
  type.
- **No overlap — hard cut-over on rekey.** Drops every in-flight frame sealed
  under the old key at the rotation instant (audible glitch). Rejected; the 1 s
  window is the ADR-0008 design.

## Consequences

- One property-tested overlap implementation (`CallKeyRing`), FFI-exposed for the
  clients and delegated-to by `GroupCallSession`. No TS/C++ duplication of the
  crypto control flow.
- **Invariants:** #4 (no non-E2EE fallback) holds — seal is always current-epoch
  and open only ever tries real keys; a frame that authenticates under neither is
  dropped, never surfaced as plaintext. C-8 / C-9 / C-10 (overlap retain / bound
  / most-recent-only) are now enforced in one place. #2 (server opaque) is
  untouched — the SFU never sees a key or a rotation. No `lakko-mls` state-machine
  change (the ring consumes the same exporter output), so invariant #3 does not
  trigger an MLS audit — but the change is call-crypto-relevant and is offered to
  `lakko-security`.
- **Deploy-gated verification.** The headless deliverable is the `CallKeyRing`
  core + FFI + client wiring that compiles + the property / unit tests of the
  two-key open logic. The live proof — two peers, one triggers a mid-call epoch
  change, media keeps flowing across the rotation with no dropped frames — needs
  a deployed SFU + two peers and is deferred, the same runtime gate as G-26.
- Per-frame receive re-derives `participant_key(sender_leaf)` on the current (and,
  during the window, previous) key rather than caching per-leaf as the current
  web worker does. The derivation is a cheap HKDF and the FFI call is in-process
  (wasm is loaded inside the worker), so it is accepted for v1; a per-leaf cache
  inside the ring is a later optimisation if a measurement shows it matters.

## Open questions

- The trigger cadence for `prune_expired` — a periodic clock tick vs. lazy on the
  next open. v1 ticks on the client's existing render/poll loop.
- Whether an Update-only rekey (no membership change) should rotate the call key
  at all, or only on membership deltas. v1 rotates on any epoch advance the call
  observes; a paranoia flag could force it.
- Simulcast interaction — a rekey mid-simulcast-switch. Out of scope here;
  simulcast layer selection (G3-3) is orthogonal to the key ring.

## References

- G-18 (`docs/security/known-gaps.md`) — the overlap-window / mid-call-rekey gap.
- [ADR-0008](0008-livekit-fork-strategy.md) — the 1 s overlap-window design.
- [ADR-0013](0013-sframe-cross-platform-aad.md) — SFrame AAD = the RFC 9605
  header, so a frame opens the same under either epoch's key.
- Call invariants C-1 / C-8 / C-9 / C-10 (`docs/security/calls.md`).
- `crates/lakko-calls/src/call_key_ring.rs` (the core), `.../group_call.rs` (the
  delegation).
