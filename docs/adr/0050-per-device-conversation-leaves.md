<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0050: Per-device conversation leaves — a second device is a full client

- Status: Accepted (2026-09-15)
- Date: 2026-09-15
- Extends: [ADR-0007](./0007-multi-device-identity.md) (builds its Phase 2b),
  [ADR-0022](./0022-group-admin-roles.md), [ADR-0023](./0023-message-edit-delete-reactions.md),
  [ADR-0045](./0045-remote-commit-epoch-fork.md), [ADR-0049](./0049-identity-recovery-word-vault.md)
- Closes: G-40 (`docs/security/known-gaps.md`)
- UX spec: [`docs/ux/second-device.md`](../ux/second-device.md)
- Security: touches `lakko-mls` (invariant #3: this ADR + property tests; the
  G-39 external review scope grows by this surface). Flag for `lakko-security`
  before merge.

## 1. Context

The product-readiness audit of 2026-09-15 (`docs/PRODUCT-READINESS.md`) found
that a second device never receives the account's conversations. The maintainer
set the standard plainly: multi-device means more than one device, and a new
device must have chats, groups, channels and calls.

The cause is that conversation groups are still on the ADR-0007 **Phase 1**
model. Every real leaf today is the AccountIdentity derived from the phrase
seed, carrying an empty `BasicCredential` (`crates/lakko-mls/src/identity.rs:231`).
RFC 9420 section 5.3 requires distinct leaf signature keys within a group and
OpenMLS enforces it (`validate_key_uniqueness`), so a second device holding the
same account key cannot be added. Discovery stores one KeyPackage per account
with no consume-on-fetch (`services/discovery/src/keypackage.rs:138-141`), so
two devices overwrite each other's row and Welcomes land on the wrong device.
Pairing hands over the DeviceGroup, the phrase seed and a history bundle whose
rows go into a message log no chat-list entry ever reads
(`apps/web/src/devices/history-bundle.ts:157`, `apps/web/src/chat/ChatList.tsx:247`).
Contacts and channel subscriptions live in one browser's `localStorage` and
travel only through a backup that runs at signup and recover.

ADR-0007 already named the fix and built half of it: the per-device random
`DeviceIdentity` exists on every client and is the leaf in the DeviceGroup
(`apps/web/src/auth/device-identity-store.ts:5-26`). Its doc comment says it is
also meant to be the leaf "in every conversation group the user joins from this
device". That sentence is what this ADR builds.

Facts this design rests on, verified in code on 2026-09-15:

- OpenMLS 0.8.1 is not vendored and validates no credential content. It does
  carry `Extension::Unknown` in a LeafNode when the leaf's capabilities list it
  (`openmls-0.8.1/src/extensions/mod.rs:146-158`), and
  `KeyPackageBuilder::leaf_node_extensions` exists but Lakko never calls it.
- The single leaf-to-identity resolution in the core is `pubkey_at_leaf`
  (`crates/lakko-mls/src/group_proposals.rs:208-214`), consumed by the receipt
  gate, the commit gate and own-proposal authorisation.
- The role table already stores 32-byte Ed25519 keys and, because Phase 1
  leaves are account keys, those stored keys already **are** account keys. The
  `0xFF02` wire format does not change; only who writes it changes.
- The DS mailbox is already per 32-byte pseudonym derived from a leaf signature
  key (`crates/lakko-contacts/src/pseudonym.rs:52-57`); the ADR-0045 sweep
  already fans one Welcome out per added leaf key (`apps/web/src/chat/sweep.ts:108-116`);
  the ADR-0027 rendezvous is already tagged by leaf key. DS, auth, push-gw and
  the KT log need no change.
- The account seed is present on every device after pairing, confirmation or
  any recovery route (ADR-0017/0018/0049), so every device can produce a
  signature by the account key.

## 2. Decision

### 2.1 Two tiers, stated once

