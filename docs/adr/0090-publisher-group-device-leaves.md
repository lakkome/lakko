<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0090: An owner or a publisher posts from any of their devices

- **Status:** Accepted (2026-11-13, maintainer: decisions A (b), B (a) and C
  as recommended, see "Decisions of the maintainer"). S1 to S3 (core, envelopes,
  FFI) built 2026-11-14, host-tested only, see "As built (S1 to S3)". S4 (web
  and desktop, iOS, Android) built 2026-11-14; an owner's second web device was
  admitted and posted on `test.lakko.me` the same day, nothing native walked,
  see "As built and walked (S4)". `lakko-security` ran 2026-11-14; its changes are item 7 there.
- **Date:** 2026-11-13
- **Context:** channels on a second device (UX
  [`second-device.md`](../ux/second-device.md) section 4a, the `lakko-sync`
  channel-subscription register of 2026-11-13, G-54); per-device conversation
  leaves ([ADR-0050](0050-per-device-conversation-leaves.md)); DeviceGroup
  commits that cannot strand ([ADR-0081](0081-devicegroup-commits-cannot-strand.md));
  proposal-based membership ([ADR-0045](0045-remote-commit-epoch-fork.md));
  journal ownership ([ADR-0057](0057-journal-ownership.md)); PublisherGroup
  ids derived from the owner's seed ([ADR-0078](0078-servers-cannot-link-accounts.md)
  phase 6); channels as conversations
  ([ADR-0087](0087-group-and-channel-handles-and-privacy.md) sections 7, 12, 15);
  device removal ([ADR-0074](0074-account-takeover-resistance.md)).
- **Extends:** ADR-0050 (the device binding and the self-service device
  admission, applied to the PublisherGroup), ADR-0081 (PublisherGroup commits
  are staged, as ordinary-group direct commits were).
- **Amends:** ADR-0087 sections 12 and 15 ("the owner's device" becomes "any
  device of the owner that holds the PublisherGroup"; the receiver rules of the
  hand-over envelopes), ADR-0064 section 7 (an owned or co-published channel is
  entered by admission from a sibling device, not by an external commit).
- **Closes, when built:** G-54.
- **Security:** a `lakko-mls` change under invariant #3 (this ADR, property
  tests, fuzz targets; the G-39 external review surface grows). Flag for
  `lakko-security` before merge.

## Context

Since 2026-11-13 every channel reaches every device of the account through the
`lakko_sync::synced::channel_subscription` register, with one subscriber key per
account and channel, so every device reads. Only the device that holds the
channel's PublisherGroup posts, approves, rotates and admits. The others are
readers (UX `second-device.md` section 4a), and a lost publishing device leaves
the channel with nobody who can post again (G-54). The recon of 2026-11-13
(`recon-channels-second-device.md`, sections 3 to 5) established why:

1. **The PublisherGroup leaf is the account key.** Web opens the group in the
   provider of the session account identity, iOS from an identity derived from
   the account's lineage seed, Android from the phrase seed;
   `PublisherGroup::create` calls `Group::create_with_id(identity, id)` with
   that identity (`crates/lakko-mls/src/publisher_group.rs:397`). Every device
   of the owner would present the same signature key, and RFC 9420 section 5.3
   (OpenMLS `validate_key_uniqueness`) admits one leaf per key. ADR-0050 solved
   exactly this for conversations; the PublisherGroup never moved.
2. **Copying the MLS state is out** (ADR-0050 section 2.9a reason 3): two
   devices on one leaf cannot apply each other's commits
   (`CannotDecryptOwnMessage`) and would both rotate.
3. **PublisherGroup commits merge before the journal answers.**
   `PublisherGroup::rotate` is `Group::self_update` (eager),
   `remove_publisher` is `remove_leaves_directly` with `DirectFinish::Merge`,
   `approve_external_join` is `approve_external_join_directly` (eager). The
   clients send the commit through the ADR-0081 commit route afterwards; a
   `409` is recorded as `journalLost` and the subscriber reseal still runs
   (`apps/web/src/channels/publisher-commit-outbox.ts:40-56, 258-284`). With one
   committer that is harmless. With two, the loser sits on an epoch nobody else
   holds and seals its seed to the subscribers.
