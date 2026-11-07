<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0060: Who can find me, and why knowing someone is not permission to reach them

- **Status:** Accepted (2026-09-28, the maintainer's call)
- **Date:** 2026-09-28 (numbered 0058 until 2026-09-29, renumbered because four
  parallel branches had claimed 0058)
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

**Removing a contact, and contacts who were offline (amended 2026-11-06,
maintainer decision).** Found live: a desktop account still held a phone's
`contact` grant after the phone had removed it; the Delivery Service accepted
the desktop's Welcome, the phone dropped it, and the sender saw one tick for
ever. The `contact` key stays one shared key per account; a per-contact key was
built and withdrawn the same day, because it tells the server the contact
count. Instead (flag day, no back-compat):

- **Remove contact rotates the `contact` key:** version + 1, the new key
  registered, and the profile carrying the new grant republished in every
  remaining contact's one-to-one, with nothing shown to anyone.
- **Retired versions stay registered:** the owner keeps the current version and
  up to `RETIRED_CONTACT_VERSIONS_KEPT` = 8 retired ones
  (`lakko_contacts::reach::contact_key_versions`; chosen, not measured), so a
  contact offline across up to eight rotations still writes. An older version
  is removed, and a write under it gets the uniform `401`: the sender sees a
  failure instead of a write accepted and ignored. The Delivery Service does
  not know classes; the bound is the set of keys the owner registers.
- **A mailbox row names the key that admitted it.** The DS stores, per mailbox
  row, the reach key a direct proof named or a delegation's issuer (DS
  migration `0016`, column `reach_pk`) and returns it to the owner's signed
  read as `reach_pk_b64` (absent for a proofless write).
- **The receiving device decides by one shared rule**
  (`classify_contact_key` + `contact_key_row_verdict`, one call
  `mailboxRowContactVerdict` on both FFI surfaces and the NSE surface): a row
  under the current key, or under any other class, is processed as before; a
  row under a retired `contact` key whose sender (resolved from the row's
  verified content: the contact request's from-key, the Welcome's founder
  binding, and so on) is a current contact is processed and answered by
  republishing the profile, with the current grant, in that one-to-one; from
  anyone else it is dropped, showing nothing and logging nothing that names
  anyone.
- **A Welcome is judged before it is joined (2026-11-07).** The main case is a
  removed contact founding a one-to-one: the row is a Welcome, and once
  joined an active group cannot be dropped silently. `Group::peek_welcome_founder`
  (`peekWelcomeFounder(welcome, identity)` on both FFI surfaces) stages the
  Welcome as a join does, returns the group id and the founder's account key
  from the sender leaf's verified ADR-0050 binding, and writes nothing; the
  KeyPackage stays usable, so the client joins afterwards only when the
  verdict says so.
- **No resend is asked of the sender.** The maintainer had proposed asking the
  sender to send again under the new key; the DS already accepted and stored
  the retired-key row, so the receiver holds it and simply processes it. Same
  outcome, one round trip fewer.
- **What the server learns that it did not:** nothing new about the writer (it
  verified the key before), but a copy of the database now shows, per mailbox
  row, the reach key that admitted it. All contacts share one `contact` key per
  version, so this tells a contact's row from a link's or a reply's and which
  `contact` version was used, not which contact wrote.
- **Residual:** a Welcome delivered by an ADR-0045 committer under a delegation
  signed with a retired `contact` grant (the proposer was offline across a
  rotation) names the committer as its sender. If the committer is not the
  owner's contact, the row is dropped and the add is not delivered until the
  proposer holds the current grant and delegates again.

`member` exists because co-members must be able to deliver a Welcome for the
owner's new device (ADR-0050 re-admission) in groups the owner hides from;
the Residuals section records what it costs.

