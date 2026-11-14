<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0064: Conversations come back on another device: saved history, and a device that re-enters by itself

- **Status:** Accepted (maintainer, 2026-10-02). Nothing is built yet. The
  maintainer set the direction the same day:
  - history is saved per conversation and channel, always, and is not linkable
    to the account;
  - a restored device asks the members to let it in and enters by itself at
    the same time; whichever lands first counts;
  - signing in with the recovery phrase adds a device and never disconnects
    the others;
  - one new visible state, *waiting*, inside the conversation; no general
    label; no "start again" button;
  - the direct hand-over at pairing stays, carries the full message record,
    and covers the newest messages; the saved history covers the rest;
  - channels are covered, subscribed and owned;
  - on the server the objects describe themselves and a database indexes
    them, so the service can run as several instances.
- **Date:** 2026-10-02
- **Context:** account recovery, multi-device. UX spec:
  [`docs/ux/conversation-backup.md`](../ux/conversation-backup.md). Builds on
  [ADR-0050](0050-per-device-conversation-leaves.md) (per-device leaves,
  section 2.5a device admission, section 2.9a no MLS state in the backup),
  [ADR-0053](0053-ds-journal-authorisation.md) and
  [ADR-0057](0057-journal-ownership.md) (who may write a journal and register
  an epoch), [ADR-0045](0045-remote-commit-epoch-fork.md) (who commits),
  [ADR-0027](0027-external-join-approval.md) (the knock). Amends ADR-0027
  alternative 2 and ADR-0057's re-founding of a DeviceGroup.