4. **"The owner's device" is singular** in ADR-0087 sections 12 and 15 (only the
   owner's device rotates). Nothing says which of two.
5. **The DS already admits only owner commits on a PublisherGroup journal.** Its
   root is derived from the owner's account seed (ADR-0057), and co-publishers
   get no registrar (ADR-0057 "Recorded" point 4). Every device of the owner
   derives the same root, so the DS cannot tell two owner devices apart; it
   accepts one commit per epoch, the first.

The service side is already account-wide, verified by reading:
`services/channels` gates `POST /v1/channels/{id}/posts` and every other
publisher route through `owner_auth::require_publisher`, which accepts an owner
proof under the key from `lakko_channels::owner::channel_owner_seed(account_seed,
channel_id)` or a co-publisher proof under an appointed key from
`lakko_channels::copublisher::channel_copublisher_seed(account_seed, channel_id)`;
`publish_post` stores the body opaque (`services/channels/src/api.rs` `publish_post`)
and never reads the post's signing key. Readers verify each post against the
signing key it carries (`lakko-channels/src/post.rs` `verify_and_decrypt` builds
the verifying key from `post.sender_pubkey`) and pin none. So any device of the
owner or of an appointed co-publisher can publish today as far as the service
and the readers are concerned. What it lacks is the channel seed, which only a
PublisherGroup member derives.

## Decision

The PublisherGroup becomes a group of devices, as conversations did in
ADR-0050: each device of the owner and of every co-publisher holds its own
bound leaf; an owner device adds and retires the devices of an account that
already publishes, without asking the person; every PublisherGroup commit is
staged and merged only when the DS takes it, so any owner device may rotate and
the DS picks one per epoch.

### 1. Leaves are bound devices

- A PublisherGroup leaf is the **device identity** of ADR-0050 (the identity
  that already holds the device's conversation leaves), carrying the ADR-0050
  device binding as its `BasicCredential` identity bytes. Every resolver in
  `lakko-mls` (`account_key_of_credential`, `MemberRecord.account_key`) already
  reads it; `PublisherGroup::members()` already groups by account.
- **The group id does not change.** It stays
  `BLAKE3-derive-key("lakko/pubgrp/id/v3", owner_account_seed ‖ channel_id)`
  (ADR-0078 phase 6), computed from the owner's account seed (the seed each
  client already passes today), so every owner device computes it.
- **API.** `PublisherGroup::create(device_identity, owner_account_seed,
  channel_id)` and `PublisherGroup::load(device_identity, owner_account_seed,
  channel_id)` replace the one-identity forms; `load_joined` and
  `join_from_welcome` take the device identity. `is_owned_by` becomes
  `is_owned_by(owner_account_seed)`: true on every owner device, false on a
  co-publisher's. **`open_or_create` is deleted from the core and both FFI
  surfaces.** The trap of the recon (section 4: a device without state founds a
  fresh group at the same id and resets the live journal) stops being a client
  discipline and becomes impossible to reach by accident: founding is `create`,
  called only from create-channel and from the re-founding of section 2.5.
- **Cap.** `MAX_LEAVES_PER_ACCOUNT = 8` (ADR-0050 section 2.4) holds in a
  PublisherGroup too, checked by the committer and by every peer.
- **Peers verify.** A PublisherGroup commit is processed through a gate that
  refuses (fail closed) a commit that adds a leaf whose binding does not
  verify, a commit that would put more than 8 leaves of one account in the
  roster, and a commit whose committer does not resolve to the owner's account.
  The last rule is new: ADR-0087 section 15 states that peers do not enforce
  "only the owner commits"; the DS enforces it (point 5 of the Context), and
  with the owner's account key known to every member it becomes a peer rule
  too. Owner devices know it as their own account key; a co-publisher's device
  records it when it joins (the issuer of the signed `/g/` capability). Sprint
  1 confirms that the capability's issuer key is the owner's account key before
  the rule is written against it.
- **Removing a publisher never touches an owner leaf.** `remove_publisher`
  refuses an account key equal to the owner's account (today it compares the
  calling device's signature key, which with device leaves no longer names the
  owner account).
- **Flag day.** No production data exists. PublisherGroups whose leaf is an
  account key are not migrated; channels on the test stack are created again.
  The ADR-0050 resolver's legacy arm (an empty credential means an account-key
  leaf) stays as it is for conversations and is not relied on here.

### 2. How a device of an account that publishes gets in, and goes

**2.1 The owner's devices.** At every pass of the publisher loop, and when the
account's DeviceGroup grows or shrinks, an owner device that holds the
PublisherGroup compares the owner-account leaves of the PublisherGroup with the
account's DeviceGroup roster:

- A device in the DeviceGroup roster with no leaf: fetch its KeyPackage from
  discovery (the account's own device list, the ADR-0050 re-admission path),
  check that its binding names the owner's account, and add it.
- An owner-account leaf whose device is not in the DeviceGroup roster: retire
  it (remove that leaf).

The DeviceGroup roster is the authority on which devices belong to the account
(ADR-0074: an own device is admitted only if it is in the roster). Everything
found in one pass goes into one commit. A device is never added on the strength
of a KeyPackage alone.

**2.2 A co-publisher's devices.** A co-publisher's publishing device runs the
same comparison for its own account but cannot commit (the DS registers no
co-publisher epoch). It sends the owner a new PublisherGroup application
message, `lakko-contacts::publisher_device_request`:

```text
"lakko/publisher-device-request/v1\0" ‖ version(1)=0x01 ‖ channel_id(16) ‖ action(1)
  action 1, admit:  key_package_len(2 BE) ‖ key_package ‖ delegation_count(1) ‖ delegations
  action 2, retire: device_key(32)
```

bounded and fail-closed like its siblings, dispatched as
`IncomingEnvelope::PublisherDeviceRequest`, with a fuzz target. An admit carries
ADR-0060 committer delegations for the co-publisher's mailbox, so the owner's
device can deliver the Welcome without holding a grant (the G-53 class). An
owner device acts on it only when the MLS sender resolves at receipt (C-68) to
a current publisher account, the KeyPackage's binding names that same account
(admit) or the named leaf belongs to it (retire), and the cap holds. A
delegation lives at most 24 hours (ADR-0060); a request not acted on by then
is sent again by the next pass of the co-publisher's device.

**2.3 The Welcome** goes to the added account's mailbox (ADR-0060: one inbound
mailbox per person). For its own account the owner device writes with the
account's own reach keys; for a co-publisher it uses the delegation of 2.2,
then any grant it holds, and otherwise keeps the Welcome owed (ADR-0050
section 2.5a item 7). The added device opens it with
`PublisherGroup::join_from_welcome` under the id it computes (owner) or reads
from the register's `publisherGroupId` (co-publisher).

**2.4 Leaving.** A device that runs Forget device, or is removed from the
account, sends a retire request for its own leaf before wiping, best effort; an
owner device that is forgotten sends the same request (it cannot remove its own
leaf: OpenMLS `CannotRemoveSelf`), and another owner device applies it. If the
request is lost, the reconcile of 2.1 (owner) or of 2.2 (the co-publisher's
other device) retires the leaf. Retiring a leaf advances the epoch, so the
retired device cannot derive the next channel seed (RFC 9420 section 12.4).

**2.5 When no device of the account is online, or none is left.** A new device
whose account has another publishing device waits until that device runs
(section 8 says what the person sees). Nothing is asked of the person and the
new device does not knock: only a device that already holds the group can add
it. An owner whose every publishing device is gone is the case G-54 records;
decision A below settles it:

- **Re-founding (decision A).** A device that founded the account's
  DeviceGroup alone (ADR-0081 third amendment point 4: no other device
  answered its knock) founds the PublisherGroup again for each channel whose
  subscription register says `Owner`: `PublisherGroup::create` at the same id,
  then a reset of the channel's journal under the same root (ADR-0057: a
  same-root claim on a live journal is a reset; the DS route exists). It then
  reseals the new seed to every subscriber as after any rotation, including
  the rows of a private channel whose ticket it can open and check. The
  admission seeds are random and today live only on publishing devices (not in
  the synced state, not in the backup), so a restored phone usually holds none:
  it mints a new admission-seed version, and the private channel's earlier
  subscribers, whose tickets it cannot check, are not resealed and need a new
  invitation (open question 4). Co-publishers are no longer in the group and are
  invited again by the owner. A device that has siblings in its DeviceGroup
  never re-founds, whether or not they publish; it waits.
- **The old device that comes back.** Its PublisherGroup journal was reset under
  it, so its signed read is refused or the DS names an epoch chain it does not
  hold, as in ADR-0081 third amendment point 5. It deletes its local
  PublisherGroup state for that channel (`Group::delete`), becomes a reader, and
  is admitted by 2.1 on the re-founding device's next pass. Its unsent posts
  under the old seed are dropped, not sent.

A co-publisher whose every publishing device is gone is invited again by the
owner, as today. A co-publisher's new device is admitted only by an owner
device (decision B).

### 3. Every PublisherGroup commit is staged

`lakko-mls` gains the staged forms and loses the eager ones:

| Today (eager) | After (staged) |
|---|---|
| `rotate` (`self_update`) | `stage_rotate` |
| `remove_publisher` | `stage_remove_publisher` |
| `approve_external_join` (`approve_external_join_directly`) | `stage_approve_external_join` |
| none | `stage_admit_devices(key_packages)`: only leaves of an account already in the roster (the owner's or a current publisher's) |
| none | `stage_retire_devices(device_keys)` |

A device may combine one admission set, one retirement set and the rotation in
one commit. The lifecycle is the one ordinary groups use since ADR-0081
(`DirectFinish::Stage`, `has_staged_sweep`, `staged_export_secret`,
`merge_staged_sweep`, `abandon_staged_sweep`, per-group `clear_pending_commit`).
The client records the staged commit with the snapshot carrying it and the
epoch-N access seed (the PublisherGroup record ADR-0081 already keeps), sends it
through `POST /ds/group/{hex}/commit`, merges on `201` or on the identical
re-send's `200`, abandons on `409`, then reads the journal and applies the
winner. An unanswered commit is re-sent by the owed verdict, as for ordinary
groups.

**The reseal follows the merge, never the stage.** A device seals a new seed
to subscribers only after its own commit merged. A lost race therefore never
puts a seed only the loser holds in front of the subscribers, which closes the
`journalLost` path of Context point 3.

**What it does to the publish path.** Posts are not MLS messages; a post is
sealed under the current channel epoch's seed. While a commit is staged the
composer keeps sealing under the current seed (the staged N+1 seed is never
used before the merge and the reseal), so posting does not wait on the DS.
Membership operations on that PublisherGroup are refused while a commit is
staged (OpenMLS `is_operational`), and application messages are not (the
hand-over envelopes still go out). The existing rule for a post sealed while
the channel service trails the MLS epoch (`composerSeedFor`,
`apps/web/src/channels/publisher-seed.ts`) is unchanged.

### 4. Who rotates: any owner device, the DS decides

Every owner device that holds the PublisherGroup runs the rotation policy
(ADR-0087 section 15, `RotationPolicy`). When a rotation is due, the device
waits a random 0 to 60 s (chosen, not measured), re-reads the journal head and
applies anything new, re-evaluates, and only then stages. Two owner devices that
still stage on the same epoch are resolved by the commit route: one merges and
reseals, the other abandons, applies the winner and does nothing more.

The policy's inputs are rebuilt from what every owner device sees, not kept per
device: the start of the current channel epoch is the DS row time of the commit
that began it (each device applied that commit), and the post count is the
number of posts listed under the current epoch id. Any commit that changes the
seed (an admission, a retirement, a publisher change) counts as a rotation for
the policy and restarts its clock.

**A reseal that its rotator never finished.** When an owner device holds an MLS
epoch whose seed the channel service has not taken (the service's current epoch
trails the group's) for 5 minutes (chosen, not measured), it performs that
reseal itself. Two devices doing it is harmless: the service commits a staged
epoch only at `current + 1` (ADR-0030), so the second commit is refused. What
the second leaves behind is staged rows for an epoch already committed, which
the unbuilt staging GC of ADR-0030 open question 2 would sweep.

Co-publisher devices still never rotate (ADR-0087 section 15, 2026-11-08); a
co-publisher's rotate request is applied by whichever owner device reads it
first.

Considered and rejected:

- *The lowest-keyed owner leaf rotates.* A lost device stays in the roster
  until someone retires it, so a lost device whose key sorts lowest stops the
  channel's rotation; without a liveness signal (ADR-0050 uses beacons; a
  PublisherGroup has none) nobody can tell. Two rotators are still possible
  across a roster change.
- *The device that created the channel, unless gone.* "Gone" is the same
  unknowable fact. While the creator is offline nothing rotates, including the
  rotation after a removal that is meant to cut a subscriber off.
- *Any device, the DS decides* is the only one with no stuck state as long as
  one owner device runs, and its race costs a wasted staged commit, not a fork.

### 5. Adding a device rotates the seed, and that is accepted

The channel epoch at the service tracks the PublisherGroup's MLS epoch: the
composer seals under the MLS epoch it read the seed at, and the reseal files the
seed for the next service epoch, which the service takes only at
`current + 1` (ADR-0030). Every commit therefore changes the seed the
subscribers need, an admission included.