| Tier | Key | Where it appears |
|---|---|---|
| **Account** | Ed25519 from the phrase seed | discovery identity bundle, KT log, contact pin, safety number, supersession proofs, the role table, message authorship, read/typing state, the DeviceGroup id |
| **Device** | Ed25519 per install, random | every MLS leaf: the DeviceGroup (as today) and, from this ADR, every conversation group; the DS mailbox for Welcomes; push pseudonyms |

The account key never sits in a conversation leaf again. Every leaf carries a
signed binding to its account key, and everything that used to key on "the
member's signature key" keys on the **resolved account key** instead.

### 2.2 The device binding

A fixed-layout blob, domain `lakko/device-binding/v1`, in `lakko-mls`
(`device_binding.rs`):

```
version(1)=0x01 ‖ account_pubkey(32) ‖ device_pubkey(32) ‖ issued_at_unix_secs(8 BE) ‖ sig(64)
```

`sig` is Ed25519 by the account key over `domain ‖ version ‖ account_pubkey ‖
device_pubkey ‖ issued_at`. It rides as the leaf's **`BasicCredential`
identity bytes** on every KeyPackage the device mints and on the own leaf of
every group it creates. OpenMLS signs the credential as part of the leaf,
carries it verbatim and never interprets it, and — unlike a LeafNode
extension — exposes it wherever Lakko needs it without exporting the ratchet
tree: `Member.credential` on the roster and `KeyPackageIn::unverified_credential`
on a wire KeyPackage before validation. (The ADR was first drafted with a
`0xFF03` LeafNode extension; that was dropped during S1 because openmls 0.8.1
gives no public path from a member to its leaf node's extensions, so the
credential is the carrier and no capability change is needed.)

A consequence: the leaf credential's identity bytes are now the binding slot
and nothing else. An **empty** credential means a Phase 1 leaf whose key is the
account key; `Identity::generate(name)` no longer carries the name (nothing ever
read it).

The binding is **verified by every peer, in Lakko code, at every gate that
admits or authorises a leaf**, because OpenMLS checks nothing in it:

- `add_member_from_wire` / `add_member` (the direct adder),
- `propose_add_member*` (the proposer) and `retain_external_join` (the
  quarantine, which surfaces the request as `retained: false`),
- `gate_incoming_proposal` and `authorise_role_commit` (every peer, on receipt),
- `sweep_keeps` (the sweep filter),
- `join_from_welcome` (the joiner refuses a roster holding an unresolvable leaf).

A leaf whose credential is non-empty but malformed, names a `device_pubkey`
different from the leaf's own signature key, or fails the signature is
**refused** (`RefusalReason::DeviceBindingInvalid`: dropped proposal, refused
commit, uncommittable Add). Fail closed, invariant #4.

The binding proves "this device key was authorised by this account key". It
does not prove the account key belongs to a person the receiver knows; that is
the receiver's existing contact pin (ADR-0016), applied to
`binding.account_pubkey` exactly as it is applied to a Phase 1 leaf today.

Revocation is group membership: removing a device removes its leaves. A binding
carries `issued_at` so a client can show it and so a future policy can bound it;
this ADR sets no expiry.

Considered and rejected for the binding carrier: the ADR-0007 sketch of a
DeviceGroup-membership lookup. It is unavailable to a non-member (the
call-admission dedup and a contact adding a device need to resolve a KeyPackage
before any shared group exists), it would require publishing the DeviceGroup
roster or an exporter, and it makes every verification depend on a second
group's epoch. A signature by the account key is self-contained, offline
verifiable, and reuses the `lakko-attest` fixed-layout template.

Considered and rejected: a `0xFF03` LeafNode extension (see above — not
reachable from the roster API) and a separate signed roster message (a second
artefact to deliver and to keep consistent with the leaf).

### 2.3 The resolver replaces the leaf key as member identity