- **External review:** this ADR changes the `lakko-mls` commit gate
  (invariant #3). The per-change half (this ADR plus property tests) is done in
  the sprint that builds it; the external review is tracked once, in G-39
  ([ADR-0046](0046-external-crypto-review.md)).

## Context

A person who signs in on a new device with the recovery phrase gets the
identity, the contacts and the settings. The conversations do not come back in
either sense:

1. **What was said.** No message history exists outside the devices. Each
   client keeps one blob per account under a key that never leaves the device
   (`message-log-store.ts`, `MessageLogStore.swift`, `MessageLogStore.kt`).
   The only transfer is the pairing history bundle, which needs another live
   device, carries at most 1000 messages in one journal row, and drops the
   author, reactions, replies, forwards, edits and deletes
   (`history_placeholder.rs`). Channel subscriptions, subscriber keys and posts
   reached no second device by any path (since 2026-11-13 the subscriptions
   and the shared subscriber key travel in the `lakko-sync`
   channel-subscription register; posts still do not).
2. **Being in the conversation.** The device is brought in by a member that is
   online (ADR-0050 section 2.5a), which checks for new devices every ten
   minutes. A conversation whose other members are away, or which has no other
   member, stays closed to the person for as long as that lasts.

A third defect sits beside these. After a phrase sign-in with no local state,
every client founds the account's DeviceGroup again and resets its journal
(`devices/lifecycle.ts`, `DeviceGroupLifecycle.swift`,
`DeviceGroupLifecycle.kt`; ADR-0057 "a re-found resets the journal"). That
cuts the person's other devices off the DeviceGroup. The phrase is what a
person has at hand when away from their other devices; using it must not
disconnect them.

Constraints:

- **Invariant #2.** The server stores ciphertext only.
- **Invariant #5.** The server must not learn which conversations an account
  is in. The storage service's backup route is named by the account id and
  authenticated by the session token, so anything stored there per
  conversation would hand the server the account's conversation count and each
  conversation's activity, which it could match against journal activity on
  the Delivery Service.
- **Invariant #3.** Any change to who can advance a group's epoch is a change
  to the MLS state machine.
- **No production data.** Flag day; no migration.

Facts the design rests on (read in the tree, 2026-10-02):

- Every device of an account holds the account identity seed (ADR-0017), so a
  key derived from it is available to every device and to a restored one.
- `services/storage` is files only. `/blob` is anonymous, immutable, never
  deleted. `/backup` is the only mutable object and is account-named.
- openmls 0.8.1 exports a GroupInfo with the ratchet tree and external public
  key (`MlsGroup::export_group_info`; every commit also returns one) and
  builds an external commit (`MlsGroup::external_commit_builder`). An external
  commit merges eagerly and may carry only ExternalInit, one Remove, PSK and
  Custom proposals inline. `lakko-mls` uses neither today and discards the
  GroupInfo every commit returns.
- In a role-table group every external commit is refused today:
  `authorise_role_commit` resolves the sender to no member leaf and
  `authorise` returns `NotAMember`. In a group with no role table
  (`process_incoming`, `process_incoming_gated`: DeviceGroup, PublisherGroup,
  call groups) an external commit is merged with no Lakko check.
- Registering epoch N+1 on the Delivery Service needs the journal's root seed
  or a registrar seed, and exactly one Append of N+1 succeeds
  (`services/ds/src/ownership.rs`, `epoch_rule`). A registrar seed is per
  account. The root seed derives from the owner's account seed.
- A non-member reaches a group's members through the knock
  (`POST /ds/group/{hex}/knock`, one anonymous token, response carries the
  latest registered epoch); members accept only an external-join proposal
  there (`Group::process_knock`).

## Decision

### 1. The vault

A new kind of object on `services/storage`, one per conversation or channel,
that no request ties to an account.

**Names and keys.** From the account identity seed:

```text
vault_root  = BLAKE3-derive-key("lakko/vault/root/v1", account_identity_seed)
vault_id    = BLAKE3-derive-key("lakko/vault/id/v1",    vault_root || ref)   (32 bytes)
vault_key   = BLAKE3-derive-key("lakko/vault/key/v1",   vault_root || ref)   (AEAD key)
vault_write = BLAKE3-derive-key("lakko/vault/write/v1", vault_root || ref)   (Ed25519 seed)
```

`ref` is a tagged, self-delimiting encoding: `0x01 || kind(1) || id_len(1) ||
id` for a conversation (the ADR-0058 conversation ref, which already names
one-to-one chats, groups and channels; id 1 to 64 bytes), `0x02` for the
account's index and `0x03` for its device group (below). The tag keeps the
three shapes apart, and the length byte lets the same encoding sit inside a
record. Two vault ids of one
account are unrelated to each other and to the account id for anyone without
`vault_root`. A vault id is never sent to another person.

**Objects.** A vault holds a **head** (small, rewritten often) and numbered
**chunks** (earlier messages or posts). Every object is sealed with
ChaCha20-Poly1305 under `vault_key`, random nonce, AAD
`vault_id || object kind || chunk number || version`, and padded to a size
bucket before sealing so the server does not see exact sizes.

**Server contract.**

| Route | Who | Rule |
|---|---|---|
| `PUT /vault/{id}` | anyone, one `vault-claim` anonymous token | first write wins; stores the write public key; `409` if claimed under another key |
| `PUT /vault/{id}/head`, `PUT /vault/{id}/chunk/{n}` | signed by the vault's write key | the body carries a version; accepted only at `current + 1` (`409` otherwise); a chunk number may be at most the current count |
| `GET /vault/{id}/head`, `GET /vault/{id}/chunk/{n}` | signed by the write key | uniform `401` otherwise, claimed or not |
| `DELETE /vault/{id}` | signed by the write key | removes every object |

The request signature is a header over method, path, body hash and time,
checked against a short window; a replayed write is refused by its version and
a replayed read returns ciphertext to someone who already saw the request, so
no nonce store is kept. No session token is accepted on these routes.
Per-vault limits (bytes, chunk count, writes per hour) are chosen at build
time and stated as chosen. A new anonymous token context `VaultClaim` (wire
value 8) rations claims per account without linking a claim to it (ADR-0054).
Nothing expires a vault; deleting the account deletes its vaults from the
client, which holds the keys.

**How the server keeps it: objects that describe themselves, and an index.**
The service must be able to run as several instances, and it must not have
two authorities that can disagree.

- **Objects are the truth for content, and are written once.** Every object
  is stored under a name that carries its version
  (`<vault id>/claim`, `<vault id>/head.v<n>`, `<vault id>/chunk.<k>.v<n>`).
  A name is never overwritten: a new version is a new object, and the older
  one is deleted afterwards. The claim object holds the write public key. So
  the files need no locking, and the whole vault state can be rebuilt by
  listing a vault's objects.
- **The index is the coordinator and the lookup, never a second copy of the
  truth.** One row per vault (write public key, bytes used, the write-rate
  counter, the storage node that holds it) and one row per object (current
  version, size). Every request is answered from the index: it gives the
  current version and the node, and the object's path follows from the vault
  id and the version, so nothing is found by walking directories. A write
  reserves version `current + 1` in the index (one conditional update; the
  loser gets `409`), stores the object, then marks the version current. A
  reservation whose object never arrived is released by a sweep. Directories
  are listed only to rebuild an index that was lost. The index also answers
  the operator's questions (how many vaults, how many bytes, which were not
  touched for how long) without touching the disks.
