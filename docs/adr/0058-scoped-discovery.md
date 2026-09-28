<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0058: Who can find me, and why knowing someone is not permission to reach them

- **Status:** Proposed
- **Date:** 2026-09-28
- **Context:** Phase 3 (product); UX spec [`docs/ux/who-can-find-me.md`](../ux/who-can-find-me.md)
- **Amends:** [ADR-0048](0048-profile-names.md) (profile envelope v3, username
  verification), [ADR-0053](0053-ds-journal-authorisation.md) (identity mailbox
  writes stop being open), [ADR-0010](0010-channels-two-tier-broadcast.md)
  (channel members visible to members), [ADR-0005](0005-contact-invitations.md) /
  [ADR-0028](0028-group-join-ticket.md) (links carry a write grant)

## Context

The maintainer's requirement, in full in the UX spec section 1: a person
chooses who can find them by `@username` **or by name** - nobody; contacts of
their contacts; members of all, or of chosen, groups and channels they are in;
or everyone - and **knowing a person's username, account id or any other
identifier does not let you reach them**. Only the setting (through search), a
link or QR code from them, or being their contact does. Default for a new
account: contacts of contacts plus all groups and channels. Channel members
become visible to one another, as in a group.

What exists today, read from the source:

- `POST /discovery/lookup` maps `OPRF(username)` to an `account_id` for
  anyone (`services/discovery/src/registry.rs`). There is no name lookup.
- `GET /discovery/identity/{account_id}` and `/discovery/keypackage/{account_id}`
  are open, and every profile envelope carries the sender's `account_id`
  (ADR-0048, version `0x02`). So every co-member of every group already holds
  enough to build a contact request or a Welcome.