`account_key_of_credential(identity_bytes, leaf_key) -> Option<[u8; 32]>`: the
binding's `account_pubkey` when the credential carries a valid binding for this
leaf; the leaf's own signature key when the credential is empty; `None`
otherwise. The
second arm is what lets a Phase 1 account-key leaf and Phase 2b device leaves
coexist in one roster while existing test-stack groups are still alive, and it
costs nothing once every leaf is bound. It is the only place the legacy rule
lives.

Every consumer moves to it:

| Site (core) | Today | After |
|---|---|---|
| `group_proposals::pubkey_at_leaf` | leaf signature key | `account_key_at_leaf`; `Proposer` carries `account_key` |
| `RoleTable` lookups, `committer_rank`, `owner_survives` | keyed on the value in the table, fed a leaf key | fed the resolved account key; **wire format unchanged** |
| `Group::create` | owner slot = creator's leaf key | owner slot = creator's **account key** (from the creator's own binding) |
| `own_role`, `own_committer_rank`, `plan_ownership_transfer` | `identity.signature_public_key()` | the device's account key |
| `MemberRecord` | `(leaf_index, signature_public_key)` | plus `account_key` |
| `Incoming::Application { sender_leaf }` | leaf only | plus `sender_account`; the leaf stays (SFrame keys on it) |
| `authorise` self-remove / `Leave` | "target leaf == my leaf" | "target leaf resolves to my account" (see 2.4) |
| external-join quarantine | leaf key | **unchanged, per leaf** (S1 finding: each device needs its own leaf, so a second device of a member account asking to join is a real request, not a duplicate); a leaf whose binding does not resolve is never retained; the `WelcomeRendezvous` tag stays the leaf key, since the Welcome is sealed to that leaf |
| `is_owner_response` reserve, `MAX_ADMINS`, `MAX_PENDING_*` | per leaf | per account |
| `key_package::signature_key` (ADR-0032 call dedup) | leaf key | new `key_package::account_key` reads the binding; the call dedup keys on it |

`committer_rank` ranks accounts; when one account holds several leaves the
device with the lexicographically lowest leaf signature key is the account's
acting candidate, so two devices of one admin do not both sweep. Beacon
observations are folded per account before ranking.

### 2.4 Who may add and remove a device

ADR-0022's rule stands for **other** accounts: adding or removing another
person needs owner or admin. Two self-service arms are added to `authorise`,
and they are the whole reason a second device can join without an admin:

- **Add:** a member may propose an Add whose leaf's binding resolves to **its own
  account**, or to an account that is **already a member** of the group. The
  first is "add my new phone"; the second is "my contact lost every device and
  restored; their new device is bound to the same account key I already trust",
  which is how standard 2 of the readiness document (contacts re-admit a
  restored user) happens with no button.
- **Remove:** a member may propose a Remove whose target leaf resolves to its
  own account. "Leave group" from a device proposes the removal of **every**
  leaf of its account; `CommitEffects.removed_leaves` and `owner_survives` agree
  on that set. `owner_survives` becomes "some surviving or added leaf resolves
  to the owner account".

A cap `MAX_LEAVES_PER_ACCOUNT = 8` is enforced at the proposer, at the receipt
gate and in `sweep_keeps`, counted over the account's leaves in the roster plus
its **distinct** pending device keys, so the self-service Add cannot be used to
fill a group and N proposals naming one device count as one.

### 2.5 Who sweeps a self-service proposal

Self-service proposals ride the ADR-0045 proposal path (a direct commit would
reopen the fork the proposal path closed). Liveness: with only owner and admins
as committers, a user's second device would join a 1x1 chat only when the
**other** person is online. So the acting-committer verdict gains one rule: when
the pending set consists solely of self-service proposals of one account, the
lowest-leaf online device of **that account** ranks immediately after the last
admin. The device that proposed usually sweeps its own proposal on its next
poll. The stand-down re-read and the staged-sweep rollback apply unchanged; the
race class is the one ADR-0045 already handles.

### 2.6 KeyPackages per device

Discovery stores one KeyPackage per `(account_id, device_id)` where `device_id`
is the device leaf's 32-byte signature key (migration in `services/discovery`;
PK `(account_id, device_id)`, index on `account_id`). Upload stays bearer-bound
to the account and PoW-gated; the request gains `device_id`; the handler caps
distinct devices per account at `MAX_LEAVES_PER_ACCOUNT` and refuses beyond it.
`GET /discovery/keypackage/{account_id}` returns the list
`{ keypackages: [{ device_id, keypackage }] }`. `DELETE
/discovery/keypackage/{device_id}` (bearer) removes one device's row. The
server never parses the bytes; the peer verifies the binding. The three
hand-mirrored wire types change in the same commit.