- **Two index implementations behind one trait**, the `ChannelStore` pattern
  of `services/channels`: MariaDB in deployment
  (`LAKKO_STORAGE_DATABASE_URL`), which is what several instances share and
  what makes the `VaultClaim` spent set durable through the shared
  `lakko-anon-token-server` gate; and a file-backed one (a small file beside
  the objects, exclusive create as the conditional update, the spent set in
  memory) for development, tests and a single instance with no database.

**Several instances without shared storage.** The stored bytes will be
dominated by attachments and may reach hundreds of terabytes, so one volume
shared by every instance is not the plan. Each storage node keeps its own
disks; a vault lives on one node, named in its index row; the instance that
receives a request reads the row and serves it or forwards it to that node.
Capacity grows by adding nodes, and new vaults are placed on the emptiest.
Today there is one node and the column holds one value. Attachment blobs get
the same index and placement in
[ADR-0065](0065-attachment-lifetime-and-access.md); replication of a node's
disks is a separate decision, outside both.

The version rule is the merge rule for several devices: a writer that meets
`409` fetches the object, merges (section 2), and writes again.

**The index.** The vault at `ref = "index"` lists the account's conversations
and channels: ref, last activity time, membership at save time. A device reads
it first, and it orders the loading: most recently active first.

**What the server learns.** That some client holds a vault of a given padded
size and how often it changes. Nothing in a request names the account, the
conversation, or which vaults belong together. Residual: a server that
correlates by network address and timing can group the vaults one device
touches in one sitting, exactly as the Delivery Service can already group the
journals one connection polls. Clients spread vault writes over time rather
than sending them in one burst; this narrows the residual and does not close
it.

**Rollback.** The server can serve an old head. For history that hides recent
messages; for re-entry (section 4) it makes the attempt fail and fall back to
the knock. Neither breaks confidentiality or lets anyone in. A device that has
seen a version refuses a lower one; a restored device has no floor. Recorded
as a residual, in the class of ADR-0037's zero-anchor case.

### 2. What is saved

One shared codec in a new crate `lakko-vault` (bounded, fail-closed, fuzzed,
on both FFI surfaces), because the three local message stores differ in shape
and a restored device may be a different platform from the one that saved.

**Conversation head** (version, then):

- conversation ref, kind (ADR-0062), title (the group name, ADR-0051; none for
  a one-to-one);
- the people: account key, display name and username snapshot (ADR-0048), and
  role, bounded; never device leaves;
- membership at save time: in / left / removed;
- shared and personal pins (ADR-0052);
- last activity time, the chunk table, and the newest messages not yet moved
  into a chunk;
- **re-entry material**, present only while the account is in the
  conversation: the GroupInfo of the **two newest epochs** this account's
  devices know, each with the ratchet tree and its epoch number, and the
  account's registrar seed for this journal when it holds one (ADR-0057).

**Message** (one record shape for the head tail and the chunks): message id
(the Delivery Service journal id), direction, author account key, time in
milliseconds, text, attachments (the `AttachmentRef` fields including the
thumbnail; references, not files: a file may have expired by the time the
history is read, ADR-0065), reply, forward, reactions keyed by account key, edited and deleted
flags, call outcome, unsupported-version marker, and `last_op_id`, the journal
id of the newest edit, delete or reaction applied to the row.

**Merge.** Rows are keyed by message id, which every device observes
identically. Union of rows; for one row the higher `last_op_id` wins. A
deleted row is stored empty, as the clients already do locally, and the chunk
holding it is rewritten, so a deletion reaches the saved copy.

