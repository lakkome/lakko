<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0022: Group-admin roles — owner / admin / member, peer-verified

- **Status:** Accepted (2026-06-17) — design reviewed by `lakko-security`:
  **APPROVE WITH NOTES**. Findings 1 (single-account-leaf assumption + Phase-2b
  path), 2 (pre-merge epoch read as a stated invariant), 3 (owner device loss +
  ADR-0017 mitigation), 4 (version fail-closed), 5 (self-Remove test) folded
  into the text below. **Sprint 1 (lakko-mls core) landed + passed the
  post-implementation `lakko-security` review (APPROVE WITH NOTES)** — both the
  design and the landed-code gates are green (invariant #3). ~~Remaining: Sprint 2
  (FFI) + Sprint 3 (client wiring)~~ — **both landed** (corrected 2026-09-02):
  `ownRole` / `setGroupRoles` / `processIncomingVerifyingRoles` are on both FFI
  surfaces ([uniffi](../../crates/lakko-ffi-uniffi/src/lib.rs),
  [wasm](../../crates/lakko-ffi-wasm/src/mls.rs)) and all three clients apply
  commits through the role-verifying path and gate the members-panel
  affordances on `ownRole`. The two post-code notes (SelfRemove, call-path)
  remain in Open Questions.
- **Extended by [ADR-0045](0045-remote-commit-epoch-fork.md)** (2026-08;
  supersedes nothing here). ADR-0045 moves the role check from the **committer**
  to each **proposal's own author**, with the by-value arm left byte-for-byte as
  this ADR defined it; adds the `SelfRemove` branch this ADR deferred as
  unreachable; builds the **ownership transfer** this ADR deferred; and adds the
  commit-level never-orphan-the-owner rule (**C-63**) that closes a hole
  `set_group_roles` has carried since this ADR shipped. Every table row below
  still holds — what changed is *whose* role is consulted, not what each role may
  do. The three places in this document that described transfer as a follow-up
  are marked up rather than deleted.
- **Date:** 2026-06-17
- **Context:** Phase 3 closeout — blocker **C** (group-admin roles). Group-chat
  membership (add / remove / roster) now ships on all three clients (A4), but
  with **no authorization**: any member can add or remove any other member.

## Context

A Telegram-class group needs an authorization model: the person who created a
group is its owner, they can appoint admins, and only owner/admins can change
the membership. Today Lakko has none of this — `addMemberToGroup` /
`removeMemberFromGroup` on web + iOS + Android call straight into
`group.addMember` / `group.removeMember` with no gate, and the UI renders the
add / remove affordances to every member unconditionally.

Three forces shape the decision:

1. **MLS is egalitarian.** RFC 9420 has no notion of an admin. Any member can
   propose and commit an Add or a Remove; the protocol authenticates *that a
   member signed the commit*, not *that they were entitled to*. Roles must be
   layered **on top** of MLS.
2. **The server is zero-trust and opaque (invariant #2 / #5).** The Delivery
   Service stores an opaque per-group journal; it cannot interpret a commit,
   so it cannot enforce "only admins may remove." Enforcement must be
   **client-side and peer-verified**, exactly like ADR-0015's attested
   removal.
3. **We already have the two primitives we need.** ADR-0015 established (a) how
   to carry an app-level policy in MLS group state via a custom `GroupContext`
   extension that propagates to every joiner through the Welcome, and (b) a
   *refuse-to-apply* gate (`process_incoming_gated`) that inspects a staged
   commit and declines to merge it before any state mutates. Group-admin roles
   are the same shape with a different policy.

**Load-bearing feasibility finding.** A role gate must answer "who committed
this Add/Remove?" The current `process_incoming_gated`
(`crates/lakko-mls/src/group.rs`) calls `processed.into_content()` immediately,
discarding the message envelope. But OpenMLS 0.8.1 exposes the committer on the
envelope *before* `into_content()`: `ProcessedMessage::sender() -> &Sender`
(`openmls/src/framing/validation.rs:316`) returns `Sender::Member(LeafNodeIndex)`
for a member commit. So the committer's leaf — and, via the pre-merge member
list, their **signature public key** — is recoverable **without forking
OpenMLS**. This is what makes peer-verified role enforcement possible.

## Decision

**Adopt an owner / admin / member role model, stored in a new `GroupContext`
extension keyed on signature public key, and enforced by a peer-verified
refuse-to-apply gate that checks the committer's role before merging an
Add / Remove / role-change commit.** Off by default (legacy + 1×1 groups stay
open); the owner opts a group in.

### Roles

- **Owner** — the group creator (leaf 0 at creation). Exactly one. May add /
  remove members, promote / demote admins, and is the only role that can change
  the policy or transfer ownership. **Cannot be removed or demoted** by anyone
  else (a Remove of the owner's leaf is refused).
- **Admin** — appointed by the owner. May add / remove non-owner members. May
  *not* demote/remove another admin or the owner, nor change the policy (kept
  deliberately narrow for v1 — see Open questions).
- **Member** — may not add or remove anyone. May leave (an MLS self-Remove is
  always allowed — leaving is not an admin action).

### Where roles live

A new `GroupContext` extension `Extension::Unknown(0xFF02)` (0xFF01 is
ADR-0015's removal-policy), carrying a bounded, canonically-encoded body:

```
version: u8
owner_pubkey: [u8; 32]            // Ed25519 signature key, stable per account
admin_count: u16
admin_pubkeys: [[u8; 32]; admin_count]   // sorted, deduplicated
```

**Keyed on signature public key, never leaf index.** Leaf indices renumber on
every add/remove cycle; the per-account signature key (the same key a
`ContactRequest` is signed with and the roster matches against, ADR-0021) is
stable for the account's lifetime in the group. The body is bounded
(`admin_count` capped, e.g. 256) and parsed with the no-`unwrap` / no-`panic`
discipline; it is opaque to the server (invariant #2). **An unrecognised
`version` is refused before any further parsing — the extension is treated as
absent/invalid (fail-closed), not best-effort parsed** (the I-7 history-bundle
discriminator precedent).

**v1 assumes one account-level leaf per member in a conversation group**
(`lakko-security` ADR-0022 review, Finding 1). Conversation groups today are
created and committed with `Identity::from_seed(account_identity_seed)` — the
account-level deterministic identity (ADR-0017) — so a committer's leaf
signature key **equals** the account key stored in `0xFF02`, and the lookup
`committer leaf → signature pubkey → role table` resolves directly. ADR-0007
Phase 2b moves conversation groups to **per-device random** leaves; when that
lands, a committer's leaf key will be the *device* key, not the account key, and
the gate must first resolve the device leaf back to its account (via the
DeviceGroup membership ADR-0007 binds per-device identities to the account)
before the role lookup. **This is an explicit v1 boundary, recorded as a
stated invariant, not a silent assumption** — the gate and its property tests
assert the single-account-leaf model, and Phase 2b carries the device→account
resolution as a prerequisite.

The extension is part of the initial `GroupContext` at `Group::create` (owner =
creator, empty admin set) and travels to every joiner inside the Welcome, so a
new member learns the role table atomically with joining — no separate sync.

### How it is enforced

A new `DeviceGroup`-style apply path on the regular `Group`:
`process_incoming_verifying_roles(identity, commit_bytes)`. It extends
`process_incoming_gated` to capture **both** sides:

- the **committer** — `processed.sender()` → `Sender::Member(leaf)` → look up the
  committer's signature pubkey in the pre-merge member list;
- the **effect** — the Added key packages' signature keys and the Removed leaves'
  signature keys (the latter already surfaced by `process_incoming_gated`).

The gate then enforces, *before merge*:

| Commit effect | Allowed committer |
|---|---|
| Add a member | owner or admin |
| Remove a non-owner member | owner or admin |
| Remove the owner | **nobody** → refuse |
| Change the role extension (GroupContextExtensions) | owner only |

A commit that fails the check returns `Err` and is **not merged** (fail-closed,
invariant #4) — exactly ADR-0015's posture. Because the committer is
cryptographically authenticated by MLS (the commit is signed by their leaf key),
a member cannot forge a commit *as* another member; the only way to bypass the
gate is to run a patched client, whose unauthorized commit every honest peer
then refuses — so the offender forks themselves out of the honest members'
shared state rather than mutating it.

**Design invariant — the role table is read from the PRE-MERGE epoch**
(`lakko-security` ADR-0022 review, Finding 2; this is load-bearing fail-closed
correctness, not a nice-to-have). The gate reads `owner_pubkey` /
`admin_pubkeys` from the group's **pre-merge** authenticated `GroupContext`
(the `group_context_extension(0xFF02)` accessor *before* `merge_staged_commit`,
mirroring how `process_incoming_gated` reads `members()` pre-merge). Reading the
table from the *post-merge* epoch would let a single commit that *both*
self-promotes the committer to admin *and* adds/removes a member self-authorize
— the commit would rewrite the role table and then be judged against its own
rewrite. The committer's authorization is therefore always evaluated against the
epoch the commit targets, never the epoch it produces. A property test asserts
this directly: **a commit that both self-promotes and acts is refused, because
the committer is not yet an admin in the pre-commit role table.**

**No member self-leave in v1 (implementation finding).** The original draft had
a "self-Remove (leave)" row allowing a member to remove their own leaf. OpenMLS
0.8.1 forbids this at the protocol level (`CreateCommitError::CannotRemoveSelf`)
— a member cannot *commit* the removal of their own leaf, so a committer is
never inside their own commit's removed set, and the gate's self-Remove branch
was unreachable dead code (removed). Consequently v1 has **no member-initiated
self-leave**: a member leaves by an owner/admin committing the removal
(authorised normally), or — as a follow-up — a member-sent self-remove proposal
an admin commits. A property test (`admin_can_remove_member`) covers the
admin-removes-member path; the residual UX gap (a member cannot leave on their
own) is documented, not a security issue.

### Owner at creation (option A)

A conversation group is **role-enabled from creation**, with the creator as
owner: `Group::create` seeds the initial GroupContext with the `0xFF02` role
table (owner = creator, no admins) plus the required-capabilities declaration
that lets a later `set_group_roles` commit rewrite it. This **closes the
bootstrap window** — there is never a moment when a new group has no owner for a
malicious member to claim (the rejected alternatives were founder-gated first
opt-in, which needs a fragile "leaf 0 == creator" assumption, and
first-claimer-owns, which is racy). The maintainer chose option A over the
draft's "default-off + opt-in" precisely to avoid that ungated window.

Groups created **before** ADR-0022 carry no `0xFF02` table; the gate's "no role
table → open" branch preserves today's behavior for them (and for any legacy
group), so nothing regresses. Every leaf (KeyPackage + both group founders)
advertises the `0xFF02` capability via the unified `lakko_leaf_capabilities`, so
a role-enabled group can add any Lakko member and later rewrite its table. A 1×1
chat is a 2-member role-enabled group: the creator is owner, the other member is
a plain member (so only the creator can convert it to a larger group by adding —
an acceptable v1 behavior).

### Surfaced to the clients

FFI (uniffi + wasm): `setGroupRoles` / `groupRoles` /
`ownRole(groupIdHex)` / `processIncomingVerifyingRoles`. Clients (1) gate the
add / remove / promote affordances on the local member's role, (2) route every
group-journal receive through `processIncomingVerifyingRoles` against the
group's role table, and (3) add a role-management UI (promote / demote, shown
only to the owner). The gate is the security boundary; the UI gating is just UX
(a non-admin's unauthorized commit would be refused by peers anyway).

## Alternatives

1. **UI-only roles (track admins in an app-level store, gate only the buttons).**
   Rejected: provides *no* security — a patched or malicious client ignores the
   UI and commits anyway, and honest peers, having no gate, would merge it. The
   whole value of a role is that peers *enforce* it; a cosmetic gate is worse
   than nothing because it implies an authorization that does not exist.

2. **Founder-only (only the creator can add/remove; no admins, no delegation).**
   Rejected as the model, but it is the *degenerate case* of this design (admin
   set = ∅, owner-only). We pay almost no extra complexity to also carry an
   admin set, and a Telegram-class group needs co-admins and ownership transfer,
   so the general model wins. Founder-only would force a re-design the first time
   the owner wants a co-admin.

3. **Server-enforced roles (the DS checks the committer's role before
   journaling).** Rejected: the DS is opaque and zero-trust — it cannot parse a
   commit (invariant #2) and must not learn the group's role structure
   (invariant #5). It is also the wrong trust boundary: a compromised server
   could then forge authorizations.

4. **Capability tokens (the owner signs a short-lived "you may add" grant the
   admin presents with the commit), à la ADR-0015's attestation.** Considered
   and deferred. ADR-0015 needed an *auth-service* attestation because a device
   removal had no in-group signer to anchor on. Here the committer *is* an
   in-group member whom MLS already authenticates, so the admin set in the
   GroupContext is sufficient and simpler — no extra signer, no token TTL, no new
   service path. A grant model would matter only if we wanted *offline-verifiable
   delegation independent of group state*, which we do not for v1.

## Consequences

- **Positive:** a real, peer-enforced authorization model with no new server
  surface and no OpenMLS fork; reuses two audited primitives (ADR-0015's
  GroupContext-extension policy + refuse-to-apply gate); default-off keeps 1×1
  and legacy groups untouched; the role table is opaque to the server
  (invariants #2/#5 preserved) and propagates atomically via the Welcome;
  fail-closed (invariant #4).
- **Negative:** a `lakko-mls` state-machine change (invariant #3) — requires
  property tests + an external-audit touch; a new attacker-reachable parser (the
  `0xFF02` extension body) needs a fuzz target; enforcement is *peer-verified*,
  not cryptographically unforgeable against a colluding majority (a quorum of
  malicious members could agree among themselves to ignore the policy — the same
  residual as ADR-0015, and inherent to a leaderless protocol); v1 keeps admin
  powers deliberately narrow (admins cannot demote each other), which some flows
  may find limiting. **Owner device loss (v1 consequence, explicit):** because
  the owner is fixed at creation and v1 has neither ownership transfer nor
  admin-quorum recovery, a group whose owner permanently loses the account key
  becomes un-administrable (no one can promote/demote/transfer). **Ownership
  transfer has since been built** ([ADR-0045](0045-remote-commit-epoch-fork.md),
  `e780952`: `Group::transfer_ownership` / `propose_ownership_transfer` over
  `RoleTable::with_owner_transferred`), so a *present* owner can now hand the
  group on — but that does **not** relieve this consequence, because minting a
  transfer needs the owner's key. An owner who has already lost it cannot
  transfer, and admin-quorum recovery is still deferred. This is
  **substantially mitigated** by ADR-0017: the owner's account key is the
  phrase-derived `AccountIdentity`, so recovering on a new device from the
  recovery phrase restores the owner key (and thus owner powers) — the group is
  only orphaned if the owner loses *both* every device *and* the recovery phrase.
- **Neutral:** the role table grows the GroupContext by `32·(1+admin_count)`
  bytes; ~~ownership transfer +~~ the admin-can-manage-admins question ~~are~~ is
  an explicit follow-up (**ownership transfer landed** under
  [ADR-0045](0045-remote-commit-epoch-fork.md), and brought with it the
  commit-level never-orphan-the-owner rule indexed as **C-63**, which closes a
  hole that was reachable through `set_group_roles` from the day this ADR
  shipped); the clients gain a role-management surface that only the owner sees.

## Open questions

- **~~Ownership transfer.~~ BUILT** under
  [ADR-0045](0045-remote-commit-epoch-fork.md) (`e780952`), and it is exactly the
  shape this question guessed: an **owner-only commit that rewrites the owner
  slot**, riding the audited `set_group_roles` / `propose_group_roles` paths
  rather than being a new commit type, so it is judged by this ADR's own gate
  against the **pre-merge** table in which the transferor is still owner.
  `OutgoingOwner::{Admin, Member}` states what becomes of the departing owner
  instead of defaulting silently. It also forced a rule this ADR should have had:
  a role table names 32-byte keys, so `set_group_roles` could always install an
  owner that no leaf holds — permanently un-administrable, since a role change is
  owner-only and the owner's leaf is unremovable. `group_proposals::owner_survives`
  now refuses such a commit **on every peer** (**C-63**).
  **The second half of this question is still open:** what happens if the owner
  loses their device *before* transferring? Transfer does not answer it — minting
  one needs the owner's key. (Candidate: an admin-quorum recovery, still
  deferred.)
- **Can an admin manage other admins?** v1 says no (only the owner promotes /
  demotes). Some products let admins demote *members* but not *admins*; revisit
  after dogfooding.
- **Phase 2b per-device leaves (Finding 1 forward path).** When ADR-0007 moves
  conversation groups to per-device random leaves, the gate must resolve a
  committer's *device* leaf back to its *account* before the role lookup. The
  resolution mechanism (a DeviceGroup-membership check vs. embedding the account
  key in the device leaf credential) is deferred to that phase; v1 asserts the
  single-account-leaf model as a stated invariant.
- **Interaction with ADR-0015 attested removals on DeviceGroups.** Confirmed
  **orthogonal** by the `lakko-security` review: different group types
  (DeviceGroup `0xFF01` vs. conversation Group `0xFF02`), distinct extension
  IDs, no shared state — a group wanting both simply chains two independent
  gates.
- **Role changes vs. concurrent membership commits** is now resolved by the
  stated pre-merge-epoch invariant above (a just-demoted admin's in-flight
  commit is evaluated against the epoch it targets); the property test pins it.
- **lakko-security reviews: both done — APPROVE WITH NOTES.** The DESIGN review
  (above) and the **post-implementation** pass on the landed Sprint 1 code
  (crypto / MLS state-machine change, invariant #3). The post-code pass verified
  the pre-merge gate ordering against openmls 0.8.1, confirmed the 4 negative
  property tests fail via the gate's own refusal (not an incidental MLS error),
  and confirmed the decode parser is bounded + panic-free; all invariants
  (#2/#3/#4/#5) pass and the ADR-0015 DeviceGroup path does not regress. Two
  forward-looking notes recorded below for the client-wiring sprint.
- **~~SelfRemove is invisible to the gate~~ (defense-in-depth, post-code Finding 1)
  — ADDRESSED; that future sprint was [ADR-0045](0045-remote-commit-epoch-fork.md).**
  The finding stood: `authorise_role_commit` detected removals only via
  `staged.remove_proposals()`, which surfaces `Proposal::Remove` exclusively — a
  `Proposal::SelfRemove` (type `0x000a`) was not seen. It was benign only while
  lakko exposed no self-remove API, and this ADR recorded the trigger: *"if a
  future sprint makes 'leaving' a controlled action, the gate MUST add SelfRemove
  detection"*. ADR-0045 is that sprint — a member may now propose its own removal
  — and the branch is back: `group_proposals::classify` maps
  `Proposal::SelfRemove` to `ProposalKind::Leave`, `authorise` gives it an
  explicit arm (anyone may leave; **the owner may not, until they transfer**), and
  `commit_effects` folds a `SelfRemove` into the removed-leaf set by reading the
  **sender's** leaf, since the proposal carries no target of its own.
- **Call-path caveat for the client-wiring sprint (post-code Finding 2).** The
  multi-party **call** group is created via the same role-enabled `Group::create`
  as a chat group, so it now carries a `0xFF02` table (owner = the call
  originator). Clients MUST route call-group receives through the **open**
  `process_incoming`, NOT the verifying gate — a legitimate non-owner
  welcome-ferry / KP-forward add (ADR-0009) would otherwise be refused. Only the
  chat-message receive loop uses `process_incoming_verifying_roles`.
  PublisherGroup is unaffected (it uses `create_with_id`, which does not seed
  `0xFF02`).

## References

- [ADR-0015](0015-device-removal-authorization.md) — peer-verified attested
  removal: the GroupContext-extension policy + refuse-to-apply gate this design
  reuses.
- [ADR-0010](0010-channels-two-tier-broadcast.md) — channels' per-publisher
  Ed25519 model (identity-based, not role-based — the contrast that motivates a
  role table here).
- [ADR-0021](0021-sfu-peer-identity-exposure.md) — the contacts-keyed,
  signature-pubkey-based name resolution the role UI reuses.
- RFC 9420 (MLS) §7 (group context, extensions) — extensions are
  application-defined authenticated group state; §12.4 (commit) — commits are
  signed by the committer's leaf, which is what the gate authenticates against.
- `crates/lakko-mls/src/group.rs` `process_incoming_gated`,
  `crates/lakko-mls/src/device_group.rs`
  `process_incoming_verifying_removals` — the apply-path precedent.
- OpenMLS `ProcessedMessage::sender()` (`framing/validation.rs:316`) — the
  committer-exposure primitive that makes the gate feasible.
