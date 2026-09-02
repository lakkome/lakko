<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0016: Identity-supersession proof transport — the discovery identity bundle (Carrier B)

- **Status:** Accepted — implement Carrier B (discovery identity bundle, pull);
  2026-06-08. **Built end to end — and then made DORMANT by
  [ADR-0017](./0017-phrase-seeded-identity.md)** (see the note below).
- **Date:** 2026-06-08

> **AMENDED 2026-09-02 (documentation audit). This ADR reads as live design
> work; the machinery it specifies is built but, in the ordinary case, never
> runs.**
>
> **Why.** [ADR-0017](./0017-phrase-seeded-identity.md) reseeded the
> `AccountIdentity` from the BIP39 recovery-phrase seed instead of the OPAQUE
> password export key. A routine recover or password change therefore
> **reproduces the same account key**, so there is no rotation, so there is no
> proof to emit — `recover` compares old and new pubkeys and skips the emit when
> they are equal. ADR-0017 states this in its own text ("**Renders ADR-0016
> dormant** for the recover path"); this ADR never said so.
>
> **Dormant is not deleted, and the distinction matters.** The carrier, the
> emit, and the fail-closed verify gate are all still built and still run on
> every `identityFetch`. What changed is the *frequency* of a rotation, not the
> handling of one — and there is exactly one path that still produces a genuine
> account-key change: ADR-0017's `mintFreshIdentity`, the lost-everything
> fallback. That path deliberately publishes **without** a proof (none is
> mintable once the old seed is gone), so it lands on this ADR's fail-closed
> branch: peers see a changed pin with no proof and **refuse**, and the contact
> re-verifies out of band. The verify gate below is therefore doing real work
> today; it is the *emit* side that is idle.
>
> **The "Hidden prerequisites" section below is fully resolved** — see the note
> there. So is the deferred `verify_supersession_chain` exposure.
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

> **Both resolved (verified 2026-09-02).** This section was a to-do list, and
> the work was done — the section was simply never struck through.

- ~~**Mobile UniFFI bindings are stale.**~~ **Resolved** —
  `signSupersessionProof` / `verifySupersession` are in both generated binding
  sets and are called from production code (`ContactsClient.swift`,
  `ContactsClient.kt`).
- ~~**Mobile never re-publishes the bundle on recover.**~~ **Resolved** —
  `uploadIdentityBundle` now has two production callers on each native platform,
  both in `AuthRepository`. (The line numbers cited here are also stale by
  several hundred lines in both files, which is a good reminder not to pin line
  numbers in an ADR.)

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
   threads an `Option<base64>` through upload + fetch (the proof is **preserved**
   across a non-rotation re-upload via `COALESCE` and only replaced by a new
   rotation's proof — see the post-review note below), never parsing the bytes;
   the three hand-mirrored wire types add the
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

## Security review (post-implementation)

A `lakko-security` adversarial pass over the whole feature (server + web/iOS/
Android emit/verify/pin) returned **APPROVE WITH NOTES**: the fail-closed verify
gate is correct and consistent across all three platforms, the atomic-emit and
monotonic-marker rules hold, and server opacity is preserved — no silent
fail-open in the verify path. Disposition of the findings:

- **#1 (MEDIUM, resolved) — non-rotation re-upload stripped a fresh proof.** The
  server originally **cleared** the proof on any no-proof re-upload; combined with
  the web re-uploading its bundle on every login, a routine login right after a
  recover could strip a still-valid proof before peers pulled it (shrinking the
  lazy-pull window — an availability/carrier-effectiveness loss, never an integrity
  break, since peers fail closed). Fixed at the root in the server: the upsert now
  **preserves** the stored proof on a no-proof re-upload (`COALESCE(VALUES(...),
  supersession_proof)`) and only replaces it on a new rotation; the proof
  self-expires via its 14-day window regardless. This fixes all platforms at once.
- **#2 (LOW, not applicable) — Android `oldExportKey` zeroization.** Reported as
  missing; on inspection the captured old export key **is** wiped in the recover
  `finally` (`oldExportKey?.fill(0)`). No change needed.
- **#3 (INFO, hardened) — Android verify gate could no-op when its pin store was
  null.** The nullable seam meant a future wiring regression could silently
  disable the gate. Closed: `ContactsClient`'s constructor now **requires** a
  non-null `PinStore` (the nullable param + the `pinStore ?: return` skip were
  removed), and `ContactsRepository` falls back to an ephemeral `InMemoryPinStore`
  when no `Context` is supplied (tests) — so the gate **always runs** and can no
  longer be disabled by construction. `:app:compileDebugKotlin` +
  `:app:testDebugUnitTest` (12 PeerPinStoreTest cases) green.
- **#4 (INFO, no action) — iOS `expires_at` signed-shift** could trap on a
  top-bit-set value; unreachable (a real `expires_at` is ~1.7e9 and the verifier
  bounds the window first), and a trap is a crash = fail-closed, not fail-open.

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
  **Done** (2026-09-02): the KT service landed and
  `verify_supersession_chain` is exposed on **both** FFI surfaces
  (`crates/lakko-ffi-uniffi`, `crates/lakko-ffi-wasm`), fuzzed
  (`verify_supersession_chain`), and the clients persist the chain-version floor
  in the pin store — delivered by
  [ADR-0019](./0019-client-authorised-log-leaf.md) Sprint 1. The upgrade this
  bullet anticipated is exactly what happened.