**Who writes, and when.** Any device of the account, from a dirty-flag
driver like the existing auto-backup driver. Saving is always on; there is no
setting. Messages are never written one by one: a conversation that changed is
written as a batch on a slow tick and when the app goes to the background.
The tick is chosen at build time and stated as chosen. Two rules are about the
re-entry material:

- **Save before publish.** A device that is about to publish a commit first
  writes a head carrying the GroupInfo of the epoch the commit leads to,
  beside the current one. Whether the commit then lands or not, one of the two
  saved epochs is the group's real one.
- **Save on apply.** A device that applies somebody else's commit writes the
  new GroupInfo on its next tick.

A device that has just signed in does not write a vault before it has read it.

**What this costs, stated plainly.** History is sealed under a key derived
from the recovery phrase. Whoever obtains the phrase reads every saved
message, where today they obtain the identity and whatever arrives afterwards.
The ADR-0049 residual grows with it: an operator who defeats their own guess
counter on the three-word vault reaches the phrase and, through it, the saved
history. The maintainer accepted this on 2026-10-02 on the ground that the
phrase already yields the whole account.

### 3. Pairing: the live device hands over what is newest

The vault is written in batches, so it trails the devices by up to one tick.
A device added beside a live one therefore gets its history from two places:

- **Directly from the live device**, as today, over the pairing rendezvous and
  sealed under the DeviceGroup exporter: the newest messages of the most
  recently active conversations, so the first screen is there at once and is
  current to the last message.
- **From the vault**, for everything older and every other conversation, in
  the same order and steps as a restored device.

The direct hand-over stops being a separate, poorer format. It carries the
same `lakko-vault` message record (author, reactions, replies, forwards,
edits, deletes, pins) and the same conversation head fields, so the two
sources merge by the rule of section 2 and a row looks the same whichever
brought it. `history_placeholder.rs` and the three clients' placeholder codecs
are replaced by the shared record; the 1000-message count cap is replaced by
a byte budget that fits the rendezvous row, filled most recent conversation
first. The live device also flushes its pending vault writes when it pairs, so
the vault catches up without waiting for the tick.

The pairing state bundle (contacts, pins, known group ids) stays until those
have a place in the vault or the DeviceGroup sync.

### 4. A device asks to be let in and enters by itself

For every conversation the index names as "in", a device that holds no group
for it does two things at once.

**It knocks.** It posts an external-join proposal for its own device leaf to
the conversation's knock route (ADR-0027, one `ds-knock` token). A member
that reads a knock whose leaf carries a valid device binding to an account
already in the group does not queue it for an admin: it admits the device at
once as a device admission (ADR-0050 section 2.5a). This replaces the
ten-minute directory check as the way a present member learns of a new device;
the directory check stays as the backstop. A device with no token left skips
the knock and relies on the entry below.

**It enters.** In a conversation group (one that carries a role table), a
commit from a sender that is not yet a member is accepted when, and only
when, all of these hold, judged against the pre-commit state:

1. it is an external commit carrying exactly one inline proposal, the
   mandatory ExternalInit: no Remove, no PSK, no Custom, no by-reference
   proposal;
2. the leaf it adds carries a valid device binding (ADR-0050 section 2.2) to
   an account that already holds at least one leaf in the group;
3. that account holds fewer than `MAX_LEAVES_PER_ACCOUNT` leaves;
4. the leaf's capabilities cover the group's required capabilities.

It changes no GroupContext extension, so the role table (C-63), the kind
(C-70) and the one-to-one rule hold by construction; the gate asserts that
rather than assuming it. Every other external commit is refused, as today.

This is ADR-0050's device admission committed by the admitted device. The
person is already a member; nobody new enters. ADR-0027 rejected an external
commit as a way for a **new member** to join without an admin, and that
rejection stands.

The device, per conversation, for the newer saved epoch and then the older:

1. Build the external commit from the saved GroupInfo in a scratch provider.
   The device is now at epoch N+1 locally and nowhere else.
2. Derive the N+1 journal access key and register epoch N+1 on the Delivery
   Service by Append, signed with the root seed (the account owns the group)
   or the saved registrar seed.