Cost, arithmetic from ADR-0087 section 7 (80 MB a rotation at one million
subscribers is 80 bytes per subscriber key): on a **private** channel each
seed change seals one envelope per admitted subscriber key from the committing
device; on a **public** channel it files one public-seed envelope under the read
key, plus per-key envelopes only to rows admitted while the channel was private
and to the account's own reader key. An admission costs what one scheduled
rotation costs, and a scheduled rotation already runs once a day by default
(C-54). Devices are added far more rarely than once a day. And private channels
move to the ADR-0086 key tree before release (ADR-0087 plan step 8), where a
seed change is one envelope for every channel size.

Considered and rejected: *an admission does not change the subscribers' seed.*
It would mean a channel epoch counter separate from the MLS epoch, advanced by
every commit except admissions, with every member agreeing on its value; a
device joining by Welcome would need the counter and the current seed before it
processed the next commit, which the hand-over (an application message sent
after the commit) cannot guarantee. It buys nothing in confidentiality (the
added device is the same person) and nothing after the key tree lands.

### 6. Publisher bookkeeping that is not MLS

| Item | Carried how |
|---|---|
| Admission mark (`admittedThroughSeq`) | A new PublisherGroup application message `lakko-contacts::channel_admission_mark` (`"lakko/channel-admission-mark/v1\0" ‖ 0x01 ‖ channel_id(16) ‖ through_seq(8 BE)`, fixed length, fuzzed), sent by any publishing device that advances its mark, kept as the maximum by every publishing device of every account. Safe to merge by maximum: a device advances its mark only after removing every invalid row at or below it, so every row still at the service at or below any device's mark was admitted. A co-publisher who inflates it gives the seed to rows it could hand the seed to anyway. |
| Admission seeds (private channel) | The existing owner hand-over, now also to the owner's own account: after an admission commit merges, the committing owner device sends the account of each added device the read key (`channel_read_key`), every admission-seed version, and the newest 64 epoch seeds (`channel_epoch_seeds`), sealed to that account's X25519 key as today. A device of that account that already holds them drops the duplicates. |
| Invite links | Unchanged: the ADR-0079 link register already carries `/g/` records with invite seeds (ADR-0087 section 7), across the devices of one account. |
| Hand-over records (`sentVersions`, `readKeySent`, `sentEpochs`) | Per device, not carried. An owner device with no record of a send treats it as unsent and sends; receivers keep a seed or key only for an epoch or version they lack, so a repeat changes nothing. |
| The account's own reader key | From the channel-subscription register (`subscriberSecret`); every device derives the key. |
| Retained publisher requests | In each owner device's group state (`retain_join_request`); every owner device reads the knock journal. An approval is a staged commit the DS decides; the joiner shows up in the roster and every other owner device's retained copy is filtered out by membership (ADR-0027). |
| Rotation state | Rebuilt (section 4). |
| Ban list, allowed reactions, name | Channel metadata posts, readable by every device. |

The hand-over receiver rules of ADR-0087 section 7 change in one word: the MLS
sender must resolve to the owner's **account** (any owner device), and
`member_account_key` may be the receiver's own account when the receiver is an
owner device.

### 7. Signing seeds and counters stay per device

Each publishing device mints its own random post signing seed and keeps its own
publish counter, and neither ever leaves the device (G-51's resolution rests on
exactly this). The post key and nonce are derived from
`(seed, channel_id, epoch_id, sender_pubkey, message_counter)`, so two devices
with different signing keys never share a nonce under one seed. Verified above:
the service gates posts on the account-derived owner or co-publisher proof,
which every device of the account can sign, and never checks a post's signing
key; readers pin none. A co-publisher's appointment is per account
(`PUT /v1/channels/{id}/copublishers/{pk}` with the account-derived key), so a
co-publisher's second device needs no appointment, and `MAX_COPUBLISHERS`
counts accounts, not devices. The proof nonce table is shared by every caller of
a channel; nonces are random per proof.

### 8. What the person sees

- **Admitted.** The reader line of `second-device.md` section 4a disappears and
  the device becomes the same as the first: composer, Edit and Delete on posts,
  the owner's or publisher's info blocks, the exit block, post notifications as
  configured. Nothing announces the change.
- **Waiting.** While another publishing device of the account exists and this
  device has no leaf yet, it stays a reader, and the line in place of the
  composer says when it will be able to post:
  - owner: "You can post here once Lakko opens on your other device."
  - publisher: "You can post here once Lakko opens on your other device and on
    the owner's."
  No button, no spinner, no key or group named. The owner's device appears on
  the publisher's line because only an owner device admits (decision B).
- **Retired.** A device that leaves the account loses its leaf and posts no
  more; the person did that on purpose, so nothing is shown beyond the existing
  removal flow.
- **Re-founded (decision A).** The restored phone posts at once; nothing is
  shown. Co-publishers see the channel as subscribers until invited again
  (their device was removed with the old group, ADR-0087 section 9 for a
  publisher); the owner's `Publishers` block lists nobody.

### 9. Invariants and review

