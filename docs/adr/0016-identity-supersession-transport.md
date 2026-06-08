<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0016: Identity-supersession proof transport — the discovery identity bundle (Carrier B)

- **Status:** Accepted — implement Carrier B (discovery identity bundle, pull); 2026-06-08
- **Date:** 2026-06-08
- **Context:** G-4 (`docs/security/known-gaps.md`) and security invariant #6
  ("identity rotation only via a KT-log supersession proof; old keys do not
  come back"). The supersession proof primitive, its strict verifier, the
  KT-log chain verifier, and the sign/verify FFI surface already landed
  (`crates/lakko-transparency/src/supersession.rs`, sprints 1–3 of G-4). What
  is missing is **transport**: a way for the proof to reach the peers who pinned
  the old AccountIdentity key, plus the client-side pin + verify wiring. This
  ADR picks the carrier and fixes the security rules; ADR-0007 (two-tier
  identity) is the load-bearing precedent.

## Context

When a user recovers their account (recover-from-words → new password → new
OPAQUE `exportKey` → new deterministic **AccountIdentity**), the account-level
Ed25519 key rotates. Per ADR-0007 the AccountIdentity signs the discovery
identity bundle, KT-log entries, and `ContactRequest.from_pubkey`; the random
per-device **DeviceIdentity** signs MLS group leaves and KeyPackages. A
consequence established by code review: **account rotation does not break
existing MLS conversations** — those ride per-device keys, which survive. A peer
therefore re-observes another user's account key only at two moments: a fresh
`ContactRequest`, or a **fetch of the user's discovery identity bundle**
(first-contact send / call-init). Nothing in steady-state messaging re-surfaces
it.

Today a rotation is a **blind TOFU**: a peer that somehow sees the new key has no
way to tie it to the old one and would silently trust-on-first-use (web's
contact store is worse — it keys on the pubkey, so a changed key forks the
contact). G-4 closes this with the supersession proof. The open question this ADR
answers is **how the proof travels peer-to-peer with no KT service yet deployed**.

## Decision

Carry the proof in the user's **discovery identity bundle** (Carrier B, a *pull*
model). The bundle (`services/discovery`, table `identity_bundles`) already holds
`account_id` / `signature_pubkey` / `x25519_pubkey` and is already re-published on
recover (web `apps/web/src/auth/AuthContext.tsx:897`). We add a nullable,
server-opaque `supersession_proof` field. A peer re-observes the account key only
when it fetches the bundle — and the **same response** carries the proof, so
verify-and-re-pin happens exactly where the key change is observed, with no new
transport, no routing, and no new persistent secrets.

### Why B is safe without the KT service (the load-bearing property)

The discovery server is **transport-only** for the proof. The proof is an
old-key-signed *public* artifact; the server stores it as opaque bytes and never
parses it. A malicious server can **strip** the proof — the client then sees a
changed key with no proof and **fails closed** (refuse + out-of-band re-verify).
That is an **availability** impact, never an **integrity** break: forging
acceptance of an attacker key needs the old key's signature, which the server
cannot produce. Full **equivocation** resistance (a server pinning a peer to an
older or forked successor) is explicitly **out of scope for this carrier** and
lands later with `verify_supersession_chain` + the KT log.

### Non-negotiable correctness rules

1. **Atomic emit-or-abort.** On recover, upload `{new keys + valid proof}` in one
   request, or upload **nothing** (keep serving the old bundle). Publishing new
   keys *without* a proof when one was mintable is a self-inflicted downgrade. The
   existing web `uploadIdentityBundle` is best-effort try/catch — the rotation
   path must not inherit "upload keys regardless".
2. **Fail-closed verify.** On fetch: fetched `signature_pubkey == pin` → proceed.
   Differs → **require** a proof, `verify_supersession(proof, pinned_old_pubkey,
   now)`; success → re-pin and advance a monotonic marker; absent / malformed /
   wrong-old-key / expired / window-bad → **refuse** (no send/call to the new key),
   surface OOB re-verify. First-ever observation (no pin yet) stays TOFU-pin.
