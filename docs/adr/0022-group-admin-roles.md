<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0022: Group-admin roles — owner / admin / member, peer-verified

- **Status:** Proposed — design reviewed by `lakko-security`: **APPROVE WITH
  NOTES** (2026-06-17). Findings 1 (single-account-leaf assumption + Phase-2b
  path), 2 (pre-merge epoch read as a stated invariant), 3 (owner device loss +
  ADR-0017 mitigation), 4 (version fail-closed), 5 (self-Remove test) folded
  into the text below. Awaiting maintainer acceptance; the landed code still
  requires the mandatory post-implementation `lakko-security` pass (invariant #3).
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
| Self-Remove (leave) | the leaving member themselves |
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

**Self-Remove ("leave") is distinguished by leaf identity**: the gate compares
the committer's leaf to the removed leaf and allows the removal only when they
are the same (a member may remove *only their own* leaf, never another's, when
not owner/admin). A property test asserts a non-admin member cannot remove any
leaf but their own.

### Default-off + opt-in

If the extension is absent (every group created before this lands, and 1×1
chats), the posture stays **open** — any member may add/remove, today's
behavior — so nothing regresses. A group gains role enforcement when the owner
enables it via a GroupContextExtensions commit
(`set_group_roles`), the same opt-in shape as ADR-0015's
`set_require_attested_removals`. Device KeyPackages + group founders advertise
support for the `0xFF02` extension type via leaf capabilities so the commit is
accepted by conformant members.

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
  becomes un-administrable (no one can promote/demote/transfer). This is
  **substantially mitigated** by ADR-0017: the owner's account key is the
  phrase-derived `AccountIdentity`, so recovering on a new device from the
  recovery phrase restores the owner key (and thus owner powers) — the group is
  only orphaned if the owner loses *both* every device *and* the recovery phrase.
- **Neutral:** the role table grows the GroupContext by `32·(1+admin_count)`
  bytes; ownership transfer + the admin-can-manage-admins question are explicit
  follow-ups; the clients gain a role-management surface that only the owner
  sees.

## Open questions

- **Ownership transfer.** v1 has none (the owner is fixed at creation). Likely a
  future owner-only commit that rewrites `owner_pubkey`. What happens if the
  owner loses their device before transferring? (Candidate: an admin-quorum
  recovery, deferred.)
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
- **lakko-security DESIGN review: done** (APPROVE WITH NOTES, above). The
  mandatory **post-implementation** `lakko-security` pass on the landed code
  remains required before Sprint 1 merges (crypto / MLS state-machine change,
  invariant #3), together with the `0xFF02` fuzz target + the gate property
  tests.

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