- **Invariant #3.** `lakko-mls` changes: the PublisherGroup opened and founded
  with a device identity and the owner's account seed; `open_or_create`
  deleted; `is_owned_by(owner_account_seed)`; the staged forms of section 3
  and the deletion of the eager ones; `stage_admit_devices` /
  `stage_retire_devices`; the PublisherGroup commit gate (binding, cap,
  committer is the owner's account); `remove_publisher` keyed on the owner's
  account. Property tests (`tests/publisher_group_devices.rs`): two devices of
  one account hold distinct leaves and derive the same channel seed; an
  admission staged and abandoned on a lost race leaves the group at N and other
  groups untouched; two owner devices staging a rotation on one epoch end at
  one epoch and one seed after the loser applies the winner; a retired device
  cannot derive the next seed; `remove_publisher` never removes an owner-account
  leaf; an admission of a leaf whose binding fails, of an account not in the
  roster, or past 8 leaves is refused locally and by every peer; a commit by a
  co-publisher's leaf is refused by every peer; `is_owned_by` is true on every
  owner device and false on a co-publisher's. Fuzz targets:
  `publisher_device_request_decode` and `channel_admission_mark_decode`
  (`lakko-contacts`). The G-39 external review grows by the PublisherGroup
  commit gate and the staged PublisherGroup lifecycle.
- **Invariant #2:** unchanged; the new envelopes ride MLS application messages
  the DS cannot open. **#4:** every new gate fails closed. **#5:** no new server
  state; the DS still sees one root per PublisherGroup journal and the channels
  service still sees one owner key and one key per co-publisher account.
- **Proposed invariant-index entries,** indexed when built: *C-90*, a device
  merges no PublisherGroup commit the journal did not take, and a channel seed
  reaches subscribers only from a merged commit; *C-91*, a PublisherGroup leaf
  is a bound device, only a leaf of the owner's account commits, and a device
  joins only for an account that already publishes and vouches for it (the
  owner's DeviceGroup roster, or a request from the co-publisher's own
  publishing device).
- **`lakko-security`** pass before merge: the device admission of a
  co-publisher by request, the committer rule, the staged lifecycle, and the
  re-founding of decision A.

## Decisions of the maintainer (2026-11-13)

- **A. An owner whose every publishing device is gone: (b).** A device that
  founded the account's DeviceGroup alone (ADR-0081 third amendment: no other
  device answered) founds the PublisherGroup again for each owned channel: same
  id, the journal reset under the same root, co-publishers invited again, every
  subscriber gets a new seed; an old owner device that comes back finds its
  journal reset, drops to reader and is admitted by 2.1 (section 2.5).
  Not chosen: (a) the channel stays readable and nobody posts again; (c) (b)
  plus an explicit action on a device whose siblings none publish.
- **B. Who admits a co-publisher's new device: (a), only an owner device.** The
  co-publisher's new device waits until an owner device runs. Not chosen: (b)
  the co-publisher's own publishing device too, which would have needed a
  co-publisher registrar on the DS (ADR-0057 point 4) and a peer rule for a
  co-publisher's own-device commit.
- **C. Any owner device rotates; the DS takes one commit per epoch** (section
  4). This replaces "the owner's device alone" of ADR-0087 section 15
  (2026-11-08), which carries a dated note pointing here.

## Alternatives

- **Keep one publishing device per account** (today, G-54). Rejected as the
  final design: a lost phone orphans the channel, and the owner must post from
  one specific device.
- **Copy the PublisherGroup state to the other devices.** Rejected by ADR-0050
  section 2.9a: two devices on one leaf cannot apply each other's commits and
  both rotate.
- **Enter by external commit** (ADR-0064 section 7, built nowhere). Needs a
  GroupInfo export, an external-commit builder and a vault in `lakko-mls` and
  `services/storage`, and still needs per-device leaves, since an account-key
  leaf cannot have two devices. Admission by a sibling reuses ADR-0050 and
  ADR-0081 instead. A device with no sibling re-founds (decision A); an
  external commit could later replace that once the vault is built.
- **The new device knocks** (the ADR-0081 rejoin pattern). The device that must
  admit it already sees it in the DeviceGroup roster (owner) or is told by the
  co-publisher's own device (request), so a knock adds a DS round, a token and a
  sealed row and decides nothing new.

## Consequences

- **Positive:** an owner or publisher posts, approves, admits and (owner)
  rotates from any device; a lost device is retired and the channel goes on;
  rotation stops depending on one device being online; the found-and-reset trap
  is gone from the API; a lost commit race no longer reseals a dead seed.
- **Negative:** every device added or retired costs one reseal (section 5); a
  co-publisher's new device waits for an owner device (decision B); the
  PublisherGroup gains a committer gate and a staged lifecycle in `lakko-mls`
  (G-39 grows); two new envelopes and two fuzz targets; a flag day for
  PublisherGroups on the test stack.
- **Neutral:** the DS, the channels service, discovery and the wire of posts do
  not change.

## Open questions

1. The 0 to 60 s rotation jitter and the 5-minute unfinished-reseal wait are
   chosen; every client logs the commit-route outcome of rotations so they can
   be set from what is seen.
2. Staged reseal rows left by a refused second reseal wait for the ADR-0030
   staging GC, which is unbuilt.
3. Whether a PublisherGroup should carry an ADR-0022 role table, making "only
   the owner adds a publisher" a peer rule as well (ADR-0087 section 15 left it
   out); the committer rule of section 1 covers commits, not who an owner may
   add.
4. ~~A re-founded private channel (decision A) loses its earlier subscribers
   because the admission seeds are not carried to a restored device.~~
   **Closed 2026-11-14:** the owner account's admission seeds ride a
   `lakko-sync` register of their own, see "As built (S1 to S3)", item 6.

## As built (S1 to S3, 2026-11-14)

Host-tested only: unit and property tests, the DS test against a local
MariaDB, the fuzz runs below. No client calls any of it yet (S4).

