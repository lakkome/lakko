<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0032: Multi-joiner shareable call-invite links — owner-founds + deterministic committer election

- **Status:** Accepted (maintainer drove the design)
- **Date:** 2026-07-14
- **Context:** Phase 3 (calls) — close the G-21 "link-based call invites are
  single-joiner in V1" limitation with a shareable link many strangers can join,
  surviving the owner leaving mid-call.
- **Relates to:** [ADR-0009](0009-link-based-call-invitations.md) (link-based call
  invitations — extended here); [ADR-0027](0027-external-join-approval.md)
  (admission approval, reused for the join toggle);
  [ADR-0028](0028-group-join-ticket.md) (joiner-initiated request shape mirrored).

## Context

A link-based call invite ([ADR-0009](0009-link-based-call-invitations.md)) is a
`CallInviteCapability` carrying `(owner_account_id, call_id, one-time
owner-KeyPackage, expiry, sig)`, served at `https://lakko.me/c/<blob_id>`. Today
it is **single-joiner** (G-21) for two structural reasons:

1. **The embedded owner-KeyPackage is one-time.** An MLS KeyPackage is single-use;
   one KP baked into a link shared to N strangers works for exactly one of them.
2. **Two joiners would found two groups.** A second joiner using the same link
   calls `Group::create` on its own side → a separate MLS group at a different
   epoch → the call does not connect (the original G-21 bug).

The multi-party wire primitives are already built + tested (`lakko-contacts`:
`call_welcome` welcome-ferry + `call_kp_forward` / `wrapCallKeyPackage`;
`dispatchIncomingEnvelope`; `lakko-calls::link_invite` with named E2E tests for
1×1, 3-party, and committer-leave handover — C-32 / C-33 / C-35 / C-36). A recon
confirmed the client wiring is genuinely absent on all three clients. What remains
is (a) that client wiring and (b) two design decisions: **who founds the group +
how joiners avoid the founding race**, and **what happens when the owner leaves
mid-call**. This ADR settles both.

## Decision

**Variant A — the owner is the founder; the acting committer is chosen by a
deterministic election so the call survives the owner leaving; every joiner is
symmetric and is admitted immediately by whoever the acting committer is.**

### Founding + joining

- The **owner** starts the call: founds the MLS group (its own leaf), joins the
  SFU room, shares the `/c/<slug>` link.
