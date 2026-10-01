<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0060: A conversation's kind is set in the group at creation, and a one-to-one keeps its two people

- **Status:** Accepted 2026-09-30 (the maintainer chose to refuse membership changes in a one-to-one on every device, and to keep deleting a one-to-one local only); S1 core, S2 FFI and S3 clients (web, iOS, Android) built 2026-10-01, see [What was built](#what-was-built-s1-s3). Not walked live on any platform
- **Date:** 2026-09-30
- **Context:** Phase 3 (product hardening). The maintainer's decision of
  2026-09-30: *"A one-to-one chat always has the same two people; a group can
  add and remove members."* This ADR records how that rule is carried and
  enforced.
- **Amends:** [ADR-0022](0022-group-admin-roles.md) (a `GroupContextExtensions`
  rewrite must carry one more extension verbatim; no role changes in a
  one-to-one), [ADR-0045](0045-remote-commit-epoch-fork.md) (one more
  commit-wide rule beside `owner_survives`),
  [ADR-0027](0027-external-join-approval.md) /
  [ADR-0028](0028-group-join-ticket.md) (no external join into a one-to-one).
  [ADR-0050](0050-per-device-conversation-leaves.md) is kept as it is: a
  person's own devices still come and go in any conversation.

## Context

Every conversation is created by the same path — `startGroupWith` on each
client, which calls `Group::create` (`crates/lakko-mls/src/group.rs`) and then
adds the other people. `Group::create` seeds two things into the
GroupContext: a `RequiredCapabilities` extension naming `0xFF02`, and the
`0xFF02` role table with the creator as owner (ADR-0022). Nothing in the group
says whether it is a one-to-one or a group.

So the clients guess. Each counts the people in the roster (accounts, not
leaves) and keeps a per-device flag:

- web: `apps/web/src/chat/conversation-kind.ts` + `conversation-kind-store.ts`;
- iOS: `apps/ios/Lakko/Chat/ConversationKindStore.swift` +
  `UI/Chat/ConversationTitle.swift`;
- Android: `apps/android/app/src/main/kotlin/me/lakko/app/chat/ConversationKind.kt`
  + `ui/chat/ConversationPresentation.kt`.

The flow that creates a conversation writes the flag (new group -> group,
contact accept -> chat). A device that first sees a conversation through a
Welcome has no flag and derives one from the count. This goes wrong in ways a
person sees:

1. **A two-person group reads as a one-to-one on everyone but its creator.**
   Seen live on web: the group "Dev club" with two members showed up on the
   second member's device as a one-to-one titled "Reader Two", its group name
   never applied, left out of the feed and of group suggestions.
2. **The three clients disagree on edge counts.** A people count of 1 derives
   `group` on web (`conversationKindOf` returns chat only for exactly 2) and
   `chat` on iOS and Android (`peopleCount > 2` decides group).
3. **The stores still flip chat -> group** when a third person is observed,
   which [conversation-info §2.1](../ux/conversation-info.md) (decided
   2026-09-23) already forbids: a one-to-one does not become a group by
   growing.
4. **Nothing stops a one-to-one from growing.** The rule in (3) is a UI
   promise. Any member's device that calls `propose_add_member` in a
   one-to-one — a modified client, a bug, an old build — turns it into a
   three-person conversation, and every other member's device accepts it,
   because the ADR-0022/0045 rules do not know the conversation is a
   one-to-one.

The kind is a fact about the conversation that every member must agree on and
that must not change. That is exactly what the GroupContext is for.

## Decision

**The kind is written into the group's GroupContext by the creator, is never
changed by any commit, and every member reads it from there. In a
one-to-one, every member's device refuses any commit that changes the set of
people in it. A person's own devices may still join and leave.**

### 1. The extension

A new custom GroupContext extension, **`0xFF03` "group kind"**, in the same
private-use range as `0xFF01` (DeviceGroup removal policy, ADR-0015) and
`0xFF02` (role table, ADR-0022). The number `0xFF03` was once proposed in
ADR-0050 for a LeafNode extension and dropped before anything shipped; no code
or wire uses it.

Body: exactly two bytes, `version(1) = 0x01 ‖ kind(1)`, where kind is
`0x01` one-to-one or `0x02` group. `0x00` and everything above `0x02` are
reserved. The codec (`crates/lakko-mls/src/group_kind.rs`,
`GroupKind::{OneToOne, Group}`) is bounded and fail-closed in the style of the
role-table codec: exact length first, then the version, then the kind; any
other input is an error, never a default. Fuzz target
`lakko-mls/group_kind_decode`, asserting no panic and that anything decoded
re-encodes to the identical two bytes.

`0xFF03` is added to:

- the `RequiredCapabilities` extension `Group::create` seeds (beside `0xFF02`),
  so a device whose KeyPackage does not advertise it cannot be added to a
  conversation at all;
- `lakko_leaf_capabilities` and the KeyPackage capabilities
  (`key_package.rs`), so every Lakko device advertises it.

This is a flag day: a KeyPackage minted before the change does not advertise
`0xFF03` and cannot be added to a new conversation. There is no production
data, so no migration and no dual path.

`GroupKind` is the core name, not `ConversationKind`, because
`lakko-ui-rules::ConversationKind` (`Chat` / `Group` / `Channel`) already
crosses the same FFI surfaces; the two describe different things (a
membership rule versus a list a row belongs to) and must not share a name.

### 2. Creation

`Group::create(identity, kind: GroupKind)` — the kind is a required argument,
so no caller can forget it. What each creator passes:

| Creator | Kind |
|---|---|
| Contact accept / "New chat" (`startGroupWith` for one person) | `OneToOne` |
| New-group flow (`startGroupWith` for the first pick, then adds) | `Group`, even while it holds two people |
| `/c/` call-link groups (`lakko-calls::link_invite::accept_call_invite`, `found_call`; the clients' multi-joiner call hosts, ADR-0032) | `Group` |
| Dev probe, tests | whatever the test is about |

Call-link groups get `Group` because their membership does grow (ADR-0032
admits joiners into one group); they stay out of the Chats and Groups lists
by the local bookkeeping that hides them today. A third kind for them was
considered and rejected (see Alternatives).

`Group::create_with_id` (the table-less base of `PublisherGroup`) and
`DeviceGroup` do not seed `0xFF03`; they are not conversations.

**The founding commit of a one-to-one.** The founder is alone in the group and
then adds, in ONE commit (ADR-0050), every device of the other person plus its
own other devices. That commit changes the set of people from one to two, so
the one-to-one rule below is stated so that it admits exactly this:

> **In a one-to-one, a commit may never remove a person, and may add people
> only up to two in total.** Formally: the accounts before the commit are a
> subset of the accounts after it, and there are at most two accounts after
> it.

With two people present this means the set of people never changes. With the
founder alone it admits one other person, with any number of that person's
device leaves. The founding commit fans out to nobody, so only the founder's
own local check ever sees it; every later commit is seen by the other person's
devices too.

### 3. Reading the kind

- `Group::group_kind() -> Option<GroupKind>`: reads `0xFF03` from the current
  GroupContext. `None` only for a group that carries no `0xFF03` at all
  (`PublisherGroup`, `DeviceGroup`).
- **Joining refuses a conversation without a readable kind.**
  `Group::join_from_welcome` refuses a Welcome whose GroupContext carries the
  `0xFF02` role table but no `0xFF03`, or an `0xFF03` body that does not
  decode. A conversation this device cannot classify is not joined, so no
  client ever has to guess again (invariant #4 shape: refuse rather than
  degrade).
- FFI: `GroupKind` enum and `Group.groupKind()` on UniFFI and wasm; the
  constructor becomes `Group(identity, kind)` on both. The NSE surface
  (`lakko-ffi-extension`) gets `Group.groupKind()` as a read, because a
  notification for a two-person group must be titled with the group, not with
  the sender as if it were a one-to-one — the same defect as the list, in the
  notification. It is a read of the GroupContext the NSE already restores;
  no compose and no write, so C-59 is untouched.

### 4. Immutability

No commit may change or remove `0xFF03`, in either kind:

- **In a one-to-one no `GroupContextExtensions` proposal is admitted at all**
  (section 5 refuses role changes), so the extension cannot be touched there.
- **In a group**, `set_group_roles`, `propose_group_roles`,
  `transfer_ownership` and `propose_ownership_transfer` rewrite the whole
  GroupContext extension list to change `0xFF02`. Each must copy the current
  `0xFF03` extension (and the `RequiredCapabilities` extension) into the new
  list verbatim. One helper builds that list, so the four sites cannot drift.
- **The peer commit gate checks it.** `commit_effects` records, for a
  `GroupContextExtensions` proposal, whether the `0xFF03` body it would
  install is byte-identical to the pre-merge one. A commit that changes it,
  drops it, or drops `0xFF03` from `RequiredCapabilities` is refused on every
  peer (`RefusalReason::GroupKindChanged`), before merge, the same way
  `owner_survives` refuses. The receipt gate refuses to store such a proposal,
  and the sweep filter never includes one.

### 5. The one-to-one rule

**One commit-wide rule, in the style of `owner_survives` (C-63):**
`group_proposals::one_to_one_holds(effects, members, kind)` — for a
one-to-one, the accounts in the roster the commit produces (pre-merge members,
minus every removed leaf, plus every added leaf's account, ADR-0050) must
contain every pre-merge account and number at most two. For a group it always
passes. It needs the added and removed leaves `CommitEffects` already
collects; nothing new has to be summarised except the kind check of section 4.

It is enforced where `owner_survives` is:

- **the peer commit gate** in `process_incoming_verifying_roles`: a commit that
  breaks it is refused, not merged, on every member's device
  (`RefusalReason::OneToOneMembershipFixed`);
- **the sweep**: each trial commit of `stage_pending_proposals` and
  `narrow_sweep_by_exclusion` re-asks it, so a committer never builds a commit
  its peer would refuse (the ADR-0045 lesson: a local freeze is better than a
  fork);
- **the direct-commit paths** (`add_member`, `add_members`,
  `add_members_from_wire`, `remove_member`): a local courtesy refusal before
  the commit is built. This is also the check that admits the founding
  commit.

**Early per-proposal refusals**, so a request fails at the proposer and never
sits in anyone's pending set. `authorise` gains the group's kind and, in a
one-to-one, refuses:

| Proposal | In a one-to-one |
|---|---|
| `Add` of a leaf whose account is already in the group | allowed as today: a person's own new device, or re-admission of the other person's new device (both ADR-0050 arms) |
| `Add` of a leaf of any other account | refused, `OneToOneMembershipFixed` |
| `Remove` of a leaf whose account keeps another leaf | allowed as today (retiring a device) |
| `Remove` of an account's last leaf | refused, `OneToOneMembershipFixed` |
| `Leave` (`SelfRemove`) from an account's last leaf | refused, `OneToOneMembershipFixed` |
| `Leave` from a leaf whose account keeps another | allowed (a device retiring itself) |
| `RoleChange` (role table rewrite, including ownership transfer) | refused, `OneToOneHasNoRoles` |

These run at the proposer (`authorise_own_proposal`) and at the receipt gate
(`gate_incoming_proposal`), exactly where ADR-0045 already runs `authorise`.
A per-proposal rule cannot see two proposals together (two `Remove`s of the
same account's two leaves are each fine alone), which is why the commit-wide
rule is the authority and these are the early answer.

**Why no role changes in a one-to-one.** The creator is the owner and the
other person a plain member; that is enough for every operation a one-to-one
has. Push routing (ADR-0042) is minted by the owner and never rotates on a
shrink that cannot happen; a person's own devices are admitted through the
ADR-0050 self-service arm, which needs no role; re-admission of the other
person's new device is proposed and swept by that person's own device. An
admin or an ownership transfer in a two-person conversation changes nothing a
person can see, and refusing it keeps the one-to-one's GroupContext frozen
from creation (section 4).

**Leave.** A person cannot leave a one-to-one at the protocol level: that
would be removing a person, and the other side would be left in a
"conversation" of one. What a person does instead is the existing local
`Delete this chat` ([conversation-info](../ux/conversation-info.md)): this
device stops reading the conversation and forgets it; the other person is not
told and can keep writing into it. See Open questions.

**External join.** A `/g/` link can only ever name a group id, so a stranger
can knock on a one-to-one's journal. In a one-to-one:

- `Group::approve_external_join` refuses with `OneToOneMembershipFixed`;
- an incoming external-join proposal is surfaced as
  `Incoming::ExternalJoinProposal { retained: false }` and not kept — the
  shape a full quarantine already has, so the knock is consumed and nothing
  is offered (the person card has no pending-requests section);
- clients never mint a `/g/` link for a one-to-one (they already do not).

### 6. New refusal reasons

Appended to `RefusalReason` (and to the flat `FfiError` variants, at the end,
because uniffi decodes by ordinal):

| Reason | Disposition |
|---|---|
| `OneToOneMembershipFixed` | `NotPermitted` |
| `OneToOneHasNoRoles` | `NotPermitted` |
| `GroupKindChanged` | `DoNotRetry` |

### 7. Client consequences

- The kind is read from the group (`groupKind()`), on every device, the same
  answer everywhere. The three conversation-kind stores, their
  "chat -> group when a third person is observed" flip, and the count-based
  derivation are deleted. The people count stays for display only ("3
  people").
- The Chats and Groups lists split on `groupKind()`. A two-person group is in
  Groups on every member's device, titled by its group name.
- The `lakko-ui-rules` info rules already key on the conversation kind; each
  client now feeds them the kind from the group instead of from its store, so
  a one-to-one opens the person and a group opens the group everywhere.
- A group-name envelope (ADR-0051) arriving in a one-to-one is dropped on
  receipt: a one-to-one has no group name, and applying one would retitle the
  person.
- Creation passes the kind (section 2). The new-group flow keeps creating a
  `Group` with its first pick and adding the rest.
- Call-link groups stay hidden from the lists as they are today.

### 8. Tests

- `group_kind.rs` unit tests: round trip; every wrong length, version and kind
  byte refused; plus the `group_kind_decode` fuzz target.
- A new integration binary `crates/lakko-mls/tests/one_to_one.rs`:
  - the founding commit of a one-to-one admits the other person with several
    device leaves and the founder's own other devices;
  - a founding commit naming two other accounts is refused locally;
  - after founding, adding a third account is refused at the proposer, at the
    receipt gate, by the sweep, and at the peer commit gate (a commit built
    without the local checks, through a test-only path, is refused and not
    merged by the other member);
  - own-device admit and retire, and re-admission of the other person's new
    device, still work;
  - removing or `SelfRemove`-ing an account's last leaf is refused; two
    `Remove`s of one account's two leaves in one commit are refused by the
    commit-wide rule though each passes alone;
  - a role change and an ownership transfer are refused;
  - external-join approve is refused; an incoming external-join proposal is
    `retained: false`.
- In `group_admin_roles.rs` / `group_ownership_transfer.rs`: a role change
  and an ownership transfer in a `Group` keep `0xFF03` byte-identical; a
  hand-built `GroupContextExtensions` commit that changes or drops `0xFF03` is
  refused on the peer.
- `join_from_welcome` refuses a conversation Welcome with `0xFF02` and no
  `0xFF03`, and one with an undecodable `0xFF03`.
- The existing `lakko-mls` suite runs with `GroupKind::Group` wherever it
  creates a group and must stay green: the group rules are unchanged.
- Clients: the list split and the info target driven by `groupKind()`; the
  group-name drop in a one-to-one. On web and iOS through the boundary; on
  Android the rule is asserted in Rust and the client tests cover the wiring
  (the host JVM suite has no native library).

## Alternatives

- **Keep deriving the kind on each device (people count plus a local flag).**
  Rejected: it is what produced the live defect. A device that joins by
  Welcome has no way to learn what the creator meant, and three hand-written
  derivations already disagree on a count of 1.
- **Carry the kind in an application message** (an envelope like the ADR-0051
  group name). Rejected: a message can be missed, arrives after the Welcome,
  needs an ordering rule, and anyone who can send one can send a different
  one. The GroupContext is agreed on by every member by construction and is
  in the Welcome.
- **Infer "one-to-one" from `RequiredCapabilities` or from the role table's
  shape** (for example: no admins and two accounts). Rejected: a two-person
  group has the same shape; that ambiguity is the bug.
- **Enforce the one-to-one rule only in the UI.** Rejected: the maintainer's
  decision is that every member's device refuses, and a UI promise is broken
  by any client that calls `propose_add_member`.
- **Only per-proposal refusals, no commit-wide rule.** Rejected: two
  individually harmless proposals (removing two leaves of one account) can
  remove a person together; `owner_survives` exists for the same reason.
- **Allow `Leave` in a one-to-one.** Rejected with the decision: it removes a
  person, and leaves the other in a conversation of one that the UI would
  have to explain. See Open questions for the product side.
- **A third kind for call-link groups.** Rejected: the extension carries a
  membership rule, and a call-link group follows the group rule. Hiding them
  from the lists is a presentation question the clients already answer.
- **Keep `0xFF03` out of `RequiredCapabilities`.** Rejected: then a device
  that does not understand the kind could be added and would not enforce the
  one-to-one rule, which is exactly the check the maintainer wants on every
  member's device.

## Consequences

- **Positive:** the kind is the same on every device and every client, from
  the moment a device joins; the "Dev club" class of defect cannot happen.
  A one-to-one cannot be turned into a group by any single device. The three
  kind stores and their flip rule are deleted.
- **Negative:** a person cannot leave a one-to-one; they can only delete it
  locally, and the other side keeps a conversation that nobody reads. A
  one-to-one whose other person has deleted their account stays a
  conversation with a person who never answers. Flag day on KeyPackages
  (section 1). The external-review surface (G-39) grows by one extension,
  one commit-wide rule and one GroupContext-rewrite discipline.
- **Neutral:** groups behave exactly as before. The server sees nothing new:
  `0xFF03` is inside the MLS GroupContext, which the DS never parses.
- **Invariants:** #3 directly — a new GroupContext extension, a new
  commit-wide rule at the peer commit gate and the sweep, a new condition on
  `join_from_welcome` (this ADR, the property tests of section 8 and the fuzz
  target are the per-change half; the external review is G-39 per
  [ADR-0046](0046-external-crypto-review.md)). #4 — every new decision point
  refuses: an unreadable kind is not joined, a commit that changes the kind or
  the people of a one-to-one is not merged. #2 and #5 untouched.
- **Invariant index:** propose **C-70** — *a conversation's kind is fixed in
  its GroupContext at creation and no commit changes it; in a one-to-one no
  commit changes the set of accounts beyond admitting the second person at
  founding.* Enforced at the receipt gate, the sweep, the peer commit gate and
  `join_from_welcome`; added to `docs/security/invariant-index.md` and
  `docs/security/calls.md` as C-70 with the core.

## Plan

- **S1 core:** `group_kind.rs` codec + fuzz; `Group::create(identity, kind)`;
  `0xFF03` in required and advertised capabilities; `group_kind()`;
  `join_from_welcome` check; the GroupContext-list helper used by the four
  role rewrites; `one_to_one_holds` and the kind check at the three gates;
  the per-proposal refusals; the three refusal reasons; the tests of
  section 8.
- **S2 FFI:** UniFFI + wasm (`GroupKind`, constructor, `groupKind()`, the
  appended `FfiError` variants); the NSE read; bindings regenerated.
- **S3 clients (web, iOS, Android together):** pass the kind at every creation
  site; read it for the lists, the info target and the notification title;
  delete the kind stores and the count derivation; drop a group-name envelope
  in a one-to-one.
- **S4:** docs reconcile (CLAUDE.md row, C-70 in the index, PRODUCT-READINESS),
  and a two-device walk: a two-person group shows as a group on both.

## What was built (S1-S3)

Built 2026-10-01 in one change (`feat(mls): record the conversation kind in
the group`). Unit- and integration-tested; **not walked live on any
platform**, so the two-device walk of S4 is still open.

- **S1 core** (`crates/lakko-mls`): `group_kind.rs` (`GroupKind { OneToOne,
  Group }`, the `0xFF03` body `version 0x01 ‖ kind 0x01/0x02`, exact length,
  then version, then kind, fail-closed); `Group::create(identity, kind)` and
  `Group::group_kind()`; `0xFF03` in the leaf capabilities and in the seeded
  `RequiredCapabilities`; `join_from_welcome` refuses a Welcome carrying
  `0xFF02` without a readable `0xFF03`, before the group is built;
  `role_rewrite_extensions` carries the extension list verbatim through
  `set_group_roles`, `propose_group_roles` and both transfer paths;
  `group_kind_unchanged` at the peer commit gate and
  `proposal_keeps_group_kind` at receipt and in the sweep; `one_to_one_holds`
  (pre-merge accounts a subset of post-commit accounts, at most two, and any
  `GroupContextExtensions` rewrite refused in a one-to-one) at the peer commit
  gate, in the sweep (`selection_is_admissible`) and on `add_members` /
  `remove_member`; the early per-proposal refusals as `one_to_one_allows` at
  the proposer and the receipt gate; an external join into a one-to-one is
  refused on approve and surfaced with `retained: false`. `RefusalReason`
  appended `OneToOneMembershipFixed` (`NotPermitted`), `OneToOneHasNoRoles`
  (`NotPermitted`), `GroupKindChanged` (`DoNotRetry`). Tests: the lib (215)
  and the integration binaries, among them the new `one_to_one` (10); fuzz
  target `group_kind_decode` over `fuzz_api::decode_group_kind`, built but not
  run (cargo-fuzz was not installed).
- **S2 FFI:** UniFFI `GroupKind`, `Group.create(identity, kind)`,
  `groupKind()`, three `FfiError` variants and three refusal-reason values
  appended; wasm `enum GroupKind { OneToOne = 1, Group = 2 }`,
  `new Group(identity, kind)`, `groupKind()`, refusals through
  `refusalReason`; the NSE surface (`lakko-ffi-extension`) a read-only
  `groupKind()`. It has no caller on iOS yet: the NSE never titles by sender,
  and call rings are titled from `CallPeerNameMirror`, written from the app's
  conversation title, which already reads the kind.
- **S3 clients** (web, iOS, Android): the kind is passed at every creation
  site and read wherever chat versus group was decided — the lists, the info
  target, titles and the one-to-one peer lookup, the shared-pin authority
  (ADR-0052: in a two-person *group* only an owner or admin pins for
  everyone), typing wording, and call routing. **Behaviour change:** a group
  of any size now calls over the SFU, so a two-person group no longer takes
  the one-to-one call path. A group-name envelope received in a one-to-one is
  dropped. The local kind stores, the count derivation and the chat -> group
  flip are deleted. A group whose kind reads as absent is treated as a group.

**Where the build differs from the text above:**

- The per-proposal refusals are a separate function (`one_to_one_allows`)
  called at the proposer and the receipt gate, not a new argument to
  `authorise`.
- In a one-to-one **any** `GroupContextExtensions` rewrite is refused by the
  commit-wide rule, not only a role change, so the kind and the role table are
  both frozen there by one check.
- A Welcome without a readable kind is refused with a plain `Error::Group`,
  not a typed `RefusalReason`; it is not a membership request anyone could
  retry.
- When the full pending selection breaks a commit-wide rule, the sweep
  narrows before building rather than building and discarding.

## Decided (maintainer, 2026-09-30)

- **Leaving a one-to-one** stays a local delete: the other person is not
  told. A "this person left" signal is not built; nothing here blocks adding
  it later.

## References

- [ADR-0022](0022-group-admin-roles.md) — role table `0xFF02`, the GroupContext
  extension precedent.
- [ADR-0045](0045-remote-commit-epoch-fork.md) — proposal gates, the sweep,
  `owner_survives` (C-63).
- [ADR-0050](0050-per-device-conversation-leaves.md) — per-device leaves, the
  own-device and re-admission arms kept here.
- [ADR-0027](0027-external-join-approval.md),
  [ADR-0028](0028-group-join-ticket.md) — external join.
- [ADR-0051](0051-group-and-channel-names.md) — group names.
- [`docs/ux/conversation-info.md`](../ux/conversation-info.md) §2.1,
  [`docs/ux/client-redesign.md`](../ux/client-redesign.md) §3, §5.1.
- RFC 9420 — the GroupContextExtensions proposal and the required_capabilities extension.