- `POST /ds/mailbox/{pseudonym}/messages` is open by design (ADR-0053: "the
  cold inbox strangers are meant to reach"), and the pseudonym is derived from
  the public identity key. Contact requests, contact acks, and every Welcome
  that adds a person to a group (direct add, ADR-0045 sweep, ADR-0028
  co-publisher) arrive through it.
- ADR-0048 shows a username only after verifying it through the public lookup,
  which a hidden person would no longer answer.
- Channel subscribers never learn of one another (ADR-0010): the publisher
  reseals the seed to subscriber keys, and the only thing a subscriber sends
  is its registration.

So the requirement is not a filter on search. The gate has to sit on the
mailbox, and the server must enforce it without learning who is whose
contact or who is in which group (invariant #5).

## Decision

**An identity mailbox accepts a write only when it is signed with a reach key
the owner registered. Reach keys are handed out, never looked up: to contacts,
to link holders, to the members of groups and channels the owner lets find
them (inside the group, in the profile envelope), to contacts of contacts
(relayed by the owner's contacts, sealed under the searched name), and to
everyone (the public record). Search is how a reach key is found, and the
server holds public entries only for people who chose Everyone.**

### 1. Reach keys

A reach key is an Ed25519 key pair. Every seed is derived from the account
identity seed, so all of a person's devices compute the same keys with nothing
to transfer but version counters (section 7):

`reach_seed = BLAKE3-derive-key("lakko/reach/v1", account_seed || class || scope_id || version)`

| Class | `scope_id` | Held by | Rotated when |
|---|---|---|---|
| `contact` | empty | every contact (sent in the 1:1 conversation) | a contact is removed |
| `link` | the invite capability id | whoever holds that link (inside the capability) | the link is revoked or expires |
| `reply` | a random 16-byte id | the one party a request of the owner's went to | single use, 7-day expiry |
| `group` | `group_id` | members of a group the owner lets find them | the group shrinks, or the owner switches the group off |
| `member` | `group_id` | every co-member of every group, whatever the setting | the group shrinks |
| `channel` | `channel_id` | members of a channel the owner lets find them | each channel seed epoch, or switched off |
| `fof` | empty | contacts of contacts (section 4) | a contact is removed, and on a schedule |
| `own` | empty | the owner's own devices | never (derived, never registered with a peer) |

A **shared** key per class, never one per contact: a per-contact key would
tell the server how many contacts a person has.

`member` exists because co-members must be able to deliver a Welcome for the
owner's new device (ADR-0050 re-admission) in groups the owner hides from;
the Residuals section records what it costs.

### 2. The mailbox gate (`services/ds`)

- `PUT /ds/mailbox/{hex}/reach`, signed by the identity key (ADR-0053's
  mailbox proof): adds and removes reach public keys, each with an optional
  expiry, and sets `open` (true only in Everyone). A claimed mailbox starts
  closed. Every client claims its mailbox at signup already (ADR-0053).
- `POST /ds/mailbox/{hex}/messages` on a claimed, closed mailbox requires an
  `x-lakko-ds-proof` signed by a registered, unexpired reach key, or by a
  delegated key (below); the nonce is consumed as in ADR-0053.
- **The refusal must not say whether an account is behind the pseudonym.** A
  write with **no proof** to a closed mailbox is answered `202` and dropped,
  the same answer an unclaimed mailbox gives. A write with a proof that does
  not verify is `401` everywhere, claimed or not. Only a writer who once held
  a valid key can learn that a key stopped working, and they already knew the
  person existed.
- **Delegation.** A reach-key holder may sign a delegation
  `(mailbox, delegate_pk, expires_at <= 24 h, nonce)`; the delegate writes
  once with it, the DS verifies the chain against the registered key and
  consumes the nonce. This is how an ADR-0045 committer who is not the
  person's contact delivers the Welcome for an add a contact proposed: the
  proposer puts a delegation for the committer in the group next to its
  proposal. And how a sweep delivers to a `/g/` joiner: the knock carries a
  delegation made with the joiner's `reply` key.
- `POST /ds/mailbox/{hex}/reach/check` with a reach-key proof: `204` if the key
  is registered, `401` otherwise. The searcher calls it before showing a
  result, so narrowing takes effect on the next search, not at the next
  rotation (UX section 8). Rate-limited per address; no token (the proof is
  the gate).
- Rendezvous mailboxes (pairing, recovery, welcome courier) are unclaimed and
  stay as they are.

### 3. Groups and channels: the grant rides in the profile

Profile envelope **v3** (ADR-0048, flag day, v2 refused) adds an optional
**grant**: the username certificate (section 5), the `group` reach seed for
this group with its version, and the key version counter. A member's client
sends the grant only in groups the setting allows; everywhere else the
envelope carries the name and no username, which is what UX section 6.2 shows.
The `member` reach seed rides in every group's profile regardless.

- **Search in 2.2 is local.** A member's client matches the typed username or
  name against the profiles holding a grant, then runs the reach check. No
  server lookup, so the server learns nothing about who searched for whom.
- **Shrink**: the owner's client, which already detects a shrink by
  signature-key set difference (ADR-0043), bumps the `group` and `member`
  versions, registers the new keys, removes the old, and republishes the
  profile under the per-group lock - strictly after the remove commit, so
  the evicted member cannot open it (the C-61 shape).
- **Growth**: the ADR-0048 growth re-send already carries the profile to the
  newcomer.
- **Switching a group off**: remove the `group` key, republish without the
  grant.

**Channels get a member list.** A subscriber uploads a **member card** -
its profile v3, sealed under the channel's current epoch seed with the
`lakko-channels` post AEAD - to `PUT /v1/channels/:id/members/:subscriber_pk`,
signed with the subscriber key it registered; members fetch
`GET /v1/channels/:id/members` (paged). The server stores ciphertext per
subscriber key it already holds; it learns nothing new about who subscribes.
A card is re-uploaded after each epoch rotation (a newcomer holds only current
seeds), deleted on unsubscribe (a signed `DELETE`) and by the existing 90-day
subscriber prune. The `channel` reach key is versioned by the channel epoch.
Publishers upload a card like anyone else.

### 4. Contacts of contacts: relayed, sealed under the searched name

When 2.1 is on, the owner (Alice) builds one **introduction record**:

- `tag_u = BLAKE3-derive-key("lakko/fof/tag/v1", OPRF(username))`, and
  `tag_n` the same over `OPRF(normalised name)`;
- the payload - identity public key, `account_id`, name, username
  certificate, `fof` reach seed and version - sealed twice, under a key
  derived from `OPRF(username)` and under one from `OPRF(normalised name)`.

She sends it to each contact in their 1:1 conversation. Each contact's client
(Carol) sends her contacts (Bob) an **introductions bundle**: the records of
all her contacts who turned 2.1 on, padded to a size bucket. Bob, searching
`anna weber`, computes the OPRF once (one anonymous token, ADR-0054) and
matches the tag locally, opens the record, runs the reach check, and shows the
result.

- **Why the OPRF.** Usernames and names are low-entropy. Keyed on the plain
  string, a bundle would let Bob recover Carol's contacts offline from a
  dictionary. Keyed on the server's OPRF, each guess costs an online,
  token-rationed evaluation, the same cost as today's public lookup.
- **Narrowing.** Alice removes a contact: new `fof` version, the old key
  unregistered at once (so the reach check fails immediately), a new record to
  the remaining contacts. Carol removes Bob: Bob keeps Alice's record until
  Alice's scheduled rotation, then his refreshed bundles stop carrying her.
  The schedule is 7 days, chosen and not measured.
- **Widening spreads as contacts' devices come online**, which is the UX
  section 8 "once your contacts next open Lakko" state; the client counts
  acknowledgements from its contacts to know when to clear it.

### 5. Everyone, username verification, and names on the server

- **Everyone** sets `open` on the mailbox and publishes in discovery: the
  username binding (unchanged table, gains `public BOOLEAN`), and
  `display_names(oprf_hash BINARY(64), account_id BINARY(16), PRIMARY KEY
  (oprf_hash, account_id))`, one row per account. `POST
  /discovery/name/lookup` returns up to 20 account ids for one hash (a cap
  chosen, not measured). The name is on the server only as an OPRF output,
  never plaintext (invariant #1), and matching is exact over the normalised
  name, which is what UX section 6.1 specifies.
- **Leaving Everyone** clears `public`, deletes the name rows and the `open`
  flag. `lookup` of a non-public username answers exactly as for an
  unregistered one.
- **Username certificate.** At registration, discovery signs
  `(OPRF(username), account_id)` with a dedicated Ed25519 key (published at a
  well-known path and pinned in the clients, the ADR-0015 pattern). A viewer
  verifies a claimed username by one OPRF evaluation plus the certificate,
  with no lookup - so ADR-0048 verification keeps working for people who do
  not answer lookups, and no endpoint confirms a guessed username for a known
  account id.
- **Name normalisation** (NFKD, drop combining marks, case-fold, collapse
  whitespace, trim) is one function in `lakko-contacts`, on both FFI surfaces;
  every client and the three search paths use it.

### 6. Links and requests

- An invite capability (ADR-0005) and a `/g/` capability (ADR-0028) gain a
  `link` reach seed, so a link holder can write the contact request, or so a
  sweep can deliver a group Welcome. Revoking the link removes the key.
- Every outgoing contact request carries a delegation made from a fresh
  `reply` key, so the accept (the 1:1 Welcome and the ack) can come back.
- A contact accept hands over the `contact` reach seed inside the new 1:1
  conversation.

### 7. The setting itself

A bounded `lakko-contacts` envelope carrying the position, the two parts, the
all/chosen form, the chosen group and channel ids, the last-parts memory (UX
section 3) and every reach key version counter, sent over the DeviceGroup so
all of the person's devices agree (the reserved DeviceGroup fields of
ADR-0050, which this makes the first incremental sync). Ordered by a strictly
increasing stamp (the ADR-0044 lesson: a re-send carries its original stamp).
The UI transition rules of UX section 11 live in `lakko_ui_rules`.

## Alternatives

- **A filter on the recipient's device.** Rejected by the requirement: the
  write still lands, and a sender who knows the account id still reaches the
  device.
- **Server-side access lists** (the owner uploads their contact list and group
  memberships, the server evaluates who may write). The social graph on the
  server: invariant #5.
- **Scoped server records** (a lookup record per group keyed by a group
  secret). Considered first and dropped: the server sees how many scopes
  each person has and when each is searched, and it adds a lookup that the
  in-group grant makes unnecessary.
- **One reach key per contact.** Tells the server the contact count.
- **Private set intersection for contacts of contacts**, so the searcher does
  not learn which contact knows the person found (Residuals). Each search
  would need every one of the searcher's contacts online. Kept as the upgrade
  path, not built.
- **A plaintext name index with prefix search.** The OPRF gives exact name
  search without plaintext on the server.
- **Re-deriving the group reach key from the MLS exporter at every epoch**
  (the ADR-0053 model). Automatic revocation, but the owner must re-register
  at every commit of every group and a newcomer is unreachable until they do.
  The shrink-rotated grant needs a registration only on shrink.

## Consequences

- **Positive:** knowing an identifier grants no reach, enforced by the DS
  before anything is stored; hidden people keep verified usernames; names are
  searchable without plaintext on the server; no graph on the server.
- **Negative:**
  - A closed mailbox makes every non-contact write path depend on a key:
    contact requests, acks, direct adds, sweep Welcomes, co-publisher
    Welcomes, device re-admission. Each needs its key or delegation wired, on
    all three clients.
  - Rotation traffic: every shrink of every group costs the owner a
    registration and a profile republish per member; every channel epoch a
    card upload per subscriber.
  - Channel member lists are new server state and paging work for large
    channels.
- **Neutral:** profile v3, capability and envelope format bumps - a flag day,
  acceptable with no deployment.

### Invariants

- **#1** held: names only as OPRF outputs, only for people who chose Everyone.
- **#2** held: reach keys are public keys; seeds never leave E2EE carriers.
- **#3** untouched: no `lakko-mls` change.
- **#4** held: the mailbox refuses; a username that fails its certificate is
  not shown.
- **#5** strengthened (no stranger reach), with the residuals below.

## Residuals: what the server and peers still learn

- **The server** learns whether a person is in Everyone; how many reach keys a
  mailbox holds (roughly the number of groups and channels in 2.2, plus live
  links); which key signed each write and when. Padding the key set to
  buckets is open question 1.
- **A searcher who finds someone through 2.1** can tell which of their
  contacts' bundles held the record - so which of their contacts knows the
  person. Inherent to relaying; the PSI alternative removes it.
- **A contact** can pass their key to anyone. As today with a username; fixed
  by the rotation on removal.
- **`member`**: a co-member the owner hides from holds a key that the DS
  accepts. The owner's client drops anything under a `member` key that is not
  a Welcome for that group, so it never shows - but it is stored, which is
  weaker than the requirement's "before it reaches them" for that one case.
- **Contacts of contacts** lose reach only at the next scheduled rotation when
  it is the intermediary who removed them.

## Open questions

1. Pad the reach-key set per mailbox to fixed buckets?
2. The 7-day FoF rotation, the 20-result name cap, the 24-hour delegation
   expiry and the 7-day reply expiry are chosen, not measured.
3. The channel member list for very large channels: paging, and whether a
   member list is fetched whole or searched.

## Plan

- **S1 core** (`lakko-contacts`, `lakko-channels`, `lakko-ds-proof`,
  `lakko-ui-rules`): reach derivation, delegation, profile v3, introduction
  record + bundle, setting envelope, name normalisation, username certificate
  verify, member card; property tests; fuzz targets for every new decoder.
- **S2 services**: DS reach registry + gate + check + delegation; discovery
  `public`, `display_names`, name lookup, certificate signing key; channels
  member cards.
- **S3 FFI**: UniFFI + wasm, and the extension surface (profile v3 decode).
- **S4 web reference**, **S5 iOS + Android**: the setting screen, local and
  network search, every mailbox write path signed, rotations.
- **S6** `lakko-security` pass (invariants #1/#5, auth-adjacent DS change)
  and the docs reconcile.

## References

- UX: [`docs/ux/who-can-find-me.md`](../ux/who-can-find-me.md)
- ADR-0005, ADR-0010, ADR-0028, ADR-0043, ADR-0044, ADR-0045, ADR-0048,
  ADR-0050, ADR-0053, ADR-0054
- Privacy Pass / OPRF: RFC 9497