1. **`lakko-mls` (S1).** `PublisherGroup::create(device, owner_account_seed,
   channel_id)`, `load(device, owner_account_seed, channel_id)`,
   `load_joined(device, group_id, channel_id)` and
   `join_from_welcome(device, welcome, group_id, channel_id,
   owner_account_key)`; every entry point refuses a device identity without a
   verifying ADR-0050 binding, and `create` / `load` refuse one bound to
   another account than the seed's. `create` also refuses when this device
   already holds a group under the id (delete first). `open_or_create`, the
   eager `rotate`, `remove_publisher` and `approve_external_join`,
   `Group::approve_external_join_directly`, `Group::remove_leaves_directly` and
   `PublisherGroup::inner_mut` are deleted. New: `stage_commit(device, admit,
   retire, rotate)` (one commit: admissions, retirements, update path),
   `stage_rotate`, `stage_admit_devices`, `stage_retire_devices`,
   `stage_remove_publisher`, `stage_approve_external_join`,
   `has_staged_commit`, `staged_channel_seed`, `merge_staged_commit`,
   `abandon_staged_commit`, `process_incoming` (the gated receive), `delete`
   (state of an active or ended group, for section 2.5), `owner_account_key`,
   `is_owned_by(owner_account_seed)`, `is_owner_device(device)`; free
   `publisher_owner_account_key(seed)` and `check_publisher_commit`.
   - **The owner's account key is recorded beside the group**, a provider row
     `lakko/publisher-group-owner/v1\0 ‖ group_id` (32 bytes) that rides the
     identity snapshot like the retained join requests: an owner device writes
     it from the seed, a co-publisher device at `join_from_welcome`, where the
     Welcome's sender must resolve (ADR-0050 binding) to that key or the join
     is refused before anything is written. `load_joined` refuses a group
     whose row is missing.
   - **S1's confirmation:** on all three clients the co-publisher `/g/`
     capability is signed with `ed25519Seed()` of the same identity the
     PublisherGroup id is derived from (web `copublisher.ts`
     `mintOneUseCoPublisherLink`, iOS `CoPublisher.mintPublisherLink`, Android
     `CoPublisher` `composeCoPublisherInviteCapability`), so its issuer key is
     the owner's account key as long as S4 keeps signing it with the owner
     account seed it passes to `create`.
   - **The gate** (`check_publisher_commit`, run by the committer before it
     stages and by every peer in `process_incoming` before it merges): the
     committer's leaf resolves to the owner's account; the commit carries
     only `Add` and `Remove` (an `Update`, a `GroupContextExtensions` or any
     other proposal is refused); every added leaf carries a binding that
     verifies (an empty credential, the legacy account-key leaf, is refused
     here); no account ends with more than 8 leaves. Typed refusals, all
     existing `RefusalReason`s, so no new `FfiError` variant: `NotOwner`,
     `UnsupportedProposal`, `DeviceBindingInvalid`, `DeviceLimitReached`; the
     owner as the target of a removal is `OwnerUnremovable`.
   - **Local rules beyond the gate:** an admission (`stage_admit_devices`,
     `stage_commit`'s `admit`) adds only a device of an account that already
     holds a leaf not being retired in the same commit; an approve adds any
     bound device (it is how a new publisher arrives); a device never retires
     its own leaf (OpenMLS `CannotRemoveSelf`; another owner device does).
   - A staged approve keeps the retained request until the commit merges
     (the merge reaps it), so an abandoned approve leaves it where it was.
   - Not built: the gate does not compare the committer's update-path leaf
     node with its old leaf (a committer could carry another binding of its
     own key in the path); recorded for the security pass.
   - Tests: `tests/publisher_group_devices.rs` (11, four of them proptests)
     plus the rewritten `publisher_group_removal.rs` (5), `join_requests.rs`
     and `group_id_width.rs`; `lakko-mls` lib 268. Every section 9 item has a
     test: distinct leaves and one seed; a lost race abandons to N with
     another channel's group untouched; two owner devices rotating on one
     epoch converge (either winner); a retired device derives no later seed;
     `remove_publisher` never touches an owner leaf; an unbound leaf, a
     foreign account and the ninth leaf refused locally, and an unbound leaf,
     the ninth leaf and a co-publisher's commit refused by every peer (built
     on a raw `Group` handle, not merged); `is_owned_by` per role; a Welcome
     from anyone but the owner refused; re-founding at the same id shuts the
     old state out until it deletes and is admitted.
2. **`lakko-contacts` (S2).** `publisher_device_request` (wire as sketched in
   section 2.2 with one change: the admit carries the requesting account's
   97-byte ADR-0078 mailbox binding before the delegation count, since a
   delegation names a mailbox and an owner device checks delegations only
   against the signed binding, as for `committer_delegation`;
   `key_package_len` 1 to 4096, at most 16 delegations; lengths 84 to 7208)
   and `channel_admission_mark` (57 bytes, as sketched). Dispatch kinds
   `IncomingEnvelope::PublisherDeviceRequest` / `ChannelAdmissionMark`
   (steps 7n5 / 7n6), cross-kind isolation against every dispatched domain,
   fuzz targets `publisher_device_request_decode` and
   `channel_admission_mark_decode`.