3. `409` or `403`: this saved epoch is not the group's current one. Discard
   the scratch state and try the other saved epoch; with none left, the
   conversation waits for a member to answer the knock.
4. Registered: record the commit durably (the ADR-0040 pattern), publish it to
   the journal under the N+1 key, adopt the group, and write a fresh head.
5. A publish refused with `401` means a higher rank replaced the registration
   because its own commit won: treat as step 3.

**First to land counts.** The Append of N+1 decides: one registration wins.
If a member answering the knock registers first, the device's own Append is
refused and the member's Welcome arrives. If the device registers first, the
member's sweep meets `409`, re-reads the journal, finds the device already in
(a pending knock for a present leaf is dropped, as `pending_external_joins`
already does for members), and has nothing to do. Nobody judges whether anyone
is "inactive".

**Why this needs no approval.** Entry needs the saved GroupInfo (sealed under
the phrase-derived key) and a device binding signed by the account key. A
stranger holding a leaked GroupInfo cannot produce the binding. A person
removed from the group while their devices were away holds stale GroupInfos
and is refused at step 3; a commit they publish anyway is at the wrong epoch
and every member drops it; their knock names an account no longer in the
group and goes to the ordinary join queue. A live member's admission of a
knocking device is automatic today already; entering without one adds no
power the phrase did not have.

**Sole person.** When the account is the only one in the conversation, only
its own devices can change the group, and save-before-publish means one of
the two saved epochs is always the real one. Entry therefore succeeds and
there is no "start again" action.

**Dead leaves.** The entering device cannot remove another leaf in the same
commit (rule 1; the openmls external builder offers none either). Leaves of
devices the person no longer has are retired when the person removes those
devices on the Devices screen, as today. An account at the leaf cap is told
so in the waiting note (UX 2.5).

### 5. The account's own device group

Signing in with the phrase **adds a device**. The DeviceGroup is not founded
again when one exists.

- The vault at `ref = "devices"` holds the DeviceGroup's two newest GroupInfos
  under the same save rules. The root seed of its journal derives from the
  account seed, so no registrar seed is needed.
- A device with the phrase and no DeviceGroup state enters the DeviceGroup by
  an external commit. The DeviceGroup's gate accepts it when it carries the
  ExternalInit alone and its leaf is bound to the same account as the leaves
  already there; everything else is refused.
- It also announces itself to the account's other devices through the
  DeviceGroup's knock, and a device that is running admits it, for the case
  where both saved epochs are stale.
- The DeviceGroup is founded (and its journal claimed) only when the
  `devices` vault does not exist: the account's first device.
- ADR-0015's attested-removal policy is about removals and is untouched. The
  Devices screen lists every device again after a phrase sign-in, including
  ones the person no longer has, so they can be removed.

**Every other group without a role table refuses every external commit.**
`process_incoming` and `process_incoming_gated` gain that refusal, with the
DeviceGroup arm above and the PublisherGroup arm of section 7 as the only
exceptions. It closes the unchecked merge that exists today.

### 6. One new state: waiting

A list row no longer requires a live MLS handle; it can be built from a vault
head. A conversation the device can read but not yet write is **waiting**,
shown inside the conversation in place of the composer and nowhere else (UX
2.2). "You left" and "You were removed" stay as they are and are also drawn
from the head when no group is held. The "Synchronizing..." label of ADR-0050
is removed on every route.

### 7. Channels

**Subscribed.** A channel's vault head holds the account's subscriber key
(one per account and channel, shared by its devices), the retained epoch
seeds, the channel name and description (ADR-0051), and the newest posts;
chunks hold earlier posts. A subscriber key is not ratchet state: two devices
holding one is safe. A device reads the channel at once and needs no new
subscription.

**Owned or co-published.** The head also holds the account's publisher
signing seeds and re-entry material for the PublisherGroup. The device enters
the PublisherGroup by an external commit under the same rule as section 5
(ExternalInit alone; a leaf bound to an account already among the publishers).
Entering advances the epoch and so changes the channel seed, so the entering
device then runs the existing reseal fan-out to the subscribers (the ADR-0028
"reseal after add" rule), without a second rotation.