- A **joiner** opens the link: decode + verify the capability (owner's signature),
  mint its own fresh KeyPackage, and reach the acting committer to be added. It
  never calls `Group::create` — so there is **only ever one founder (the owner)**
  and no founding race. The capability's embedded one-time owner-KeyPackage is
  **unused** on this path (retained only for the ADR-0009 1×1 fast-path), so the
  one-time-KP-in-a-shared-link problem disappears — each joiner supplies its own
  fresh KP.
- **Admission toggle** (per call, owner's choice, mirroring
  [ADR-0027](0027-external-join-approval.md)): **auto** (any valid join is added)
  or **approve** (the acting committer sees "X wants to join" and confirms).

### Transport

- **Live call** (the acting committer is connected to the SFU room): the joiner
  connects to the SFU room (JWT-admitted for `call_id`) and its KeyPackage → the
  committer's Welcome ride the existing **SFU `signal-relay`** opaque
  peer-to-peer relay in-band. No stranger-inbox round-trip.
- **Cold start** (the owner is not in a live call yet): the built stranger-inbox
  **welcome-ferry** (`wrapCallWelcome` / `wrapCallKeyPackage` over the
  `ContactRequest` transport) bootstraps the first join asynchronously.

### Owner leaves mid-call — deterministic committer election

The **committer-of-record at any instant is a pure function of public state**, so
every member computes the same answer locally with no election protocol and no
split-brain:

> **committer = `owner_account_id` if that member is present (connected to the SFU
> room); otherwise the present member with the lowest MLS leaf index.**

Recomputed on every SFU roster change. Failover + reclaim fall out automatically:

- Owner drops → not present → all members agree the committer is now the
  lowest-leaf present member. That successor is a **full owner**: it drives in-call
  commits (removing dropped peers, rotating keys) **and admits new joiners
  immediately** — no one waits for the original owner.
- Owner returns → present again → the rule reverts the committer to the owner
  (reclaim), with no separate handshake.

### G-22 per-joiner dedup

The acting committer dedups incoming joins by `(call_id, joiner leaf key)` so a
re-sent KeyPackage is idempotent (add-once); the server `max_uses` on the
`/c/<blob_id>` blob is the orthogonal anti-abuse ceiling.

**Load-bearing properties:**

- **Single founder → no founding race** — the G-21 two-groups bug is closed at the
  design level, not patched with a racy existence check.
- **Deterministic committer → automatic, split-brain-free failover + reclaim** —
  no consensus/election protocol; a pure function of `(membership, SFU presence)`.
- **Zero new server surface (invariant #2 / #5)** — live joins reuse the SFU
  `signal-relay`; cold start reuses the `ContactRequest` stranger inbox; the DS/SFU
  still see only `(pseudonym → opaque ciphertext)` + opaque relayed bytes.
- **Fail-closed (invariant #4)** — a joiner arms SFrame on the derived seed but
  connects media only after it holds a valid Welcome and has joined the group; no
  plaintext or half-open call.
- **No new MLS state-machine surface (invariant #3)** — the orchestration composes
  `Group::create` + `add_member_from_wire` + `wrap_call_welcome`, all existing,
  audited, fuzz-covered paths (the `accept_call_invite` posture).

**Accepted residual (public-link authentication is inherently weak):** for a
*public shareable* link (strangers), "which committer is admitting me" is not
strongly authenticated — during owner downtime a fresh joiner, knowing only the
owner's key from the signed link, cannot cryptographically distinguish the
legitimate lowest-leaf successor from an impostor also in the room. This is a
**low-severity redirect/annoyance**, NOT a confidentiality break: the joiner's
media is always E2EE to whatever group it joined (never plaintext, #4), and the
**real call's E2EE among its actual members is untouched** — a redirected joiner
simply lands in a wrong/isolated group. This is inherent to public links (whose
model is already "anyone with the link participates"); the successor-admission
does not worsen the core E2EE invariants. A strong-authentication successor
delegation (the owner pre-signs a verifiable succession authorisation at
founding) is a possible future hardening, out of scope for V1.

## Alternatives

- **Variant C — keep the shipped joiner-founds topology.** Rejected: it needs a
  race-free "the group already exists" signal to pick first-vs-subsequent, and the
  zero-trust DS offers no atomic existence primitive → two joiners reading an empty
  journal both found (the G-21 bug). It also keeps the one-time-owner-KP conflict.
- **Wait for the original owner to return before admitting new joiners.**
  Rejected: unnecessary friction — the elected successor is a full owner; making
  newcomers wait for a specific person defeats the point of a shareable link, and
  (per the accepted residual) admitting via the successor does not break the real
  call's E2EE.
- **Any-member-can-commit (no designated committer).** Rejected: concurrent
  commits → epoch forks; the deterministic election gives exactly one committer.
- **Embed N KeyPackages in the link.** Rejected: a link is one static artefact;
  pre-embedding single-use KPs for an unbounded stranger set is impossible and
  re-use breaks forward secrecy.

## Consequences

- **Core / FFI:** a joiner-side "request-to-join a call" helper (decode capability
  → mint a fresh KeyPackage → hand it to the transport) + an owner/committer-side
  "found-or-add + ferry/relay Welcome" orchestration in `lakko-calls::link_invite`
  (mirroring `accept_call_invite`) + a pure `elect_committer(members, present) →
  leaf` helper (owner-if-present-else-min-leaf) with property tests, and an
  extension of the C-35 / C-36 E2E tests to a single-link N-joiner + owner-failover
  + reclaim case. FFI surfaces the entry points on UniFFI + wasm. No server change;
  no new MLS state-machine surface.
- **SFU:** live-call joins reuse the existing `signal-relay`; if a distinct frame
  kind for the KP/Welcome relay is cleaner than an opaque `payload_b64`, it is an
  additive `lakko-sfu-proto` frame (opaque to the SFU, invariant #2). Presence for
  the election reads the existing SFU roster.
- **Clients (web / iOS / Android):** a `/c/<slug>` route + deep-link handler +
  landing screen driving the joiner helper; the committer's inbound dispatch
  (SFU-relay + stranger-inbox) running `dispatchIncomingEnvelope` + found-or-add;
  the admission toggle UI; the deterministic-election recompute on roster change;
  a "waiting for owner" → "in-call" transition; and a local `(blob_id, joiner)`
  idempotency cache.
- **Invariants:** #2 / #3 / #4 / #5 all preserved (see load-bearing). C-32 / C-33 /
  C-36 reused. This ADR + the extended E2E tests are offered to `lakko-security`.
- **Deferred:** a strong-authentication successor delegation (owner pre-signs
  succession); a v2 slimmer capability dropping the embedded owner-KP; a live
  deployed-SFU N-joiner + failover e2e (runtime-gated, like G-26).

## UX (V1)

The protocol above serves a **live, ad-hoc** call — not a persistent room:

- **Owner hosts live; an empty call is dead.** The owner founds the group and
  hosts it live; if the owner shares the link then leaves *before anyone has
  joined*, the call is simply over (no live committer → nothing to found/admit
  into). A joiner whose relayed KeyPackage gets no committer response within a
  short window (~18 s) surfaces "this call has ended or isn't active" rather than
  waiting forever. Once ≥1 other member is present, the deterministic election
  keeps the call alive across the owner leaving (failover) as above.
- **Guest join (no account required).** A joiner who opens `/c/<slug>` with **no
  Lakko session** must still be able to join — so a full account is auto-created
  on the "Join" tap via `signupGuest` (web `AuthContext`): it reuses the audited
  `signup` machinery verbatim (OPAQUE registration under a random never-shown
  password, device-key bootstrap, first-ever identity-bundle publish with the
  G-30 genesis leaf-auth) but **skips `registerUsername`** — yielding a
  **username-less full account** with phrase-only recovery. It lands via the
  existing `save-fresh-phrase` state so the guest saves their recovery phrase;
  **claiming a username is the later onboarding step** (a random `guest-xxxx`
  handle is deliberately NOT minted — usernames are first-come immutable, so
  auto-squatting would block the user's real choice). `signupGuest` adds no new
  crypto; it is a new *entry point* to the existing account-creation path,
  reachable pre-ACCESS from a public link — offered to `lakko-security`.
- **Account-creation-spam mitigation ~~(deferred backend sprint)~~ — BUILT as
  [ADR-0033](0033-call-admission-antispam-throttle.md)** (recorded here
  2026-09-02; the forward link was missing). The throttle described below is
  what shipped, at the call-admission JWT mint in `services/auth`
  ([`throttle.rs`](../../services/auth/src/throttle.rs), with the
  `call_admission_events` + `throttled_calls` tables), and the
  owner-auto-suspension is still deliberately **not** built for exactly the
  weaponisability reason given here. One detail ADR-0033 added that this
  paragraph did not anticipate: the throttle **fails open** on a store error —
  a deliberate exception to the fail-closed default, because an anti-abuse
  counter that fails closed would self-DoS every call. Authorization still runs
  first and still fails closed. Auto-minting an
  account per anonymous link visit is an account-creation-spam surface. The
  mitigation is an **intelligent throttle at the call-admission / auth layer**
  (which sees `(account_id, call_id)` + account age per JWT mint): a burst of
  new-account joins on one link → **end the call + throttle further new joins on
  that link**. An automatic **owner-suspension** on such a burst is gated on a
  hard-to-forge abuse signal (App Attest / Play Integrity / device / a PoW cost
  on the guest mint), NOT the raw "many new accounts on `call_id`" counter —
  otherwise it is weaponisable (flood a victim's link → suspend the victim); the
  residual is low for privately-shared links (the attacker needs the link, which
  the owner hands only to intended people). This throttle is a separate backend
  arc; the client flow functions without it.

## References

- [ADR-0009](0009-link-based-call-invitations.md) — the link-based call-invite
  protocol this extends.
- [ADR-0027](0027-external-join-approval.md) — the admission-approval pattern the
  join toggle reuses.
- [ADR-0028](0028-group-join-ticket.md) — the joiner-initiated request shape.
- [ADR-0033](0033-call-admission-antispam-throttle.md) — the guest-account
  admission throttle that closes this ADR's named account-creation-spam
  residual.
- [ADR-0021](0021-sfu-peer-identity-exposure.md) — the per-call SFU peer tag /
  roster the presence election reads.
- G-21 / G-22 (`docs/security/known-gaps.md`) — the single-joiner limitation +
  per-joiner dedup this closes.
- Audit-pack C-32 / C-33 / C-35 / C-36 (`docs/security/calls.md`) — the
  welcome-ferry / KP-forward / multi-party / handover invariants reused.
- `crates/lakko-calls/src/link_invite.rs`,
  `crates/lakko-contacts/src/{call_welcome,call_kp_forward,call_invite}.rs`,
  `crates/lakko-sfu-proto/` — the built primitives + where the orchestration lands.