Each device publishes its own KeyPackage on sign-in, after every consumed
Welcome (as today), and after obtaining the account seed on a fresh device.

Creating a chat or adding a person adds **every device** of that account: the
adder fetches the list and proposes one Add per KeyPackage. Creating a chat
also adds the creator's **own other devices** the same way. A device that has no
published KeyPackage at that moment is picked up later by the re-admission arm
of 2.4 when any member next observes it in the list (the client re-checks a
contact's device list on a bounded schedule and on membership-related events).

**A founder adds every device in ONE commit** (`Group::add_members`, built
2026-09-15 after the first live test). The founder is the only member when it
adds, so its commits fan out to nobody and are dropped; with one direct commit
per leaf, every leaf but the last held a Welcome for an epoch the founder had
already advanced past, with the advancing commit on no journal, and was
stranded there. Observed on `test.lakko.me`: the first-added device stuck at
epoch 1, every peer at epoch 2, every row `WrongEpoch`. One commit gives every
added leaf a Welcome for the same epoch; the one Welcome carries every leaf's
secrets and goes, byte-identical, to each leaf's mailbox. Pinned by
`a_persons_devices_added_in_one_commit_land_at_one_epoch_and_both_read_the_founder`.

**The published KeyPackage is a last-resort package** (RFC 9420 section 10,
`last_resort` extension; `Identity::create_key_package` marks it and the leaf
capabilities advertise it). Found while building 2.8: OpenMLS deletes a
one-time package's private init key when a Welcome built on it is opened, and
discovery serves one package per device, so admitting one device to N chats
from one fetch — the pairing fan-out, a contact's re-admission — produced N
Welcomes of which only the first could be opened. A last-resort package keeps
its init key, so every Welcome sealed to it opens; pinned by
`one_published_key_package_opens_a_welcome_per_group`. Cost, stated: the init
key survives in the identity's storage until that storage is wiped, so a later
compromise of the device opens any captured Welcome sealed to it — the
forward-secrecy property of one-time packages is given up for the device's
join into groups it has since left. Clients still republish after every
consumed Welcome, so later adds use a fresher key. Two follow-ups, unbuilt: a
batch of one-time packages per device consumed on fetch, with the last-resort
package as the fallback (the Signal one-time / signed prekey shape), and
pruning of superseded last-resort bundles from the provider.

Metadata cost, stated: the count of devices per account becomes readable by
anyone who knows the `account_id`, as the single row was before. No names, no
platform, no timestamps beyond `updated_at`. Recorded in section 6.

### 2.7 Welcomes and mailboxes

A device polls two DS mailboxes: the **account** mailbox
(`IdentityPseudonym(account_key)`) for contact requests, acks and legacy
Welcomes, and its **device** mailbox (`IdentityPseudonym(device_key)`) for
Welcomes to its own leaf. The sweep already addresses Welcomes by added leaf
key; nothing changes there. Contact requests stay sealed to the account X25519
key, so any device can accept one; the accepted contact then syncs (2.9).

### 2.8 Pairing and confirmation: the fan-out

`runScanFlow` serves both QR pairing and the ADR-0049 confirmation prompt, so
one hook covers both. Order:

1. New device: generate `DeviceIdentity`; obtain the account seed (pairing
   recovery row, or a vault / phrase / passkey route); mint its binding;
   publish its conversation KeyPackage to discovery; join the DeviceGroup.
2. Old device, after `addDeviceFromWire` and the DeviceGroup Welcome: read its
   `knownGroupIds`; fetch the new device's KeyPackage from discovery (poll with a
   bounded wait, since step 1 races); for each group propose a self-service
   Add; sweep where it is the acting committer.