**Amended 2026-11-13 (maintainer), ahead of this ADR's build.** The channel
list and the shared subscriber key no longer wait for the vault: they travel
in the `lakko-sync` channel-subscription register (ADR-0050 section 2.9 as
amended that day), carried between the account's devices and in the backup;
the pairing state bundle hands over the newest epoch seeds once. The
subscriber key is, as written above, one per account and channel, shared by
its devices. The "owned or co-published" paragraph does not hold yet: a
PublisherGroup leaf is the account key, so a second device of the same
account cannot enter by an external commit without removing the first
device's leaf. Until PublisherGroup leaves are per device (G-54), every
device of an owner or co-publisher but the one holding the PublisherGroup is a
reader of the channel (UX `second-device.md` section 4a).

**Amended 2026-11-14 (maintainer), after [ADR-0090](0090-publisher-group-device-leaves.md).**
PublisherGroup leaves are now bound devices, so the "owned or co-published"
paragraph above becomes buildable, and it is built ahead of the rest of this
ADR, for owned channels only. The case it closes: an owner's device that
holds no PublisherGroup (it was added after the publishing device last ran,
and that device is gone; or decision A cannot fire because the account has
other devices) waits forever, since only a device holding the group can admit
it. The maintainer chose entry from a server-held GroupInfo over a manual
"take back posting" re-founding.

1. **Only the owner's devices enter.** A co-publisher's new device is admitted
   by an owner device (ADR-0090 decision B); a co-publisher never enters by an
   external commit.
2. **The vault.** Each owned channel has a vault at the channel's conversation
   ref under the owner account's vault root (section 1). Its head is a
   `ConversationHead` whose `reentry` holds the two newest PublisherGroup
   GroupInfos (with the ratchet tree and the external public key), and no
   registrar seed: the journal root derives from the owner's account seed
   (ADR-0057). Posts and seeds are not put in it by this amendment.
3. **Who writes, and when.** Every owner device that holds the group: before
   it sends a PublisherGroup commit through the DS commit route (ADR-0081), it
   writes the head with the GroupInfo of the epoch the staged commit moves to;
   after it applies a commit another owner device made, it writes the head with
   the current GroupInfo. A write that meets `409` reads the head, keeps the two
   newest epochs of both, and writes again. A failed head write does not stop
   the commit: the commit is what makes the channel work, the head only lets a
   device that lost everything come back, and a stale head degrades to the
   waiting line, never to a wrong group.
