<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0027: External-join proposal approval (G-3)

- **Status:** Accepted
- **Date:** 2026-06-24
- **Context:** Phase 3 (admin-side core) → Phase 4 (channel subscriber join)

> **AMENDED 2026-09-01 by [ADR-0045](0045-remote-commit-epoch-fork.md) S7 (`576ea14`).
> The approval mechanism described below is intact; what it *produces* changed.**
>
> `Group::approve_external_join` no longer commits. It publishes an `Add` **proposal the
> approving admin authors itself**, carrying the joiner's retained KeyPackage, which one
> elected committer later sweeps — so approving advances no epoch and returns no Welcome.
> Three consequences for a reader of this ADR:
>
> - **The approve is no longer immediate.** The joiner's wait is two-stage: *asked* →
>   *approved* (a proposal is on the wire) → *admitted* (a committer swept it and posted
>   the rendezvous). ADR-0045 decision 2 gives that last step **no deadline**.
> - **The Welcome moved to the sweep, and this ADR's Phase-3 rendezvous is where it
>   lands.** The sweeping device is not necessarily the approver and cannot tell which
>   adds came from a `/g/` link, so it posts a `WelcomeRendezvous` for **every** added
>   key. Pinned as **C-69**.
> - **The retention queue survives.** ADR-0045 decision 4 wanted `pending_external_joins`
>   retired; it is not, and the reason is load-bearing — it is the quarantine that keeps a
>   non-member out of the member proposal store, whose quota a `/g/` link-holder could
>   otherwise fill to freeze the group. See
>   [ADR-0045 § Why decision 4's design does not survive](0045-remote-commit-epoch-fork.md#why-decision-4s-design-does-not-survive).
>
> One framing below is now wrong rather than merely incomplete: a retained request is
> described as **epoch-bound and ephemeral**. The retained *KeyPackage* outlives epoch
> advances — only the joiner's original external proposal had to arrive at a matching
> epoch. What still drops it is a restart (the queue is in-memory) and, new under S7, the
> joiner having landed: `pending_external_joins()` filters members, so an approved request
> retires itself on every device with no message saying so.
>
> The **role gate is unchanged and is still the boundary.** What changed is that the
> admin's approval is now an MLS-authenticated artifact — its own signature on its own
> proposal — rather than a local decision expressed as a commit.

## Context

MLS (RFC 9420 §12.1.8.3) lets a **non-member** ask to join a group by sending
an **external Add proposal** (`Sender::NewMemberProposal`): the prospective
joiner frames a `Proposal::Add` carrying their own `KeyPackage` and routes it to
the group. A current member then commits the proposal, which adds the joiner and
produces a Welcome the joiner uses to bootstrap their view of the group.

Today `lakko-mls` surfaces this defensively and does nothing with it:
`Group::process_incoming*` returns a **data-less** `Incoming::ExternalJoinProposal`
unit variant — it drops the `QueuedProposal` (`ExternalJoinProposalMessage(_)`),
so there is **no joiner identity** to show and the proposal is **not retained**
in the proposal store. The uniffi FFI maps it to `ProcessedMessage::ExternalJoinProposal`
("defensive; no state") and the three clients ignore it entirely (G-3).

Consequently there is no way for a group to (a) see *who* asked to join or
(b) *approve* them. This is the prerequisite for the Phase-4 channel-subscriber
path ([ADR-0010](0010-channels-two-tier-broadcast.md)): a channel subscriber
joins the PublisherGroup precisely via an external join, gated by the
channel owner/admin.

**Forces:**

- **The MLS state machine is sacrosanct** (invariant #3) — any `lakko-mls`
  change needs an ADR + property tests + (post-Phase-2) an external crypto
  audit. Surfacing the joiner identity + driving a commit is exactly such a
  change.
- **Authorisation must be peer-verified** — approving a join is an Add commit,
  which [ADR-0022](0022-group-admin-roles.md) already gates: only an owner or
  admin may commit an Add, and peers refuse a commit from an unauthorised
  committer (`process_incoming_verifying_roles`). The approve path must reuse
  that gate, not invent a second one.
- **Fail-closed** (invariant #4) — a non-admin must not be able to approve; an
  unparsed / malformed proposal must drop, never auto-admit.
- **The joiner is not yet a member**, so the **Welcome delivery** to them is a
  transport problem distinct from the in-group Commit broadcast: the Welcome is
  HPKE-sealed to the joiner's KeyPackage init key, not to the group epoch, so it
  can be carried by an opaque relay without the relay learning group state
  (invariant #2).
- **Spam** — an open "anyone can ask to join" surface is a denial-of-service
  vector; admins must be able to reject without committing, and the proposal
  must be rate-limitable.

## Decision

Add an **admin-approved external-join** flow: `lakko-mls` surfaces the joiner's
identity from the external Add proposal and retains it; a **role-gated**
`approve_external_join` commits the pending proposal (producing the in-group
Commit + the joiner's Welcome); a `reject_external_join` discards it. The Welcome
is delivered to the joiner via a **journal rendezvous envelope** keyed on the
joiner's KeyPackage reference, which the (not-yet-member) joiner polls and opens
with their KeyPackage init key.

This is a deliberately **phased** decision — the admin-side core is contained
and testable now; the joiner-side proposal creation + the Welcome transport are
Phase-4-scale and land behind the channel path.

### Phase 1 — admin-side core (`lakko-mls`, testable now)

1. **Surface the joiner identity.** Replace the data-less unit variant with
   `Incoming::ExternalJoinProposal { joiner_signature_key: Vec<u8>, joiner_credential: Vec<u8>, proposal_ref: Vec<u8> }`,
   extracted from the `QueuedProposal`'s `Proposal::Add(AddProposal)` →
   `KeyPackage` → `LeafNode` (`signature_key()` + `credential()`), with the
   openmls `ProposalRef` so a later approve/reject targets exactly this proposal.
   A `QueuedProposal` whose inner proposal is **not** an `Add` from a
   `NewMemberProposal` sender is refused (fail-closed) — defence in depth against
   a member smuggling a non-Add through this path.
2. **Retain the proposal.** Call `MlsGroup::store_pending_proposal` so the
   proposal survives in the group's proposal store until an admin acts. (Today
   it is dropped.) Bounded: a `MAX_PENDING_EXTERNAL_JOINS` cap rejects further
   external proposals once the queue is full (anti-spam).
3. **Role-gated approve.** `Group::approve_external_join(identity, proposal_ref) -> MemberAddition`
   — first checks the local `RoleTable` (ADR-0022): the caller's own role must be
   owner or admin, else `Error::Unauthorized` (fail-closed, no commit produced);
   then `commit_to_pending_proposals` over the single targeted proposal →
   `(commit, welcome)`. The commit broadcasts to the group on the existing
   journal path; the welcome is the Phase-3 transport's payload. Because the
   resulting Add commit is itself role-gated on the receiving side
   (`process_incoming_verifying_roles`), a forged local approve cannot take
   effect on peers either — defence in depth.
4. **Reject.** `Group::reject_external_join(proposal_ref)` removes the pending
   proposal from the store without committing (no state advance, no membership
   change).
5. **Property tests:** valid-approve-adds-joiner · non-admin-approve-refused ·
   reject-removes-without-adding · non-Add-external-proposal-refused ·
   pending-cap-enforced · approved-commit-passes-the-role-gate-on-a-peer.
   Extend the `role_table_decode` / membership fuzz surface if a new parser is
   introduced.

### Phase 2 — FFI surface (`lakko-ffi-uniffi` + `lakko-ffi-wasm`)

`ProcessedMessage::ExternalJoinProposal` carries the joiner fields + the
`proposal_ref`; `approveExternalJoin(proposalRef) -> MemberAddition` and
`rejectExternalJoin(proposalRef)` exposed on both surfaces. Mirrors the
`processIncomingVerifyingRoles` / `setGroupRoles` shape from ADR-0022.

### Phase 3 — Welcome rendezvous transport (DS + clients)

The Welcome from an approve is posted to the **group journal** as a distinct
rendezvous envelope tagged with the joiner's KeyPackage reference (opaque to the
DS, invariant #2). The not-yet-member joiner — who knows the `group_id` because
they sent their proposal there — polls the journal for an envelope matching
their KP ref and opens it with their KeyPackage **init private key**
(`join_from_welcome`). No new service is required; the DS FIFO journal already
exists.

### Phase 4 — client UI (web + iOS + Android)

The members sheet shows a **pending external-join requests** section (the
joiner's identity resolved through the existing contact/transparency machinery
where possible, else the raw signature-key fingerprint), visible only to an
owner/admin (`ownRole`), with **Approve** / **Reject** actions wired to the
Phase-2 FFI. Mirrors the ADR-0022 members-management UI.

### Out of scope here (separate follow-ups)

- **Joiner-side proposal creation** — a non-member framing the external Add
  proposal needs the group's public parameters (group_id, epoch, ciphersuite,
  and a join ticket / `GroupInfo`). That is the channel **join-ticket**
  mechanism of [ADR-0010](0010-channels-two-tier-broadcast.md) and lands with
  the channel subscriber path. Phase 1's tests synthesise the external proposal
  directly against a test `MlsGroup` rather than depending on this.
- **Rate-limiting at the DS** — the `MAX_PENDING_EXTERNAL_JOINS` cap is a
  per-group client-side bound; a server-side per-account PoW gate on posting an
  external proposal is the DS-side anti-spam follow-up.

## Alternatives

1. **Auto-approve any external join** (no admin gate). Rejected — it makes every
   group openly joinable by anyone who can frame a proposal, a security +
   spam disaster, and contradicts invariant #4 and the ADR-0022 authority model.
2. **External *commit* self-join** (`join_by_external_commit`) instead of an
   approved proposal. Rejected — an external commit lets the joiner add
   *themselves* with no in-group approval step; we specifically want owner/admin
   gating, which the proposal-then-admin-commit shape gives. (A channel may
   later opt into self-join for a public-read tier, but that is a per-channel
   policy, not the default.)
3. **Leave it data-less** (status quo, surface a contentless indicator only).
   Rejected — it cannot show *who* or offer an action, so it does not actually
   close G-3; it would be busy-work UI with no payoff.

## Consequences

- **Positive:** closes G-3 with a real, peer-verified approve flow; provides the
  PublisherGroup-join primitive ADR-0010 needs; reuses the ADR-0022 role gate
  (one authority model, no second one); the Welcome stays HPKE-sealed end-to-end
  (server opaque).
- **Negative / new risks:** an `lakko-mls` state-machine change (mitigated by
  ADR + property tests + the existing audit posture); a new pending-proposal
  retention surface (bounded by the cap); a new journal rendezvous-envelope
  shape (opaque, but a new wire artefact to review); multi-admin race — two
  admins approving the same proposal both produce a commit, one wins the epoch
  and the other's is rejected as stale (benign, but surfaces as a transient
  error to handle in the client).
- **Neutral:** the joiner-side + channel-ticket work is deferred, so Phase 1-2
  ship a verifiable core with no live external-joiner until Phase 4 / channels.

## Open questions

- **Joiner mailbox / rendezvous identity** for the Welcome — Phase 3 keys it on
  the KeyPackage reference in the group journal; whether channels want a
  dedicated rendezvous instead is a Phase-4 decision.
- **Spam economics** — the per-group cap + a future DS PoW; the exact PoW shape
  is deferred to the DS-auth work.
- **Approve race** — the v1 accepts the transient stale-commit error; an
  optimistic "claim" marker on a pending proposal could pre-empt it (follow-up).

## References

- RFC 9420 (MLS) §12.1.8.3 — external join proposals; §12.4.3.1 — Welcome.
- [ADR-0001](0001-mls-only.md) — MLS for everything.
- [ADR-0010](0010-channels-two-tier-broadcast.md) — channels; the
  PublisherGroup join is the primary consumer of this flow.
- [ADR-0022](0022-group-admin-roles.md) — the owner/admin role gate the approve
  path reuses (`process_incoming_verifying_roles`).
- Security invariants #2 (server opaque), #3 (MLS sacrosanct), #4 (fail-closed).