3. **DS (S2), no change.** A DB-gated test
   (`commit::tests::two_owner_devices_get_one_commit_per_epoch_and_a_reset_drops_the_chain`)
   shows two devices deriving one PublisherGroup root get `201` then `409`
   at one epoch, the identical re-send `200`, and a same-root reset leaves
   only the new chain (the old epoch key's read is `401`).
4. **FFI (S3).** UniFFI and wasm carry the new `PublisherGroup` surface, the
   two envelopes (`publisher_devices` modules), the two appended dispatch
   kinds, and the admission-seed register; the extension surface recognises
   both envelopes (`isPublisherDeviceRequestEnvelope`,
   `isChannelAdmissionMarkEnvelope`). The receive method keeps its name,
   `processIncomingVerifyingRoles`, and now runs the gate. Each surface has
   the two tests the plan asks for (two owner devices admit a third, one
   commit lands; a co-publisher's request crosses the boundary and is
   applied). The exact signatures are in the S4 contract.
5. **Not changed:** the Delivery Service, the channels service, discovery,
   the wire of posts, `lakko-channels`.
6. **Open question 4, the carrier (coordinator's call, 2026-11-14).** A new
   `lakko-sync` register `channel_admission_seed`, keyed
   `(channel id, admission version, seed id)` where the seed id is
   `BLAKE3-derive-key("lakko/sync/channel-admission-seed/id/v1", seed)`,
   value the 32-byte seed, tag 21, persisted state version 14 (13 refused):
   write-once per seed at the writer, folded by the ordinary rule, carried by
   its own DeviceGroup service envelope `lakko/sync/channel-admission-seed/v1\0`
   (1 to 16 entries of 58 bytes, sorted by the register key; the seed id is
   computed, not carried), re-sent on DeviceGroup growth, persisted and so in
   the backup, never in the growth snapshot; the seed never reaches `Debug`.
   **Two seeds of one version coexist**: two owner devices that read the same
   highest version and mint it at once each hand out links naming their own
   seed, so both registers survive on every device (property test
   `concurrent_mints_of_one_version_both_survive_in_either_order`), and a
   ticket of version `v` is checked against every seed held for `v`
   (`channel_admission_seeds_for`); the reseal and the admission accept a row
   whose ticket opens under any of them. A sibling register rather than a field
   of the channel-subscription register: that register is last-writer-wins on
   the whole value, so two owner devices writing it at once (one a new
   admission version, the other a name) would drop a seed. Nothing else is
   needed to keep checking tickets: the live invite keys come from the `/g/`
   link records (ADR-0079 register, invite seeds included) and the owner's
   read key derives from the account seed.

7. **The recorded owner key (2026-11-14).** The `lakko-sync`
   channel-subscription register gains `owner_account_key`: 32 bytes,
   required for role co-publisher, absent for owner (its own key derives from
   the account seed, so a recorded copy could only disagree) and subscriber;
   envelope version `0x02` (`0x01` refused), persisted state version 15. The
   device that joins as co-publisher writes it from the issuer of the `/g/`
   capability it verified. A co-publisher's device accepts a PublisherGroup
   Welcome only from a device of that recorded key (it passes it to
   `join_from_welcome`); the key is never taken from the Welcome itself, a
   contact match or a group-id match.

## As built and walked (S4, 2026-11-14)

1. **Built on web (and desktop), iOS and Android** against the S1 to S3
   surface: the PublisherGroup opened with the bound device identity, never
   founded outside create-channel and decision A; the own-device reconcile of
   2.1 and the co-publisher request of 2.2; every PublisherGroup commit staged,
   recorded, sent through the ADR-0081 commit route and merged or abandoned on
   the answer; the reseal after the merge; the rotation of section 4; the
   admission-seed register (`lakko-sync` `channel_admission_seed`, keyed by
   channel, admission version and seed id) and the owner's account key carried
   in the channel-subscription register for a co-publisher (persisted synced
   state version 15); the two waiting lines of section 8 in every catalog
   language.
2. **Unit suites, run 2026-11-14:** web vitest 8301 passed (two unrelated cases
   that fail in the full run pass alone), iOS `LakkoTests` 5303 with 0
   failures, Android 5695 with 0 failures, `services/ds` 98 of 98 in three
   parallel runs.
3. **Walked on web, 2026-11-14, `test.lakko.me`.** An owner created a private
   channel on device one and added a second web device. Device one admitted
   the second by itself: one staged commit through the DS commit route, then
   the reseal, then the Welcome. The second device got a composer and posted;
   device one received the post.
4. **Found on the walk, fixed on all three clients: no rotation with
   subscribers had ever succeeded.** The stage entry of a staged rotation
   (ADR-0030) sent `subscriber_pubkey_hex` where the channels service has
   always required `subscriber_id`, and encoded the sealed envelope as standard
   base64 where the service requires base64url without padding. Every reseal
   to subscribers was refused (`422`, then `400` once the field was renamed)
   on every client since the staged rotation was built. After the fix the
   stage and the commit both answered `200`.
5. **The flag day, as seen.** A PublisherGroup that lived in the old account
   provider is not carried to the device provider, so an existing owner's
   earlier channels are read-only on its own device (three channels of the
   test owner). Such a channel shows neither a composer nor the waiting line:
   an open UI gap (G-54).
6. **Not walked:** a native second device; decision A's re-founding; a
   co-publisher's device request; a rotation by a second owner device.
7. **Security pass (`lakko-security`, 2026-11-14), three changes asked before
   merge, made the same day on all three clients:**
   - *The admission mark.* Any current publisher's device could send a mark
     that every receiver kept as a maximum for good, which switched off the
     private-channel ticket check for every later subscriber row and outlived
     the sender's removal. A mark is now taken only from a sender that
     resolves at receipt to the channel owner's account (co-publishers do not
     commit), and it counts only up to the newest subscriber row this device
     has itself read from the channels service; the rest waits until it has.
   - *No PublisherGroup is deleted on an unauthenticated DS signal.* Refused
     reads, an empty-knock epoch that differs, and a DS latest epoch below the
     group's used to drop the local group. Now they only make the device
     suspect a reset: it stops posting and rotating for that channel and keeps
     the group. The group is replaced only just before joining a PublisherGroup
     Welcome for the same group id whose sender is a device of the owner (the
     key derived from the seed on an owner device, the key recorded in the
     subscription register on a co-publisher's), and the provider is copied
     first and restored if the join fails. These signals do nothing on the
     account's only device, and on web a refusal caused by clock skew does not
     count. A suspicion lifts by itself after signed reads under the held
     group's current epoch key succeed again: web after one read whose DS
     latest epoch equals the group's, Android after two such reads in a row,
     iOS after two reads in a row with no unopened journal row framed past the
     group's epoch (iOS has no path that reads the DS latest epoch). The rules
     differ; each refuses on a refused read, which is what a real re-claim of
     the journal under another root produces.
   - *Android decision A reseal.* A private channel with no admission seed
     reseals only to the account's own reader key and leaves the other rows to
     the admission pass, as web and iOS do; only a channel that was never
     private reseals to every row.
   - Recorded, not changed: decision A judges "the account's only device" from
     the local DeviceGroup roster, so a device whose roster lags re-founds
     while another owner device still holds the group; that device then
     suspects a reset, becomes a reader and is admitted by the re-founded
     group, so the cost is one re-found and a reseal, not a lost channel. The
     peer gate still does not compare a committer's update-path leaf node with
     its old leaf (item 1 of S1 to S3). The surface joins G-39.
   - Suites after the changes: web vitest 8312 passed, iOS `LakkoTests` 5309
     with 0 failures (2 skipped), Android 5702 with 0 failures (1 skipped,
     older). Not walked after the changes.

## Retry and epoch-mapping rules (2026-11-20)

Seen on an iPhone during a live walk: the reseal of a recorded rotation was
refused `401 owner-proof-required` (the owner key the service holds was not
the one the device signed with; see ADR-0074, note of the same day). The
entry was kept and "retried later", but every publisher pass also staged,
merged and journalled one more rotation, so one entry piled up per pass
(154 in three hours), and every replay walked all of them back to back.
Built the same day on web (and the desktop app), iOS and Android,
host-tested only, not walked:

- **A refusal holds, a transient failure backs off.** A channels-service
  answer of `401 owner-proof-required` or `410 channel-deleted` holds the
  channel's recorded commits, the subscriber admission pass and the
  account's reader-key registration until the next start (or account
  binding). Any other failure waits 30 s, doubling to 10 minutes (the
  schedule of the owed DS registrations). A replay stops a channel at its
  first failure, so a pile of entries costs one request per channel. The
  entries are kept: their commit merged, so the reseal (and an approve's
  Welcome) are still owed.
- **The journal step is never held.** An entry whose next step is the DS
  journal is sent every pass, as section 3 says; only a failure after the
  journal holds the channel.
- **No new rotation while a commit is owed.** A scheduled or requested
  rotation is not staged while a recorded commit of the channel still owes
  its journal step or its reseal. That reseal, when it lands, seals a seed no
  subscriber holds yet to the subscribers listed then, so a forced rotation
  (a removed subscriber) is covered by it. An entry owing only a Welcome,
  an appointment or a hand-over does not hold a rotation.
- **The reseal anchors the epoch mapping.** "Channel epoch = MLS epoch +
  offset" assumes one reseal per commit. A landed reseal now sets the offset
  exactly (target epoch minus the group's MLS epoch), keeps the resealed seed
  at that epoch (overwriting one kept there) and drops kept seeds above it.
  A device whose group is more than one epoch ahead of the service re-maps
  its current epoch to the service's next one (or to the service's epoch
  when the seed it keeps there is already this one). Web stores and learns a
  negative offset, as iOS and Android did.
- Residual: a co-publisher or other owner device that applies a whole run of
  unresealed commits only after the recovering reseal landed, and does not
  hold that seed, maps one epoch too high until the next landed reseal.

## Plan

No server changes. The DS commit route (ADR-0081), the same-root reset
(ADR-0057), the knock and the channels service's routes already do what this
ADR needs; S1 confirms it by test, not by a change.

**S1. Core: `lakko-mls`.** The PublisherGroup opened and founded with a
device identity and the owner's account seed; `open_or_create` deleted;
`is_owned_by(owner_account_seed)`; `stage_rotate`, `stage_remove_publisher`,
`stage_approve_external_join`, `stage_admit_devices`, `stage_retire_devices`,
the eager forms deleted; the PublisherGroup commit gate; `remove_publisher`
keyed on the owner's account. Must prove (property tests in
`tests/publisher_group_devices.rs`, all section 9 items): distinct leaves per
device and one channel seed; a lost race abandons to N with other groups
untouched; two owner devices staging on one epoch converge on one epoch and
one seed; a retired device cannot derive the next seed; no owner-account leaf
is ever removed by `remove_publisher`; bad binding, foreign account and the
ninth leaf refused locally and by every peer; a co-publisher's commit refused
by every peer; `is_owned_by` per role; re-founding at the same id after the
old state is deleted yields a group the old device cannot read. First
confirms that the `/g/` capability issuer is the owner's account key.