3. New device: receive one Welcome per group in its device mailbox, join,
   persist; the history bundle (2.9) now lands under real groups; reconcile the
   durable cursor so the bundled tail is not re-fetched.

The user sees one label, "Synchronizing...", in the chat list while any
self-service Add for this device is pending; nothing per chat. See the UX spec.

### 2.9 What else must follow the device

The pairing bundle becomes `StateBundle v2` sealed under the DeviceGroup
exporter as the history bundle is today: history (as today) plus accepted
contacts (with pins and rotation markers), channel subscriptions (subscriber
keys, per-epoch seeds), own publisher keys for owned channels, and the list of
known group ids. Afterwards, incremental sync rides the DeviceGroup journal in
the reserved proto fields 14-31 of `devicegroup_v1.proto`: `ContactUpsert`,
`ContactRemove`, `ChannelSubscribed`, `ChannelUnsubscribed`. LWW by the
existing HLC. Backup restore (`restoreFromServerBackup`) runs on **every**
identity-acquisition route, not only `recover()`, and the backup uploads on
change (owned by the readiness arc C; named here because a restored device
with no other device alive gets its contacts only from there).

Call history has no store on any client; it is out of scope here and is
listed in `docs/PRODUCT-READINESS.md` section 5 D.

### 2.10 Device removal

Removing a device (Devices screen, or forgetting this device) proposes, in
every group the account is in, the removal of that device's leaves
(self-service arm), removes the DeviceGroup leaf as today, and deletes the
device's discovery KeyPackage. ADR-0043's rotation follows the removal commit
where a role table exists. Server-side session revocation on removal is arc C.

## 3. Alternatives considered

1. **Primary-device relay.** Keep one leaf per account; the primary decrypts
   and forwards to siblings over the DeviceGroup and sends on their behalf.
   Rejected: chats exist only while the primary is online, the primary becomes
   a plaintext concentrator for every sibling, and every send from a sibling is
   a two-hop round trip. It does not meet the standard.
2. **Clone the account leaf to every device** (what backup restore does
   today). Rejected: two devices holding one leaf fork the epoch on the first
   concurrent commit; this is the G-38 hazard by construction.
3. **Per-device KT-log leaves.** Rejected: the log binds one key per account
   version and has no shape for concurrent keys; device authorisation belongs
   in a client-verified signature chain rooted at the logged account key.