**Versions after a restore (amended 2026-10-01).** The version counters lived
only on the devices (the local book and the DeviceGroup reach state); a device
restored with no other device alive started every class at version 0 and
registered it again, so a contact or co-member whose key had been rotated away
(a removed contact, an evicted member) regained write access to the mailbox.
The version must not be a counter that dies with the devices. The DS gains an
owner-only read of the reach keys registered on a mailbox (signed by the
mailbox's identity key, like the claim); a device that holds no version
state derives each class's keys for versions 0, 1, 2, ... and takes, per class
and scope, the highest version whose key is registered as the current one,
registering nothing lower. A class with no registered key starts at 0. The
read returns public keys only (invariant #2) and only to the mailbox owner.

### 2. The mailbox gate (`services/ds`)

- `PUT /ds/mailbox/{hex}/reach`, signed by the identity key (ADR-0053's
  mailbox proof): adds and removes reach public keys, each with an optional
  expiry, and sets `open` (true only in Everyone). A claimed mailbox starts
  closed. Every client claims its mailbox at signup already (ADR-0053).
- `POST /ds/mailbox/{hex}/messages` on a claimed, closed mailbox requires an
  `x-lakko-ds-proof` signed by a registered, unexpired reach key, or by a
  delegated key (below); the nonce is consumed as in ADR-0053. A reach proof
  names its signing key inside the signed bytes, so the DS verifies one
  signature rather than trying every registered key (amended 2026-09-28 after
  S2, the maintainer's call): proofs at `REACH_EPOCH` (`u64::MAX`) carry a
  sixth header field, `v1.<epoch>.<ms>.<nonce>.<sig>.<reach_pk>`, appended to
  the canonical bytes; any other proof carrying it, or a reach proof without
  it, is malformed. A delegate's proof names the delegate key.
- **The refusal must not say whether an account is behind the pseudonym.** A
  write with **no proof** to a closed mailbox is answered as a stored write
  (`201 {id, created_at}`, the id taken from the same sequence) and dropped,
  the same answer an unclaimed or open mailbox gives. A write with a proof that does
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
- **Rendezvous moves to its own namespace** (amended 2026-09-28 after S2, the
  maintainer's call). Pairing, recovery, the welcome courier and the state
  bundle use random ids under `/ds/rendezvous/{hex}/messages`, open, never
  claimable, stored in their own table (`ds_rendezvous_messages`, DS
  migration `0008`) so an id can never reach a personal mailbox, and refusing
  any proof, delegation or wake header. `/ds/mailbox/` becomes
  personal only, and a `GET` without a valid identity proof is `401` whether
  the mailbox is claimed or not: before the split an unclaimed mailbox had to
  stay readable for rendezvous, so a `GET` told anyone holding an identity
  key whether it had claimed its mailbox.

### 3. Groups and channels: the grant rides in the profile

Profile envelope **v3** (ADR-0048, flag day, v2 refused) adds an optional
**grant**: the username certificate (section 5), the `group` reach seed for
this group with its version, and the key version counter. A member's client
sends the grant only in groups the setting allows; everywhere else the
envelope carries the name and no username, which is what UX section 6.2 shows.
The `member` reach seed rides in every group's profile regardless.

- **Search in 2.2 is local.** A member's client matches the typed username or
  name against the profiles holding a grant, as loosely as its local search
  (any part of any word), then runs the reach check. No
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
signed with the subscriber key it registered (domain
`lakko/channel/member-card-sig/v1`); members fetch
`GET /v1/channels/:id/members` (paged). The server stores ciphertext per
subscriber key it already holds; it learns nothing new about who subscribes.
A card is re-uploaded after each epoch rotation (a newcomer holds only current
seeds), deleted on unsubscribe (a signed `DELETE`) and by the existing 90-day
subscriber prune. The `channel` reach key is versioned by the channel epoch.
Publishers upload a card like anyone else.

**A card is bound to the account behind it** (decided 2026-09-29 after S4/S5).
The sealed card plaintext is `identity_pk(32) ‖ sig(64) ‖ profile`, where `sig`
is the account identity key's Ed25519 signature over
`"lakko/channel/member-card-binding/v1" ‖ channel_id ‖ subscriber_pk ‖
BLAKE3(profile)`; a reader that cannot verify it drops the card. Without it
any subscriber could upload a card naming another account, and the reader
would file the card's grant and name under that account.

**A subscriber can unsubscribe.** `DELETE /v1/channels/{id}/subscribers/{pk}`,
signed by the subscriber key over its own domain
(`lakko/channel/unsubscribe/v1`), removes the row, its seed envelopes and its
member card. Before it there was no way to leave a channel on the server
short of the 90-day prune.

**A subscriber's key becomes Ed25519** (amended 2026-09-28 after S2, the
maintainer's call). The key a subscriber registered was the X25519 key of the
seed envelope, which cannot sign a card. A subscriber now holds one Ed25519
key pair; the seed envelope's X25519 key is derived from it by the standard
conversion, so the channel still sees one key per subscriber.

### 4. Contacts of contacts: relayed, sealed under the searched terms

**To be superseded by [ADR-0086](0086-circle-directories.md) (proposed 2026-10-30):**
the card and bundle relay below costs 390 MB to 8 GB a week per person and does not
meet the scale requirement. ADR-0086 also replaces the rotation rules of section 3
for groups and channels. This section describes what is built today.

When 2.1 is on, the owner (Alice) builds one **introduction record**:

- the **search terms**: the username, and every prefix of 3 to 16 letters of
  every word of the normalised name (a word shorter than 3 letters whole), at
  most 64 name terms (section 5, "Name search");
- the payload - identity public key, `account_id`, name, username
  certificate, `fof` reach seed and version - sealed once under a random
  content key;
- per term `o = OPRF(term)`: `tag = BLAKE3-derive-key("lakko/fof/tag/v1", o)`
  and the content key wrapped under `BLAKE3-derive-key("lakko/fof/key/v1", o)`.

She sends it to each contact in their 1:1 conversation. Each contact's client
(Carol) sends her contacts (Bob) an **introductions bundle**: the records of
all her contacts who turned 2.1 on, padded to a size bucket. Bob, searching
`anna web`, computes the OPRF of one query term (`anna`, the longest word;
one anonymous token, ADR-0054), matches the tag locally, opens the record,
filters by the whole query, runs the reach check, and shows the result.

- **Why the OPRF.** Usernames and names are low-entropy. Keyed on the plain
  string, a bundle would let Bob recover Carol's contacts offline from a
  dictionary. Keyed on the server's OPRF, each guess costs an online,
  token-rationed evaluation, the same cost as today's public lookup.
- **What prefix search costs** (the maintainer's call, 2026-09-28): a Bob with
  enough tokens can walk every 3-letter prefix and so list the contacts of his
  contacts who turned 2.1 on. The token quota is the only bound. Exact
  whole-name matching would have made that walk impractical and was rejected
  as poor UX.
- **Narrowing.** Alice removes a contact: new `fof` version, the old key
  unregistered at once (so the reach check fails immediately), a new record to
  the remaining contacts. Carol removes Bob: Bob keeps Alice's record until
  Alice's scheduled rotation, then his refreshed bundles stop carrying her.
  The schedule is 7 days, chosen and not measured. The time of the last
  rotation is kept per device and not shared between devices. A device's
  first registration pass (install, restore, pairing) only starts the
  schedule and rotates nothing, so a new device does not invalidate the
  records the person's contacts already hold. A reach state from another
  device with a higher `fof` version restarts the schedule instead of
  rotating again (2026-11-04).
- **Widening spreads as contacts' devices come online**, which is the UX
  section 8 "once your contacts next open Lakko" state; the client counts
  acknowledgements from its contacts to know when to clear it.

**Only the contacts I choose (2026-10-30, the maintainer's decision; UX spec
section 2.1.2).** Part 2.1 gets the same two forms as part 2.2: all contacts
(the default) or only the ones the person chose. Under the second form the
record goes only to the chosen contacts, so only their contacts can find the
person; everything after the offer works as above. The relay is the contact's
own device and is unchanged, and the contacts are told nothing.

- **The set is the owner's, synced in the setting.** `VisibilitySetting` gains
  `contacts_form` (all / chosen) and `chosen_contacts`, a list of 16-byte account
  ids, at most `MAX_CHOSEN_CONTACTS` (4096, chosen, not measured). Account ids,
  because an account key changes on a rotation and a reset and the choice must
  not. A contact with no known account id cannot be chosen.
- **Narrowing** is the narrowing above: switching a contact off, or moving from
  all to chosen, makes a new `fof` version, unregisters the old key at once and
  sends a new record to the contacts still chosen. A contact switched off keeps
  an old record until it ages out, which opens to a key the reach check refuses,
  so a searcher's row is dropped; a modified client can still read the name and
  username in a record it already held, the residual a removed contact leaves
  today.
- **Widening** sends the record to the newly chosen contact at its next poll.
- **A device that asks** (below) is answered with an offer only by a person who
  has chosen it, so the ask is answered with the profile alone otherwise.
- Wire: a flag day on the visibility envelope; there is nothing to migrate.

**A device that holds nothing asks (2026-10-30).** An offer and a bundle go out
once per conversation, so a device that missed them (a new device, a sign-in on
a device that lost its state, a dropped row) would never get them. Measured on a
live account: a person with one contact held no record at all and found nobody
through that contact. No new envelope carries the ask: a device that holds
neither an offer nor a bundle from the other person of a one-to-one sends the
existing field-less profile request (ADR-0048), at most once a day per
conversation, or at most once a week when it already holds their `contact`
grant (a person who does not allow contacts of contacts offers nothing, and a
daily ask would never end). The other person's device answers with its profile
as it always did, and also forgets that conversation in its sent records, so its
offer and bundle go out again, at most once an hour per conversation. All three
numbers are chosen, not measured.

### 5. Everyone, username verification, and names on the server

- **Everyone** sets `open` on the mailbox and publishes in discovery: the
  username binding (unchanged table, gains `public BOOLEAN`), and
  `display_names(oprf_hash BINARY(64), account_id BINARY(16), PRIMARY KEY
  (oprf_hash, account_id))`, one row per name search term. `POST
  /discovery/name/lookup` returns up to 20 account ids for one term's hash (a
  cap chosen, not measured); the client fetches their profiles and filters by
  the whole query. The name is on the server only as lookup keys, never
  plaintext and never as OPRF outputs (invariant #1; see the lookup-key item
  below). The server learns how many terms each public
  person has (roughly their name's length) and which accounts share a term;
  anyone with tokens can list the people in Everyone by walking prefixes,
  which is what Everyone means.
- **A person in Everyone is found with a name** (decided 2026-09-29 after
  S4/S5; the first draft said "the client fetches their profiles" with no
  store behind it). Entering Everyone also publishes one **public record**:
  the section 4 introduction record, keyed per term over the same name terms
  plus the username, with no `fof` grant in its payload (the mailbox is open,
  so none is needed, and a grant given to the world would outlive leaving
  Everyone). Discovery keeps it opaque in `public_records(account_id,
  record)`; `/discovery/name/lookup` and `/discovery/lookup` return it beside
  each account id, and the searcher opens it with the OPRF output it already
  holds for its query term.
- **Discovery stores lookup keys, never OPRF outputs** (decided 2026-09-29,
  found after the core blocker sprint). The public record is sealed under keys
  derived from the OPRF output of each term, so if discovery held the outputs
  themselves, anyone reading its tables could open every public record and
  read every name in it. Every `oprf_hash` a client sends - username
  register and lookup, name terms, name lookup, channel handles - is instead
  `lookup_key = BLAKE3-keyed-XOF(BLAKE3-derive-key("lakko/discovery/lookup/v1"), output)`,
  64 bytes, so the server columns and the certificate keep their width. It is
  one-way, so a copy of the database opens no record. `finalize` on both FFI
  surfaces returns the pair (`output`, `lookupKey`) so every call site names
  which one it uses; the output never leaves the device. This does not help
  against the operator, who holds the OPRF key (Residuals).
- **A username found by `/discovery/lookup` counts as verified without a
  certificate**: discovery is the authority that signs certificates and is
  the one that answered. Every other username a client shows needs its
  certificate.
- **Leaving Everyone** clears `public`, deletes the name rows and the `open`
  flag. `lookup` of a non-public username answers exactly as for an
  unregistered one.
- **Username certificate.** At registration, discovery signs
  `(lookup key of OPRF(username), account_id)` with a dedicated Ed25519 key (published at a
  well-known path and pinned in the clients, the ADR-0015 pattern). A viewer
  verifies a claimed username by one OPRF evaluation plus the certificate,
  with no lookup - so ADR-0048 verification keeps working for people who do
  not answer lookups, and no endpoint confirms a guessed username for a known
  account id.
- **Name search** (UX section 6.1, decided 2026-09-28: word prefixes, not the
  whole name). One implementation in `lakko-discovery-client` (not
  `lakko-contacts`: that crate is linked by the iOS notification extension
  and must not carry Unicode tables), on both FFI surfaces: normalisation
  (NFKD, lowercase, drop marks, collapse whitespace, trim; at least 2
  characters), `name_search_terms` (what a person publishes or seals under),
  `name_query_term` (the one term a query looks up: its longest word, cut to
  16 letters), and `name_matches_query` (the post-filter: every query word is
  a prefix of some name word). Name terms are OPRF inputs in their own domain,
  apart from usernames and channel handles.
- **Publishing evaluates its terms in one request (amended 2026-11-04).** Entering
  Everyone, or changing the name while in it, evaluates the whole term set (up to 64
  name terms plus the username) in one `POST /discovery/oprf/batch` for one
  `discovery-register` anonymous token, not one `POST /discovery/oprf` and one
  `discovery-lookup` token per term: a device's pool holds at most 32 tokens and the
  per-IP bucket bursts at 10, so a name of two or three long words could not be
  published (found live on `test.lakko.me`). The route takes 1 to 66 blinded elements
  and answers their evaluations in order; a malformed batch is refused before the token
  is spent and before any evaluation; it carries no session and sits under the per-IP
  bucket, charged once. Lookups (searching for others) are unchanged, one
  `discovery-lookup` token per evaluation. ADR-0087 section 4 carries the same
  amendment for group and channel records. **Residual:** a register token buys up to 66
  evaluations, so with the default 20 register tokens a day an account can evaluate up
  to 1320 terms a day through this route against 300 lookups; the records reached are
  public by choice.

### 6. Every write path carries its key

Amended 2026-09-28 before the client sprints (the maintainer's call, "S3b"):
the first draft named delegations here that no one could build, because the
issuer of a delegation must know the delegate's key in advance. Each path
now carries a grant, or a delegation to a key known in advance.

- **Contact request (reply).** The sealed request carries the requester's
  `reply` grant. The recipient writes the accept (the 1:1 Welcome) and the
  ack with it, the Welcome FIRST: the requester closes the key when the ack
  arrives, so an ack written first (or in parallel, as the natives did) let an
  online requester close it before the Welcome landed, and the Welcome was
  refused (seen live 2026-09-30). Registered with a 7-day expiry, removed once
  answered.
- **One-to-one conversation (contact).** A 1:1 conversation's profile carries
  the `contact` grant (and `member`); the receiver keeps it only from the
  other participant of a 1:1 conversation.
- **Contact and call links (link).** An `/i/` and a `/c/` capability carry a
  `link` grant inside their signed, sealed part, scoped to that link, so
  revoking the link removes exactly its key. A call joiner's cold-start
  message to the owner carries the joiner's `reply` grant, for the Welcome.
- **Group link (reply).** A `/g/` join request's self-signed attribution
  carries the joiner's `reply` grant; the committer delivers the sweep's
  Welcome with it (C-69).
- **An add a contact proposed (delegation).** Under ADR-0045 the member who
  proposes an Add may not be the one who commits it. The proposer posts a
  committer-delegation envelope in the group: one delegation per committer
  candidate (the owner and each admin), each to that candidate's account
  key. A delegation lives at most 24 hours, so the proposer re-sends a fresh
  one while its proposal is pending. Built 2026-10-30 on all three clients
  (until then no client re-sent, in spite of this sentence): the proposing
  device checks at the tail of each foreground poll of the group, and one
  rule, `lakko_ds_proof::committer_envelope_due`, says when an envelope is
  due (12 hours after a signed one, chosen; at once for a candidate it did
  not cover). An envelope without delegations (the next bullet) names a
  binding, which does not expire, so only a new candidate makes it due.
- **An add of a person found by search (amended 2026-10-29, the
  maintainer's call).** Group info's Add members screen
  ([`docs/ux/conversation-info.md`](../ux/conversation-info.md) section 2.4)
  lets the owner or an admin add a person the search found who is not a
  contact. Two cases:
  - found through a grant the person handed out (contacts of contacts, a
    group or a channel): the proposer keeps that grant as one it holds for
    the person, and the delegations above are signed with it;
  - found in **Everyone**: the person publishes no reach key, only the
    mailbox binding in the public record, and their mailbox is open. The
    proposer posts the committer-delegation envelope with **no delegation**
    and the binding from the search result (verified under the person's
    account key, as everywhere). Whichever candidate commits writes the
    Welcome to that mailbox without a proof, spending one `mailbox-write`
    token ([ADR-0078](0078-servers-cannot-link-accounts.md) section 1), as a
    contact request to that person does. A committer writes unsigned only
    for a person named by such an envelope, and only when it holds no grant
    or delegation for them.

  **Residual, accepted.** If the person leaves Everyone between the search
  and the sweep, the mailbox is closed when the Welcome is written; the
  Delivery Service drops the proofless write and answers as for a stored
  one (it never tells open from closed), so the committer counts the
  Welcome delivered and the person is not in the group. This is the case
  ADR-0050 section 2.5a item 7 keeps sweepers from (writing unsigned with
  no binding at all); here the binding is a fresh, verified statement that
  the mailbox was open. Any member could post a no-delegation envelope; its
  binding must verify under the added person's account key, so it can only
  name that person's own mailbox, and the cost to a committer is one token.

**One inbound mailbox per person** (decided 2026-09-29 after S3b, the
maintainer's call). Reach keys are registered on the **account** mailbox
only, and every write a reach key or a delegation authorises goes there,
Welcomes included: ADR-0050 section 2.7 is amended so that a Welcome is
addressed to the added leaf's account mailbox rather than its device
mailbox. A device mailbox stays claimed and closed with no reach keys, so
nobody can write to it. The alternative, the account's reach keys mirrored on
every device mailbox, would have made every delegation per device and added a
key set that can drift between mailboxes.

### 7. The setting itself

A bounded `lakko-contacts` envelope carrying the position, the two parts, the
all/chosen form, the chosen group and channel ids, the last-parts memory (UX
section 3) and every reach key version counter, sent over the DeviceGroup so
all of the person's devices agree. **The carrier** (decided 2026-09-29 after
S4/S5): a DeviceGroup body `ServiceEnvelope { bytes envelope }` at field 20 of
`DeviceGroupPayload` (14-17 stay ADR-0050's, 18-19 are the chat-folders
branch's), carrying a `lakko-contacts` envelope verbatim; the receiver
dispatches it and keeps only Visibility and ReachState from it, ordered by
their own stamps (since 2026-09-30 also the own profile envelope, see
ADR-0048 section 4a). A device that joins the DeviceGroup is sent the current
Visibility and ReachState by the device that admits it, through that device's
DeviceGroup sync runtime after it reloads at the post-add epoch, never through
the pairing flow's own handle of the same DeviceGroup: two handles sealing at
one sender generation reuse a key and nonce (seen live 2026-09-30 after the
merge with the chat-folders branch, where the sync runtime began reloading
from the pairing's snapshot; the new device refused the second message as a
secret reuse). Visibility is ordered
by a strictly increasing stamp (the ADR-0044 lesson: a re-send carries its
original stamp). ReachState is **merged, not replaced** (decided 2026-09-29,
after the three clients had each written a different rule): a key version
never goes back, so `lakko_contacts::visibility::merge_reach_state` keeps the
maximum per `(class, scope)` and tells the receiver whether to store and
whether to send the result on - adopt a newer state verbatim when it already
knows every version, otherwise re-stamp strictly after both and send.
Replacing the whole state would drop a rotation two devices make at once.
Every client calls the core rule (`mergeReachState` on both FFI surfaces);
none re-implements it.
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

- **#1** held: names only as one-way lookup keys, only for people who chose
  Everyone; the operator can still evaluate guessed names (Residuals).
- **#2** held: reach keys are public keys; seeds never leave E2EE carriers.
- **#3** touched once: the DeviceGroup `ServiceEnvelope` body (section 7) is a
  `lakko-mls` payload change, with property tests and the extended
  `devicegroup_payload` fuzz target; no state-machine change.
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
- **An introduction record shows its issue time and its term count in the
  clear**, so a bundle holder sees roughly how long each name is and how the
  records were issued over time. Padding dummies copy both from real records.
- **A bundle holding exactly one real record has no dummies**, because the
  smallest size bucket is 1; its holder learns the relaying contact has one
  contact with 2.1 on.
- **Prefix search is enumerable.** With enough anonymous tokens, a searcher can
  walk every 3-letter prefix and list the people in Everyone, and the contacts
  of their own contacts who turned 2.1 on. The token quota is the only bound.
- **The operator reads the names of the people in Everyone.** It holds the
  OPRF key, so it can evaluate every 3-letter prefix itself at no token cost,
  derive each lookup key, fetch the public records and open them. Storing
  lookup keys instead of outputs protects the names against a copy of the
  database, not against the operator. Inherent to server-side prefix search
  with an operator-held OPRF key; a person who does not accept this does not
  choose Everyone.
- **A contact** can pass their key to anyone. As today with a username; fixed
  by the rotation on removal.
- **`member`**: a co-member the owner hides from holds a key that the DS
  accepts. The owner's client drops anything under a `member` key that is not
  a Welcome for that group, so it never shows - but it is stored, which is
  weaker than the requirement's "before it reaches them" for that one case.
- **A `/g/` joiner's reply key is readable by every group member**, who can
  write to the joiner's mailbox until the key expires (7 days) or the joiner
  removes it on joining.
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

## Client contract (S4 / S5)

What every client (web, iOS, Android) builds on top of S1-S3b, written once so
the three do not diverge. UX is `docs/ux/who-can-find-me.md`; FFI names are the
UniFFI spelling, wasm uses the same names.

**Mailboxes and transport**
- One inbound mailbox per person: the account mailbox. Reach keys are
  registered there; Welcomes are addressed to the added leaf's **account**
  mailbox (resolve the leaf through its device binding). A device claims its
  device mailbox and stops polling it.
- Every `GET /ds/mailbox/{hex}/messages` carries the identity proof
  (`dsMailboxSigner`); an unclaimed or unproven read is `401`. A device claims
  the account mailbox before it reads it.
- Pairing, recovery, the welcome courier and the state bundle use
  `/ds/rendezvous/{hex}/messages`, with no proof, delegation or wake header
  (any of those is `400`).
- A write to someone else's mailbox is signed `DsProofSigner.forReach(seed)`
  with the reach grant that authorises it (the sixth proof-header field is
  added by the signer). A request that needs no grant (the person is in
  Everyone and their mailbox is open) is unsigned.

**Reach keys the owner holds** (all derived with `reachSeed` /
`deriveReachGrant` from the account seed; register with
`PUT /ds/mailbox/{hex}/reach {add:[{reach_pk_b64, expires_at_ms?}], remove:[...], open?}`
signed by the identity proof; keep the version counters in the ReachState
envelope on the DeviceGroup)
- `contact` (no scope, no expiry, rotated by version when a contact is
  removed): sent in the 1:1 profile beside `member`. Since 2026-11-06 the
  owner keeps `contactKeyVersions(current)` registered (the current version
  and up to eight retired ones) and removes every other `contact` version;
  Remove contact bumps the version, registers the new key and republishes the
  profile in every remaining contact's one-to-one; every mailbox row is judged
  by `mailboxRowContactVerdict(accountSeed, currentContactVersion,
  row.reach_pk_b64, senderIsContact)` (section 1).
- `member` per group, `group` per group, `channel` per channel (versioned by
  epoch): sent in the profile in that scope when the setting allows it; the
  channel grant goes in the member card.
- `link` per `/i/` or `/c/` link: `linkId = generateLinkId()`, register before
  sharing, remove on revoke or expiry, persist `linkId` + version with the
  link list.
- `reply` per outgoing request (contact request, `/c/` cold start, `/g/` join
  attribution): random 16-byte scope, registered with a 7-day expiry before
  sending, removed once answered, joined or expired; persisted so every device
  can remove it.
- `fof`: in the introduction record (section 4), registered while 2.1 is on.
- `open = true` only in Everyone.

**Write paths**
- Contact request, sender: `composeContactRequest(..., profile, replyGrant, intro)`.
  Recipient: keep `OpenedContactRequest.replyGrant`; write the accept (1:1
  Welcome) and the ContactAck with `forReach(replyGrant.seed)`.
- `/i/` and `/c/` mint: `composeInviteCapability` /
  `composeCallInviteCapability` with `linkId` + `linkGrant`. Joiner: write the
  request (or the `/c/` cold-start intro) with `forReach(cap.linkGrant.seed)`,
  never keep that grant, and include its own `reply` grant.
- `/g/` joiner: `composeGroupJoinAttribution(seed, cap, replyGrant)`.
  Committer: the sweep's Welcome to the joiner is written with
  `forReach(attribution.replyGrant.seed)`.
- An Add proposed by a contact of the added person: the proposer signs one
  `signDelegationBlob(contactGrant.seed, addedAccountMailbox, candidateAccountKey, now+<=24h, random16)`
  per committer candidate (owner, then admins by rank, up to 16), wraps them in
  `composeCommitterDelegationEnvelope` and sends it in the group; re-sends while
  the proposal is pending and when the candidate set changes. A committer that
  sweeps that Add sends `x-lakko-ds-delegation: committerDelegationFor(env, ownAccountKey)`
  with the Welcome write, signed `forReach(ownAccountSeed)`. Keep the newest
  envelope per (sender signature key resolved at receipt, added account key).
- A received `contact` grant is kept only from the other participant of a 1:1
  conversation; `group` / `member` only from members of that group; `channel`
  only from that channel's member cards; always keyed on the signature key
  resolved at receipt (C-68).

**The setting, search and discovery**
- The Settings screen drives `chooseVisibilityMode` / `setContactsOfContacts` /
  `setGroupsVisible` / `setGroupsForm` / `excludeScopeUnderAll` /
  `setScopeChosen`; the result is stamped, sent as the Visibility envelope on
  the DeviceGroup, and applied: grants added or withheld in profiles and member
  cards, keys registered or removed, `open` set, introduction offers sent or
  withdrawn, Everyone published or unpublished. Default for a new account:
  `defaultVisibilitySetting()`.
- Every discovery request carries `OprfResult.lookupKey`, never
  `OprfResult.output`; records (public and introduction) are composed and
  opened with the `output`s. A client that kept outputs derives keys with
  `discoveryLookupKey`.
- Everyone: `PUT /discovery/visibility {public, name_terms_b64}` with the
  lookup key of every `nameSearchTerms(displayName)` term (`blindNameTerm`),
  and the public record composed over the outputs, on entering Everyone and
  on every name change; `public: false` on leaving.
- Username certificate: kept from `POST /discovery/register` or fetched with
  `GET /discovery/username-certificate`, carried in profile v3; a viewer shows
  a username only after `verifyUsernameBinding` (expected hash = the lookup
  key of the claimed username) against the key from
  `GET /.well-known/lakko-username-cert-key` (pinned at build time where the
  client pins keys). This replaces the ADR-0048 lookup verification.
- Search: local first (contacts, profiles holding a grant, any part of any
  word, `nameMatchesQuery`); the network search takes the username
  (`/discovery/lookup`, public accounts only) and one name term
  (`nameQueryTerm` -> `/discovery/name/lookup`), then the introduction bundles
  (`introductionTags` / `openIntroduction` against the OPRF output of the
  username and of the query term), filters by the whole query, and runs
  `POST /ds/mailbox/{hex}/reach/check` with the grant before showing a result.
- Contacts of contacts: when 2.1 is on, compose one introduction record per
  version over `nameSearchTerms` + the username and send it to each contact as
  an IntroductionOffer; each client relays its contacts' offers as a padded
  Introductions bundle to its own contacts, and keeps the newest bundle per
  contact (`bundleIsNewer`).
- Channels: a subscriber key is Ed25519 (`ChannelSubscriberKeypair`, regenerate
  any stored X25519 key); upload the member card (`sealMemberCard`) signed with
  `memberCardRequestSignature` in `x-lakko-member-card-sig`, re-upload after
  each epoch rotation, delete on unsubscribe; members list via
  `GET /v1/channels/{id}/members`.
- Every new envelope kind (IntroductionOffer, Introductions, Visibility,
  ReachState, CommitterDelegation) is dispatched, never rendered, on the
  foreground path and in the background filters (web service worker, Android
  background decrypt; the iOS NSE uses `decodeScopedDiscoveryEnvelope`).

## References

- UX: [`docs/ux/who-can-find-me.md`](../ux/who-can-find-me.md)
- ADR-0005, ADR-0010, ADR-0028, ADR-0043, ADR-0044, ADR-0045, ADR-0048,
  ADR-0050, ADR-0053, ADR-0054
- Privacy Pass / OPRF: RFC 9497