**S2. Core: `lakko-contacts` and the DS check.** `publisher_device_request`
and `channel_admission_mark`: codec, dispatch kinds, fuzz targets
`publisher_device_request_decode` and `channel_admission_mark_decode`
(compiled and run for a stated budget), the cross-kind isolation tests. A
DB-gated DS test shows two owner devices' commits on one PublisherGroup epoch
get `201` and `409`, an identical re-send `200`, and a same-root reset drops
the chain the old device holds.

**S3. FFI.** Both surfaces (UniFFI, wasm) for S1 and S2, appended `FfiError`
variants only if a new refusal needs one; the extension surface recognises
the two envelopes to suppress them; bindings, wasm, xcframework and Android
`.so` rebuilt. Must prove: an FFI test per surface where two owner devices
both admit a third and exactly one commit lands, and a co-publisher's device
request crosses the boundary and is applied.

**S4. Clients: web (and desktop), iOS, Android in one pass.** The
PublisherGroup in the device provider; create-channel founds with the device
identity; the reconcile of 2.1 and the request of 2.2; the staged commit
through the commit route with the owed verdict and the reseal after the merge;
the rotation of section 4 with the unfinished-reseal takeover; the
bookkeeping of section 6; the re-founding and the returning old device of 2.5;
the UX of section 8 (two catalog keys in all languages). Must prove: unit
suites green on each client; then a walk on two devices of one owner (the
second posts after the first opens Lakko; both run, a scheduled rotation
lands once; Forget device on the first retires it and the second keeps
posting), a co-publisher's second device admitted after the owner opens
Lakko, and a phrase restore with no other device that posts at once while an
old device that comes back reads and is then admitted.

**S5. Docs and review.** ADR-0064 section 7, G-54 closed, C-90 / C-91
indexed, `second-device.md` section 4a, PRODUCT-READINESS row for channels on
a second device; `lakko-security` pass on S1 to S4.

## References

- ADR-0050, ADR-0081, ADR-0045, ADR-0057, ADR-0060, ADR-0074, ADR-0078 phase 6,
  ADR-0086, ADR-0087 sections 7, 12, 15, ADR-0030, ADR-0064 section 7
- `docs/security/known-gaps.md` G-51, G-54
- RFC 9420 sections 5.3, 12.4