4. **The entry.** An owner device that holds no PublisherGroup for an owned
   channel reads the head and, newest saved epoch first: builds the external
   commit (ExternalInit alone, its own bound device leaf), sends it through the
   DS commit route signed with the journal root (the owner's seed), and on
   `201` adopts the group and reseals the new channel seed to the subscribers
   as after any admission (ADR-0090 section 5). `409` or a refused proof: try
   the older saved epoch; none left: wait as today. An owner device that holds
   the group and reads the entry commit applies it like any other.
5. **The gate.** `check_publisher_commit` accepts a commit from a sender that
   is not yet a member when, and only when, it is an external commit carrying
   exactly the ExternalInit, its leaf carries a valid device binding to the
   owner's account, and that account holds fewer than eight leaves. It changes
   no GroupContext extension. Every other external commit to a PublisherGroup
   is refused, as today (C-91 extended).
6. **Decision A** (ADR-0090) remains the fallback for an owned channel with no
   head (a channel created before this build, or a head that was never
   written).
7. Not covered: channels created before this build have no head (test
   accounts only, no production); a co-publisher whose every device is gone is
   admitted by the owner as today.
8. The head's reentry entry also carries the epoch's Delivery Service access
   seed (`Group::export_secret("lakko/ds/access/v1", ...)` at that epoch), so
   an entering device can prove the epoch-N read and send the commit without
   any server change; the head is sealed under the owner's vault key, so this
   gives nothing to anyone but the owner.
9. Cost accepted by the maintainer (2026-11-14): the entry commit is a
   PublicMessage (RFC 9420), so the Delivery Service reads the entering leaf's
   binding and with it the owner's account key next to the PublisherGroup
   journal. Recorded as G-55; closed the same day by wrapping the entry row
   under a key derived from the epoch-N access seed (ADR-0078 section 4).
10. A device that lost its PublisherGroup state but kept its signature key
    and whose leaf is still in the tree cannot enter (the resync Remove
    OpenMLS adds is refused by the gate) until another owner device retires
    that leaf. A reinstall loses the key with the state, so this needs a
    store failure that spares the identity.

**As built (2026-11-14), host-tested only, nothing walked or deployed.**
- `services/storage`: the vault routes of section 1 (`src/vault.rs`, a
  `VaultIndex` beside the blob index in both the MariaDB and file
  implementations, migration `0004`). A write body and a read answer are
  `version (u64 BE) || sealed object`, so the version is inside the signed
  body and a captured write cannot be replayed at another version. Object
  files are named by version and a random reservation token, so a writer
  whose reservation timed out cannot overwrite the one that took over; a
  reservation not committed within 60 s is taken over by the next writer (no
  sweep). Limits chosen: 64 MiB, 256 chunks and 240 writes per hour per vault.
  Not built: rebuilding a lost index from the files, forwarding to another
  node.
- `lakko-mls`: `PublisherGroup::enter`, `group_info`, `staged_group_info`, the
  external-entry arm of `check_publisher_commit` (and a refusal of any
  PublisherGroup commit that changes a GroupContext extension). OpenMLS 0.8.1
  merges an external commit while building it, so the entry is built in a
  throwaway provider, copied in and held by a provider row until
  `merge_staged_commit` / `abandon_staged_commit`. Lifetime validation is
  skipped when entering (residual 9): a co-publisher leaf never commits, so its
  KeyPackage lifetime would otherwise block every entry once it ran out.
- `lakko-vault`: a reentry epoch carries the epoch's Delivery Service access
  seed (item 8).
- Clients (web and desktop, iOS, Android): a vault client, the head writer
  (before every PublisherGroup commit, after applying another owner device's
  commit, at creation and after decision A), and the entry as a recorded
  PublisherGroup commit kind replayed after a restart, tried before decision
  A. The vault and the journal root are both named by the account's lineage
  identity, so a head survives an ADR-0074 key rotation. A head is rebuilt on
  each write with only its reentry carried over.
- Known: in a private channel the entering device reseals only the account's
  own reader key at once; admitted subscribers get the new seed on the next
  admission pass, as after decision A.
- The client deletes a channel's vault after the owner deletes the channel,
  and the vaults of every channel the account owns before the account is
  deleted (best effort, bounded in time: web 5 s, iOS 10 s, Android 15 s,
  chosen), as section 1 requires.
- Not built: the rest of this ADR (conversation history, the conversation and
  DeviceGroup entries, the index vault).
- `lakko-security` (2026-11-14): APPROVE WITH NOTES. Recorded residual
  (MEDIUM): a device removed from the account that keeps the lineage seed and
  runs a modified client can enter again, since removal changes no seed and
  co-publisher peers cannot see the owner's roster; it could already reset and
  re-found the channel with that seed. Mitigation: an owner device that applies
  another device's entry runs the own-device reconcile at once (C-91). Fixed:
  after decision A the head is replaced, not merged, since the re-founded
  group's low epochs would otherwise lose to the dead group's.

## Alternatives considered

1. **History inside the account backup.** One 4 MiB blob rewritten whole,
   account-named, loaded all at once. Rejected: size, and the loading order
   the maintainer asked for is impossible.
2. **Per-conversation objects on the session-authenticated backup route.**
   Simple, and it hands the server the account's conversation count and each
   conversation's rhythm. Rejected: invariant #5.
3. **Content-addressed blobs with a manifest in the account backup.** The
   blob route is immutable and never deletes: every change orphans a blob
   forever and a deleted message stays on the server. Rejected.
4. **Carry the MLS state in the backup.** Rejected in ADR-0050 section 2.9a.
   A GroupInfo is not that: it is the public description of one epoch, it
   holds no leaf's secrets, and using it creates a new leaf with its own key.
5. **Members first, entry only when nobody answered within a minute.**
   Fewer races with members who are changing the group at that moment, at the
   price of a minute's wait per conversation whenever nobody is online.
   Considered with the maintainer, who chose knock and entry at once.
6. **Entry only, no knock.** A stale saved epoch would then wait up to ten
   minutes for the directory check. Rejected.
7. **A setting to turn saving off.** Rejected by the maintainer: the phrase
   already opens the account, and a person who never opens Settings is the one
   who needs the history.
8. **Phrase sign-in makes the device the account's only one.** It is what the
   DeviceGroup re-founding does today and what a first draft of this ADR
   extended to the conversations. Rejected by the maintainer: the phrase is
   what a person uses when away from their other devices.
9. **Per-vault state in files only.** Enough for one instance, and the first
   draft. It cannot coordinate two instances and it loses the spent-token set
   on a restart. **A database as the only home of the state** makes the
   database a second authority beside the files. Both rejected for the
   self-describing objects plus an index of section 1.
10. **A "start again" action for a conversation only the person is in.** Made
    unnecessary by save-before-publish.

## Consequences

- A sign-in on another device shows earlier messages within seconds, most
  recent conversations first, and the device is in each conversation as soon
  as it has read the head, with no member online, unless the group changed
  after the last save and nobody is there to answer the knock.
- The recovery phrase now opens saved history.
- A phrase sign-in no longer disconnects the person's other devices.
- `lakko-mls` gains accepted external commits (invariant #3): one arm per
  group kind, property tests for each clause and each refusal, fuzz targets
  for the vault codecs, and a larger G-39 surface.
- `services/storage` gains signed vault routes, per-vault limits and its
  first database (an index that can be rebuilt from the objects); `auth`
  gains a token context; the three clients gain a vault writer and reader,
  rows without a group handle, the knock on sign-in, the knock answer, and
  one more field per stored message; the pairing history bundle moves to the
  shared record.
- Each account's devices upload a head per conversation per epoch change, and
  one before each of their own commits. In a large group the GroupInfo is
  tens of kilobytes; the write frequency is the group's membership churn. To
  be measured before the limits are fixed.
- A sign-in spends one `ds-knock` token per conversation. The quota must
  cover an account's conversation count or the knock is skipped for the rest.

## Residuals and open points

1. Network-address and timing correlation of vaults (section 1).
2. Rollback of a vault by the server for a device with no floor (section 1).
3. Both saved epochs stale and no member online: the conversation waits. This
   needs somebody else to have changed the group after the account's last
   save, so a member was recently active.
4. A person removed while every device was away sees the conversation as
   waiting, not as removed: nothing tells the device.
5. **To verify in the build, before the MLS sprint:** what each client does
   on a `409` when registering an epoch for a staged sweep. The rule to
   enforce is that a committer re-reads the journal before any Replace.
6. **To verify in the build:** that stored message rows name the author by
   account key on all three clients, not by device leaf key.
7. **To verify in the build:** that DeviceGroup and PublisherGroup leaves
   carry the device binding the two new arms judge by; if a PublisherGroup
   leaf is an account-key leaf, its arm compares that key instead.
8. Registering N+1 and then failing before the publish leaves the journal
   registered ahead of the group. The durable record republishes on restart;
   a device that never returns leaves the existing ADR-0057 wedge for an owner
   or admin to repair.
9. A saved GroupInfo whose tree holds leaves older than the KeyPackage
   lifetime needs lifetime validation skipped on the entering device; the
   gate on the members' side is unchanged.

## Sprints

1. `lakko-vault`: derivations, head, message and post codecs, sealing and
   padding, request signing, fuzz; both FFI surfaces.
2. `services/storage` vault routes, the versioned object layout and the
   two index implementations; `VaultClaim` context in
   `lakko-anon-token`, the `auth` quota, the client mirrors.
3. `lakko-mls`: GroupInfo export, the entry builder, the three commit-gate
   arms (conversation, DeviceGroup, PublisherGroup), the refusal everywhere
   else, the knock rule for a bound device; property tests; FFI.
4. Clients, writing: the vault writer and index, save-before-publish and
   save-on-apply, `last_op_id`, channels.
5. Clients, reading: sign-in and pairing read the vault; rows without a group
   handle; the waiting state; chunk loading; knock and entry; the knock
   answer; DeviceGroup entry in place of re-founding; the pairing hand-over
   on the shared record; removing the "Synchronizing..." label.
6. Owned channels (PublisherGroup entry and reseal); a `lakko-security` pass;
   documents.