3. **Pin keyed on `account_id`, not the pubkey.** A changed key must be a *detected
   mutation*, not a new contact (today's web `upsertAccepted` fork-on-change bug).
4. **Monotonic rotation marker.** The pin store records the last-accepted
   `expires_at` (or a counter) and refuses moving a pin **backwards** to an earlier
   successor, blunting within-window rollback.
5. **Emit window < 30 days** (e.g. 7–14 d). `MAX_VALIDITY_SECS` (30 d) is the hard
   ceiling; a shorter emit window bounds replay. Do **not** widen the ceiling to
   paper over the lazy-pull gap — that directly weakens invariant #6.

## Threat model

- **Invariant #2 (server opaque).** The proof is a nullable `VARBINARY(150)` the
  server never parses. The only new metadata exposed is "this account rotated
  recently", already implied by the changed `signature_pubkey` + `updated_at`.
- **Invariant #4 (no downgrade).** The verifier rejects every downgrade vector
  internally (`SupersessionOldKeyMismatch` / `SupersessionExpired` /
  `SupersessionInvalidValidityWindow`); invariant #4 reduces to "the client calls
  it and fails closed". The entire risk is in the verify-sprint wiring, so that
  sprint is gated through the `lakko-security` skill with negative tests
  ("changed key + absent/expired/wrong-key proof fails closed") as merge gates.
- **Invariant #6 (old keys do not return).** One-directional old-key signature +
  30-day ceiling + embedded-old-key-equals-pin binding + monotonic marker.
- **Residual risks (accepted, documented):**
  - **Lazy-pull window.** The proof reaches a peer only on their next bundle fetch,
    within the window; a peer first re-fetching after expiry sees the new key with
    no valid proof and falls back to OOB. Acceptable because the fallback is
    fail-closed, not silent acceptance. Proactive PUSH (a future, properly-scoped
    Carrier A) is the eventual closure, gated behind the KT-log chain verifier.
  - **Proof-stripping server** → availability impact only (see load-bearing
    property), never an integrity break.
  - **Replay** of an in-window proof re-asserts the *same* old→new transition (it
    cannot redirect to an attacker key); across a second rotation the replayed
    proof's `new_pubkey` no longer equals the served key and is rejected via
    `SupersessionOldKeyMismatch`. Equivocation on *which* successor is deferred to
    the KT log.

## Hidden prerequisites (verified in the tree)

- **Mobile UniFFI bindings are stale.** `signSupersessionProof` /
  `verifySupersession` are absent from the iOS and Android generated bindings →
  mobile cannot call the FFI until `just regen-bindings` (+ iOS `just build-ios`
  xcframework rebuild). This is a prerequisite sprint, not a line in emit.
- **Mobile never re-publishes the bundle on recover.** `uploadIdentityBundle` is
  defined but has zero production callers on iOS (`DiscoveryClient.swift:352`) and
  Android (`DiscoveryClient.kt:1032`); only web calls it. The mobile emit sprint
  must **add** the call, not augment one.

## Alternatives considered

- **Carrier A — new DS-mailbox envelope kind (push).** Rejected for v1. The
  mailbox routing key `IdentityPseudonym = BLAKE3("lakko-pseudonym/v1/identity",
  account_pubkey)` is derived from the rotating key, so the old inbox dies on the
  exact event we signal. The rotation-surviving `PerRelationship` pseudonym
  (`crates/lakko-contacts/src/pseudonym.rs`) is not persisted on any client.
  Push needs a large per-relationship-seed-persistence prerequisite first; it is
  the right *future* shape, gated behind the KT-log chain verifier so the push is
  non-equivocable.
- **Carrier C — piggyback on an MLS Commit.** Rejected: leaks identity-rotation
  state to every member of the conversation group.

## Decision (sprint breakdown)

A hard dependency chain across tiers; fan-out parallel within a tier (the
`lakko-dev` coordinator sequences it).

0. **Server column + wire mirrors** — `identity_bundles` migration adding nullable
   `supersession_proof VARBINARY(150)`; `services/discovery/src/identity_bundle.rs`
   threads an `Option<base64>` through upload + fetch (cleared on a non-rotation
   re-upload), never parsing the bytes; the three hand-mirrored wire types add the
   optional field in one logical change.
1. **Regenerate mobile UniFFI bindings** (`just regen-bindings` + `just build-ios`).
2. **Persistent per-contact account-pubkey pin store** ×3 (web localStorage schema
   bump keyed on account_id; new iOS `PeerPinStore.swift`; new Android
   `PeerPinStore.kt`), with a monotonic marker.
3. **Emit proof on recover (atomic)** ×3 — read the old `exportKey` before it is
   overwritten, sign, upload `{new keys + proof}` atomically (mobile must *add* the
   bundle re-publish).
4. **(Audit)** confirm recover is the only AccountIdentity rotation; fold a
   password-change rotation into the shared emit helper if one exists.
5. **Verify + re-pin on bundle fetch (fail-closed)** ×3 at every `identityFetch`
   site — the security-critical sprint, routed through `lakko-security`.

## Consequences

- G-4 moves from "primitive + verifier + FFI" to a deployed end-to-end rotation
  bridge under an honest-but-curious server, closing the blind-TOFU gap for the
  common recover-from-words case.
- The discovery service gains one opaque nullable column; server opacity (invariant
  #2) is preserved by construction.
- A documented residual remains (the lazy-pull window) and a clear future-work line
  (proactive PUSH + full equivocation resistance via the KT log) is recorded.
- The proof's transport is now **carrier-decided**, which unblocks exposing
  `verify_supersession_chain` over FFI when the KT service lands (it can reuse the
  same bundle channel, upgraded from "trust the old-key signature" to "trust the
  operator-signed Merkle inclusion").