4. **Binding via DeviceGroup lookup** (ADR-0007's sketch). Rejected in 2.2.
5. **Direct commits for self-service Adds** to avoid the sweep wait. Rejected:
   reopens the remote epoch fork ADR-0045 closed; 2.5 addresses the wait
   instead.

## 4. Consequences

Positive: a new device is a full client for chats and groups; a user who lost
every device is re-admitted by their contacts automatically; edits, reactions
and read state made from one device are attributed to the person on all of
them; the account key stops appearing in leaves, so a device compromise never
exposes the key that contacts pin.

Costs: a `lakko-mls` change with a new gate at every admission point;
group size grows by device count (a 1x1 between two three-device users is a
six-leaf group; commits and Welcomes scale accordingly); discovery exposes a
per-account device count; every client site that keyed on a leaf key must move
to the resolver (the web list is in the client sprint below); the G-39 review
surface grows.

Not changed: DS, auth, push-gw, the KT log, the safety number, the contact pin,
the supersession chain, the ADR-0027 rendezvous, the SFrame keying (per leaf,
correctly).

## 5. Sprints

Status 2026-09-16: **S0 to S4 built** on web; **S5 built on iOS and Android**
(build and unit tests only, not yet exercised on a device or across devices);
S6 open. S5, per platform, mirrors the web items below with these differences:
neither native client has the ADR-0049 confirmation prompt, so the pairing
fan-out hooks the QR scan path only; iOS has no background receive over all
chats, so the re-admission check runs at the tail of the open chat's poll;
Android's re-admission never retires this device's own leaf (its first poll can
precede its own KeyPackage upload); the iOS NSE and the Android background
decrypt rebuild the throwaway identity from the device seed. Known native
gaps: Android committer-beacon observations and call-link dedup still key on
the leaf; iOS pending-request row labels compare device keys with person keys;
iOS shows "Synchronizing..." only for bundle-named groups, not for a recovered
device with zero chats. Status before S5: Web S4 built:
per-device KeyPackage publisher; device mailbox poll; group create / add-contact
add all devices (a founder in ONE commit, 2.6); the pairing and confirmation
fan-out (`BackgroundReceiveApi.admitDevice`: wait for the new device's
KeyPackage in the directory, then a self-service Add proposal per chat, swept
where this device is the acting committer, a direct commit on a table-less
group); the ADR-0045 sweep at the tail of every **background** poll, so a
proposal in a chat nobody has open lands without that chat being opened; the
removal fan-out (`retireDevice`: the one leaf per chat, then the directory
row); the resolver at the client sites; people rather than leaves in every
count and 1x1 test; the single "Synchronizing..." label. Built later the same
day: the **re-admission arm of 2.4 on a schedule**
(`chat/device-readmission.ts`, run at the tail of every background poll, at
most once per ten minutes per chat: the device lists of this account and of
every accepted contact in the chat are compared with the roster; a listed
device with no leaf gets a self-service Add, and — for this account only — a
leaf whose directory row is gone gets a Remove, which is how `forgetDevice`
retires itself: it deletes its own directory row before the wipe; a leaf
this client saw removed is remembered per chat and never re-admitted, while a
removal that happened before this client joined is invisible to it — the
arm's known limit); the **state bundle** of 2.9 as one more tagged row on the
pairing rendezvous (`devices/state-bundle.ts`: accepted contacts with their
renames, the per-contact pins and rotation markers, the ids of every group the
old device holds — the last drives the "Synchronizing..." label as "known
groups not yet joined"); and the **history bundle on the confirmation route**,
which `prepareAnonymousJoinFlow` had skipped outright ("no device key"): both
bundles are now fetched at accept and applied at `persistTo`. Not built on
web: the DeviceGroup sync kinds 14-17 (a contact accepted after the pairing
reaches the other device only through a backup), and channel subscriptions in
the bundle (the channel list does not survive a reload on any device yet, so
there is nothing durable to carry).

Live on `test.lakko.me`, 2026-09-15 evening, two accounts, three web
clients: a device added through the ADR-0049 confirmation prompt showed
"Synchronizing...", was admitted to the account's existing 1x1 chat by the
confirming device (self-service Add proposal, swept by that device, Welcome to
the new device's mailbox), and received the next message the first device
sent; the contact received it too. Seen and fixed in the same run: the
confirming device had the chat open, so the background sweep left it to the
view, whose long poll was parked in a hidden tab — the view now polls on the
background's request (`ReceiveCoordinator.requestPoll`). Seen, fixed, not yet
re-verified live: a message from the person's other device rendered as
incoming; it now renders as the person's own. Seen, open: the pairing history
bundle's rows for the chat did not show on the new device (the chat opened
empty although the first device's log held a message from before the
pairing). Not exercised live: the removal fan-out; the founder's one-commit
add is pinned by the core test, the live chat of this run was founded with one
device per account.

- **S0** UX spec (`docs/ux/second-device.md`).
- **S1 core (built)**: `device_binding.rs` (sign/verify/parse, the
  `device_binding_decode` fuzz target); `Identity::bind_to_account` /
  `set_device_binding` / `from_seed_bound` / `from_seed_with_binding` /
  `account_public_key` / `device_binding`; the binding as the leaf credential
  on `create_key_package` and on the own leaf at `Group::create`;
  `account_key_of_credential` and `proposer_at` as the one resolver at every
  site in 2.3; the two self-service arms, the account-scoped Remove and the
  owner's-last-leaf rule in `authorise`; `owner_survives` over accounts and
  over the current owner even without a role change; `own_committer_rank` and
  `committer_rank_of` with the acting-leaf tiebreak and the 2.5 self-service
  rule, also honoured by the sweep's committer check; `MAX_LEAVES_PER_ACCOUNT`;
  `MemberRecord.account_key`, `Incoming::Application.sender_account`,
  `key_package::account_key`; two refusal reasons (`DeviceBindingInvalid`,
  `DeviceLimitReached`). Ten property tests in `tests/device_leaves.rs`.
  Property tests: a device leaf without a binding is refused at every gate; a
  binding for a different account is refused; two devices of one owner both
  resolve to the owner and only the lowest-leaf one acts; self-service Add and
  Remove accepted, cross-account refused; Leave removes all own leaves;
  `owner_survives` over device leaves; re-admission of a bound device of an
  existing member account by a plain member; the cap. Extend the
  `key_package` and `role_table_decode` fuzz targets.
- **S2 FFI (built)**: both surfaces: `Identity.fromSeedBound` /
  `fromSeedWithBinding` (+ `bindToAccount` on wasm), `deviceBinding`,
  `accountPublicKey`, `GroupMember.accountKey`, `ProcessedMessage.senderAccount`,
  `keyPackageAccountKey`, `committerRank` now ranking through the resolver, the
  two refusal variants appended (UniFFI ordinals preserved).
- **S3 discovery**: migration, handler, list fetch, delete, cap, three wire
  mirrors.
- **S4 web**: per-device KeyPackage publisher; device mailbox poll; group
  create / add-contact add all devices; the pairing fan-out in `runScanFlow`;
  StateBundle v2 and DeviceGroup sync kinds 14-17; the resolver at every
  client site (`peer-name` 1x1 test, `peerContactByMemberPubkeys`,
  `author-identity`, `presence-ops` folds, `group-routing` authority,
  `committer-beacon` dedup, profile resolver, membership-growth); device
  removal fan-out; backup restore on every acquisition route; the single
  "Synchronizing..." label. Two-device live test on `test.lakko.me` before
  the sprint is called done.
- **S5 iOS + Android**: the mirrors listed in the client fact sheet, plus the
  ADR-0049 confirmation prompt that the natives lack.
- **S6** `lakko-security`, docs, `PRODUCT-READINESS.md` rows moved.

## 6. Security notes

- Invariant #2: discovery stores opaque KeyPackages with an extra opaque
  32-byte key; the binding is a public signed artefact; nothing else new
  reaches a server.
- Invariant #3: this ADR and the S1 property tests; the change is on the G-39
  list.
- Invariant #4: a leaf without a valid binding is refused, never admitted
  unbound.
- Invariant #5: the per-account device count in discovery is the one new
  observable. Accepted for now; a future ADR may pad or blind it.
- Invariant #6: unchanged; the account key still rotates only via supersession,
  and a rotation invalidates every binding signed by the old key, which is
  correct: the contacts re-verify the account out of band and its devices
  re-bind under the new key.
- Invariant #7: device keys stay device-local; the account seed is already on
  every device by ADR-0017.
- Threat, stated: an attacker holding the account seed can mint bindings for
  attacker devices and, through the re-admission arm, be added to every group
  the victim is in by the victim's own contacts. Holding the account seed is
  already full account compromise (it signs supersession and recovers
  identity); this ADR does not widen it, but it makes the consequence
  immediate and automatic. The Devices screen lists every bound device with
  its added-on date, and removal is one action away.
