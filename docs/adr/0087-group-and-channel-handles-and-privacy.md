<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0087: Every group and channel has an @name, and is public or private

- **Status:** Proposed (2026-10-31, amended 2026-11-01; section 18, the photo, added
  2026-11-02; a new publisher's earlier seeds and its waiting request, 2026-11-08). The
  product decisions in Context are the maintainer's and are final; the mechanism below is proposed. Nothing is built, apart from the group of one and the
  end-of-membership groundwork.
- **Date:** 2026-10-31
- **Context:** product, the scale requirement in `CLAUDE.md`, invariants #1, #2,
  #3, #4, #5. UX spec:
  [`docs/ux/group-and-channel-privacy.md`](../ux/group-and-channel-privacy.md)
- **Amends:** [ADR-0010](0010-channels-two-tier-broadcast.md) (seed delivery for
  public channels), [ADR-0027](0027-external-join-approval.md) and
  [ADR-0028](0028-group-join-ticket.md) (what a knock row is, the capability
  gains a knock key), [ADR-0045](0045-remote-commit-epoch-fork.md) UX section 6.2
  (what happens when membership ends), [ADR-0051](0051-group-and-channel-names.md)
  (a public name is published), [ADR-0059](0059-channel-discover.md) (`listed`
  is folded into public, the handle route, channel post content),
  [ADR-0060](0060-scoped-discovery.md) (the public record covers groups and
  channels), [ADR-0065](0065-attachment-lifetime-and-access.md) (a shorter idle
  life for a channel's files; a keep source for conversation photos),
  [ADR-0084](0084-profile-photo.md) (its avatar codec is shared with groups and
  channels, section 18),
  [ADR-0064](0064-conversation-backup-and-device-re-entry.md) (a vault ends with
  the membership), [ADR-0078](0078-servers-cannot-link-accounts.md) section 4
  (one knock key per group, not per link; this ADR makes it a prerequisite)
- **External review:** step 3 of the plan changes `lakko-mls` (invariant #3). The
  per-change half is an ADR plus property tests; the external review is tracked
  once, in G-39 ([ADR-0046](0046-external-crypto-review.md)).

## Context

The maintainer decided on 2026-10-31:

1. Every group and every channel has an @username. A group or channel without
   one does not exist. One-to-one chats are unchanged.
2. Each group and channel has a privacy flag. **Public:** it can be found in
   search and a person can join by sending a request to join. **Private:** it
   never appears in search, and a person joins only by an invitation from the
   owner or an admin.
3. A person removed from a group loses it: the row leaves their list and the
   history is wiped from their devices. If added again they see only new
   messages. A private group must not be findable by them afterwards.
4. A group may be created with nobody but its creator.

And on channels, the same day:

5. A channel uses the same screen shapes as a group. It is not a group (corrected by
   the maintainer 2026-11-01): the owner and co-publishers post, everyone else comments
   and reacts, and a channel has no calls. Broadcasts, a kind of call not yet designed,
   may come later.
6. A channel post carries what a chat message can.
7. Owner and admins invite people to a channel, public or private, from a contact
   picker and by link, the way people are added to a group.
8. Public channels and groups are found by @name and by name.
9. The subscriber list says only what a person can act on. Machinery goes: keep
   what a person acts on, and fold `listed` into the flag if it fits ADR-0059.

Scale: a person can have 10,000 groups and channels, a channel can have millions
of subscribers, and accounts are on the order of a billion. Phones are offline
and apps are closed.

What exists, read in the tree on 2026-10-31:

- **Names.** `usernames` maps a lookup key to an account, and answers a lookup
  only when `is_public` is set; a registration of a hash that is present is `409
  username-taken`, whoever holds it (`services/discovery/src/registry.rs`). A
  retired name is held 30 days. `channel_names` maps a lookup key to a clear
  `channel_id`, first come first served, with one `discovery-register` token and
  no renewal (`channel_names.rs`, migration `0009`). The two use different OPRF
  inputs (`CHANNEL_OPRF_DOMAIN`, `crates/lakko-discovery-client/src/blinder.rs`),
  so the server cannot compare them, and `blind_channel` does not apply the
  username character rule. A channel handle is optional
  (`apps/web/src/routes/Channels.tsx`, "empty handle skips discovery"). A group
  has no handle.
- **A knock is bound to an epoch and is lost when read late.** The joiner posts
  an MLS external proposal framed at the epoch it read
  (`Identity::create_external_join_proposal`). OpenMLS accepts a proposal only at
  the current epoch (`public_group/validation.rs`, ValSem003), and the client
  drops the row and moves its cursor past it (`apps/web/src/chat/client.ts`, "a
  stale-epoch proposal ... Dropped"). Nothing tells the joiner. In a public group
  every admission advances the epoch, so a queue of requests would be thrown away
  one admission at a time. The retained KeyPackage does survive: approving
  publishes an `Add` the admin authors from it (ADR-0045 S7).
- **Knock rows are readable by the DS** (ADR-0078 finding 7): the leaf carries
  the joiner's account key. ADR-0078 section 4 is not built. A knock spends one
  `ds-knock` token (quota 200 a day), a row is at most 16 KiB and lives 7 days,
  and no cap per journal exists (`services/ds/src/access.rs`).
- **`MAX_PENDING_EXTERNAL_JOINS` is 64** (`crates/lakko-mls/src/group.rs`); past it
  a request is surfaced with `retained: false` and lost.
- **Every subscriber is admitted by the publisher's device.**
  `apps/web/src/channels/subscriber-admission.ts` reads the whole subscriber list
  every 15 s while the app is open and seals the seed to every key. Whoever holds
  a channel id can register a key with one `channel-subscribe` token
  (`services/channels/src/api.rs`, `subscribe`), and the publisher's device seals
  to it. `list_subscribers` has no paging. A seed envelope is 80 bytes
  (`SEED_ENVELOPE_LEN`), kept for 4 epochs, with one rotation a day by default
  (ADR-0031, `rotation_policy.rs`).
- **Chats search reaches a channel by exact @handle only, and misses some.** One
  search asks the channel namespace in parallel with the username and the name term
  (`apps/web/src/search/people-search.ts`, `channelLeg`; the same on Android,
  `search/PeopleSearch.kt`, and iOS, `LakkoApp.swift`). The leg runs only when the
  text passes the username rule (`canonicalUsernameOf`,
  `apps/web/src/search/wiring.ts`), it spends an OPRF token, and it calls
  `POST /discovery/channel/lookup`, an exact match. Three things follow. A channel
  with no handle cannot be found. A channel is never found by its name: no term is
  published. And **a handle outside the username rule is registered and then
  unfindable**: the new-channel field has no validation (`Channels.tsx`) and
  `blind_channel` only normalises, so `@ab` or `@my-news` is claimed and the search
  leg never asks for it. A hit is one line, "Channel @x" (`search.people.channel_result`),
  with no name, and it opens the subscribe page directly.
- **A channel is a separate product today.** `apps/web/src/routes/Channels.tsx` has its
  own list pane (`ChannelsLayout.tsx`), header, `PublishComposer` (text only), `PostCard`
  and a Manage screen of five cards (`ManagePanel.tsx`: the channel's details, List in
  Discover, Subscribers, Co-publishers, Protection). The conversation-info pattern
  reaches only as far as a read-only block with a `Manage channel` row.
- **Posts are text.** The plaintext is classified as a metadata record or raw `Text`
  (`meta.rs`, `classify_post_plaintext`), and `PostCard` shows "the text" and no author.
  `post.rs` says attachments "belong in object storage with content-addressed
  pointers"; none was built. Posts are kept 30 days (`DEFAULT_RETENTION_SECS`), seeds 4
  epochs, so with a rotation a day a late subscriber reads about 4 days of a 30-day
  store.
- **The rotation runs only while the panel is mounted.** `RotationPanel` starts
  `startRotationCron` itself, and `CoPublishersPanel` runs its journal poll; both live
  inside `ManagePanel`, mounted while the owner has that channel open. `SubscriberAdmission`
  alone is app-wide (`App.tsx`). A channel its owner does not open does not rotate.
- **The subscriber rows mean nothing to a person.** "Subscriber {number}" with "Ready"
  or "Setting up..." (`channels.subscribers.*`): a row number and whether this device
  sealed the seed at the current epoch. The service holds per-channel random keys
  (`freshSubscriberKeys`), so "anonymous to the service" holds; member cards
  (ADR-0060) are the only names, and `MembersPanel` already shows them, on a separate
  screen.
- **A co-publisher is what a group calls an admin, nearly.** It can post, let
  subscribers in and rename. (This bullet also said "rotate"; corrected 2026-11-08 by
  the maintainer: only the owner's device rotates the channel key, section 15.) A PublisherGroup has no role table, so its client
  can approve another co-publisher; what holds a stranger back is the owner-signed
  appointment at the channels service, without which `require_publisher` refuses it.
- **The chat survives the end of a membership** (ADR-0045 UX section 6.2):
  built around keeping the transcript. Decision 3 reverses that.
- `Group::create` does not need a second member; the founding path in the clients
  does (`NewGroup.tsx` requires a pick, `new-group.md`).
- OpenMLS has `MlsGroup::delete`; `lakko-mls` does not expose it, so a deleted
  chat leaves its group state in the provider.

## Decision

**One namespace holds people, groups and channels. A group or channel holds its
name with a key of its own that names no account. A public one is published the
way a person in Everyone is; a private one reserves a name and answers nothing. A
request to join is a sealed KeyPackage with no epoch. A public channel has one
seed envelope for everyone and no per-subscriber work; a private channel admits
only keys carrying an invitation its publisher checks. A channel is a conversation
that only its owner and admins write in, with the same screens and the same message
body as a group. The end of a membership wipes the conversation on every device of
the account.**

### 1. One namespace

- People, groups and channels share one set of names: `[a-z0-9_]`, 3 to 32
  characters, the rule `lakko_contacts::username::check_canonical_username`
  already defines. `@name` resolves to at most one thing.
- `CHANNEL_OPRF_DOMAIN` and `blind_channel` go. One OPRF input, so one lookup key
  per name, and one evaluation per typed name (a search spends two tokens, not
  three).
- `name_claims(lookup_key BINARY(64) PRIMARY KEY, kind TINYINT)` is the uniqueness
  table. `usernames` and `handles` (section 2) each insert into it in the
  transaction that takes a name, so a person and a group can never hold one name.
  A refused claim is `409 name-taken` on every path.
- `POST /discovery/lookup` answers for either kind. `POST /discovery/channel/*`
  goes.
- A retired name, of any kind, is held 30 days as ADR-0076 holds a username, and
  its last holder may take it back.
- The name of a private group is a name in this namespace. Decision 1 means about
  as many names as groups, so the client proposes a name a person does not have to
  invent (UX section 2.1).

One rule on every name also ends the defect in Context: a name that can be claimed
is a name search will ask for.

Why not separate namespaces (today's shape): the person typing does not know the
kind (`navigation.md` section 4.3), and a group `@anna_weber` beside a person
`@anna_weber` is impersonation the product would be manufacturing.

### 2. What discovery stores

| | Public group or channel | Private group or channel |
|---|---|---|
| `name_claims` | one row | one row |
| `handles` | `handle_id(16)`, `lookup_key`, `write_pk(32)`, state, `lease_until`, `retired_at` | the same row |
| `public_records` | one sealed record | **nothing** |
| `search_terms` | one row per name term | **nothing** |
| group or channel id | inside the sealed record only | **nowhere** |
| account | **nowhere** | **nowhere** |

`public_records` and `display_names` lose their account key: both are keyed on a
16-byte target (an account id or a `handle_id`) with a kind byte, so a person in
Everyone and a public group are served by one query.

**A private name reserves and answers nothing.** `lookup` joins `public_records`,
as `registry.rs` does today, so a private row and an absent row take the same
query and return the same `404`. **Claiming** a name is the only thing that
touches the row, and it is refused with the same `409` a retired name, a person's
private username and a public name get.

**What a refused claim reveals, stated.** Uniqueness is itself the disclosure: if a
second claim fails, someone learns the name is held. Global uniqueness and a
private name cannot both be perfectly silent, and no design here removes that one
bit. It says: someone holds or recently held this name. It does not say what kind
of thing holds it, what it is called, who is in it or how to reach it, and it
yields no id. A test costs one `discovery-lookup` and one `discovery-register`
token, and the second has a quota (section 3). A lookup that answered followed by
a refused claim separates public from the rest, nothing further. Live "is it
free" checks are not built for that reason: while typing, the client runs an
ordinary lookup, which sees public names only, and the only answer about a
private name comes from pressing Create.

### 3. The handle key

Write authority is a key that names no account:

```text
handle_seed = BLAKE3-derive-key("lakko/handle/write/v1", journal_root_seed)
```

`journal_root_seed` is ADR-0057's, derived from the owner's account seed and the
journal id, and per-group, so two handles of one owner are not linkable, every
device of the owner derives the key with nothing to transfer, and a restore from
the phrase gets it back. A PublisherGroup has a root too, so groups and channels
are one case. Only the owner holds it; admins and co-publishers never do. Renaming
the group's display name stays an owner or admin action (ADR-0051); the published
copy updates when an owner device next runs.

Every write is a signed statement in the ADR-0057 shape
(`lakko/handle/statement/v1\0 ‖ kind ‖ lookup_key ‖ write_pk ‖ unix_ms ‖ nonce ‖
fields ‖ sig`, 120 s window, nonce consumed), with no session:

| Route | What it does | Tokens |
|---|---|---|
| `POST /discovery/handle/claim` | take a name, with state and, if public, record and terms | one `discovery-register` |
| `PUT /discovery/handle` | replace the record and terms, flip the state, renew the lease | none, the key is the gate |
| `POST /discovery/handle/replace` | move to a new name; the old row is retired | one `discovery-register` |
| `POST /discovery/handle/rekey` | move `write_pk`, signed by the old key | none |
| `DELETE /discovery/handle` | retire the name | none |

An answer to a claim carries a **handle certificate**: `lakko_attest` signs
`(lookup_key, write_pk)` with the discovery key that signs username certificates,
domain `lakko/handle-cert/v1`. Without it a private group, which no lookup can
check, could show its members any name it liked. As built (2026-11-02) it is 169
bytes, `version ‖ lookup_key(64) ‖ write_pk(32) ‖ issued_at(8) ‖ sig(64)`; earlier
drafts of this document said 153.

**The lease.** A handle names no account, so `DELETE /discovery/account` cannot
find it and nothing else would ever free a dead one. `lease_until` is 180 days
from the last renewal (chosen, not measured); an owner device renews a handle with
fewer than 90 days left, at most 20 per foreground session (chosen). A lapsed row
is retired: it stops answering, is held 30 days, and its key can still renew it
during the hold. The group or channel keeps working; only the name is gone, and
the owner's device asks for a new one (UX section 4.4). An account being deleted
retires its handles first.

**Ownership transfer.** ADR-0057's root handover already carries the old root seed
to the new owner in an envelope, so the new owner derives the old handle key and
signs `rekey` to the key from its own root. No new carrier. If that fails the name
stays under the old key, which the new owner holds, and the step is retried. A
channel's owner move (`PUT /v1/channels/{id}/owner`) does the same.

**Rotation and reset.** An ADR-0074 rotation adds one owed `rekey` per owned handle,
paced like the renewals. An ADR-0073 key reset loses the old seed, so the handle
of an owned group cannot be moved: the recreated group takes a new name and the
old one lapses. Stated cost: up to 210 days (lease plus hold).

`discovery-register` goes from 10 to 20 a day (chosen, not measured): every group
and channel now spends one, and a person retrying a taken public name spends
another.

### 4. The public record, and search by name

A public group or channel is published the way a person in Everyone is
(ADR-0060 section 5): `lakko_contacts::introduction::compose_public_record`, sealed
under each search term, with a new payload kind:

```text
version ‖ kind(1 group, 2 channel) ‖ updated_at_ms(8) ‖ id_len ‖ id
        ‖ key(32) ‖ knock_version(2) ‖ name_len ‖ name ‖ description_len(2) ‖ description
        ‖ size_band(1)
```

- `id` is the `group_id` (32) or `channel_id` (16). `key` is the group's knock
  public key (section 5) or the channel's read key (section 6). `knock_version` is the
  version that knock key belongs to, so a person who found the group by search names it
  in the row they seal; removing an admin bumps it (section 5). A channel's is 0. Added
  2026-11-03 (the payload's first byte moves to `0x43`): without it a searcher could only
  guess version 0 after an admin was removed, and no admin could open the request.
- The search terms are the handle plus the ADR-0060 name terms, so a public group
  is found by `@name` exactly and by the start of a word from 3 letters. Each
  term costs the publisher one `discovery-lookup` token, once per name.
- A public group needs a name: a title built from member names must never be
  published.
- `size_band` is the ADR-0059 band, `<10`, `10+`, `100+`, `1k+`, `10k+`, `100k+`. For
  a group it is the owner's own statement as of the last publish. A channel's is read
  from the service that counts it, so the field in a channel's record is unused.
- A name lookup returns at most 20 records, oldest first (chosen: a later flood of
  one prefix cannot push out what is there). A person typing more letters narrows
  it, as with a person in Everyone. The client filters by the whole query.
- A name lookup gets people and groups and channels from one query, so a search
  costs two tokens, one for the `@name` and one for the longest word, against up to
  three today.
- **The cost of being found by name.** A three-word name under the ADR-0086 term rule
  (prefixes of 3 to 6 letters and the whole word) is about 15 terms, under today's
  rule (3 to 16 letters) up to 64: that many `discovery-lookup` tokens once per name,
  80 bytes of server row each, 92 bytes of record each. A rename pays again. The 300 a
  day quota covers it.
- ADR-0059's Discover directory stays, with its ranking; section 16 folds `listed` into
  the flag.
- **Amended 2026-11-04: publishing evaluates its terms in one request.** Found live on
  `test.lakko.me`: a publish ran one `POST /discovery/oprf` per term, each spending a
  `discovery-lookup` token under the per-IP bucket (burst 10), and a device holds at most
  32 tokens, so a name of about ten terms ("QA Second Channel") stopped at the tenth call
  with a 429. Publishing (the record of a public group or channel here, a person's public
  record in Everyone under ADR-0060) and a rename now evaluate every term in one
  `POST /discovery/oprf/batch` (`{"blinded": [...]}` -> `{"evaluated": [...]}` in request
  order, 1 to 66 elements: `MAX_HANDLE_TERMS` (65, the `@name` included) plus the old
  `@name` of a rename), for **one `discovery-register` token** in `x-lakko-anon-token`,
  with no session (a session would link the account to the publish by time), under the
  same per-IP bucket charged once. An empty list, more than 66 elements or one element
  that does not deserialise is a 400 before the token is spent and before anything is
  evaluated. Lookups are unchanged: one `discovery-lookup` token per evaluation on
  `/discovery/oprf`. The bullet above that prices a name in `discovery-lookup` tokens is
  history; a publish now costs one `discovery-register` token for the batch plus the one
  the claim or replace already spends. **Residual:** a register token buys up to 66
  evaluations, so with the default 20 register tokens a day an account can evaluate up to
  1320 terms a day through this route, against 300 through lookups. Every evaluation is a
  blind OPRF of a term the client chose; the records it reaches are public by choice.

Sealing protects against a copy of the tables, as it does for a person: the
operator, who holds the OPRF key, can open any record whose name it can guess, and
a public record is public. The residuals of prefix search (ADR-0060 section 4)
apply: anyone with tokens can walk prefixes and list public groups, which is what
public means.

### 5. A request to join a public group

**A knock row becomes a sealed KeyPackage.** Sealed to the group's knock key, it
carries no MLS message:

```text
knock_seed(v) = BLAKE3-derive-key("lakko/group/knock/v1", journal_root_seed ‖ v)
row  = 0x02 ‖ v(2) ‖ eph_x25519_pk(32) ‖ AEAD(plain), padded to 4 KiB or 16 KiB
plain = key_package ‖ reply_grant ‖ profile ‖ capability?
```

- The DS stores opaque bytes. This is ADR-0078 section 4, moved up from a later
  phase because a public group would otherwise publish its requesters' account
  keys. It differs in one way: **one knock key per group, versioned, not one per
  link.** A link's capability carries `(knock_pk, v)` in its sealed part (capability
  v3, flag day). The owner hands `knock_seed(v)` to admins in the ADR-0057
  registrar-seed envelope shape. Removing an admin bumps `v`; older versions stay
  derivable, so older links still open. An ex-admin cannot read rows anyway, since
  knocks need a write-tier proof (`get_knocks`).
- **A key rotation of the owner (ADR-0074 section 3.5) moves the journal root, and so
  every `knock_seed(v)`.** Added 2026-11-03. The rotating device therefore bumps `v`
  as one of its owed steps: versions up to the old `v` stay derived from the
  predecessor's root seed (kept with the own predecessors), the next version and
  every later one from the new root; the owner records the boundary per group, hands
  the new seed to the admins, and republishes the public record of a public group.
  Rows naming an older version still open, so links made before the rotation keep
  working.
- **No epoch.** A KeyPackage has none, so a request can wait in a queue for days
  and survive every commit. `Group::retain_join_request(identity, key_package)`
  replaces `process_knock` and `retain_external_join`: it validates the package and
  its ADR-0050 binding and applies the existing dedup and cap. Approving is
  unchanged: the admin authors an `Add` from the retained package, a committer
  sweeps it, the Welcome goes to the joiner's mailbox under the reply grant (C-69).
  The peers' checks do not move, since they judge the admin's proposal.
  A second request from a joiner already waiting (same leaf signature key) changes
  nothing: the package held first stays, and an approve acts on it (decided
  2026-11-02, `JoinRequestOutcome::AlreadyPending`). What happens to a held package
  that expires before anyone decides is not designed yet.
- It closes a class of defects too: a knock row can no longer be a commit, so
  `is_external_join_proposal_for` has nothing left to guard.
- The co-publisher join moves to the same row. The ADR-0081 rejoin knock keeps its
  own form and keeps the epoch in the knock response.

**In a private group** a row counts only with a capability of this group; a row
without one is dropped unread by any person. A public group takes a row with or
without.

**Abuse bounds.**

| Layer | Bound |
|---|---|
| Requester | one `ds-knock` token per knock (quota 200 a day); a device asks once per group per day (7 days until 2026-11-12) |
| DS, per journal | new cap: 4,096 rows (chosen, not measured), refused with `429 knock-queue-full` before a token is spent. The count and the insert are not one transaction, so knocks arriving on one journal at the same moment can end the queue at most their own payloads (4 per knock) past the cap; locking would give every unclaimed journal a lock row (decided 2026-11-02) |
| DS, row | 16 KiB, 7 days (existing) |
| Admin device | 64 retained (`MAX_PENDING_EXTERNAL_JOINS`); **the knock cursor stops while the queue is full**, so the backlog stays on the DS instead of being lost |
| Admin | new `DELETE /ds/group/{hex}/knocks?through=<id>` (write tier) behind `Reject all`: deletes the rows up to the last one the admin saw, so a request that arrived after it stays; refused on an unclaimed journal |

A device reads at most 64 rows at a time, so at most about 1 MB per drain (64 x
16 KiB, arithmetic), however large the backlog.

A flood is a denial of the queue and nothing else: 4,096 tokens is about 3 accounts
for a week at the quota (4,096 / 200 = 20.5 account-days, arithmetic). The DS
cannot tell a real knock from a false one because it cannot see who sent it. What
the group loses is new requests, until an admin presses `Reject all` or makes it
private; members, messages and invitations are untouched.

No push wakes an admin for a request: a wake must be signed by the group's wake key
(ADR-0078 section 3), which a stranger does not hold. A request waits until an admin
opens Lakko, and the requester is told that.

### 6. A public channel

**"Send a request to join" cannot mean an approval per subscriber for a channel.**
At one million subscribers: a person reviewing each at 10 seconds needs 116 days
without stopping (arithmetic, 10 s chosen); and if the publisher's device approves
automatically, which is what happens today, it reads 67 MB of list every 15 s,
4.5 MB/s, 386 GB a day (1,000,000 x 67 bytes, 5,760 reads, arithmetic on
`ADMISSION_INTERVAL_MS`), seals 1,000,000 envelopes a day, 80 MB of upload in 200
stage calls, and stores 320 MB of them. A request nobody can refuse is not a
request.

**So for a public channel the request is the subscribe, and it is answered at
once, with no one's device involved.**

- The public record carries a 32-byte **read key**.
- A publisher writes one envelope per epoch, not one per subscriber:
  `PUT /v1/channels/{id}/public-seed/{epoch}` (publisher proof, ADR-0059 section 9),
  `epoch(4) ‖ nonce(12) ‖ AEAD(read_key, seed)`, AAD `channel_id ‖ epoch`, 64 bytes,
  signed by the publisher so a subscriber cannot replace it. Kept as long as the posts
  it opens, not ADR-0031's 4 epochs, so a person who subscribes today reads the last 30
  days: about 2 KB a channel at a rotation a day (arithmetic).
- `GET .../public-seed/{epoch}` is open and immutable, so a cache in front serves
  it.
- A subscriber registers as today (one `channel-subscribe` token, so counts cost
  accounts, ADR-0059 section 4) and reads at once. No publisher needs to be online.
- Nobody reads the subscriber list in normal running.

**This does not weaken what exists.** Today any key registered gets the seed
automatically, so anyone, the operator included, can read a channel by subscribing.
A public channel says so plainly. There is no removing a subscriber from a public
channel: anyone can come back. Posts stay ciphertext to the server until it, too,
subscribes.

### 7. A private channel

The publisher's client is the boundary, and the channels service is a door in front
of it.

**The invitation.** A `/g/` link of a new preview kind (kind 3, a subscriber
invitation: `channel_id`, `invite_seed(32)`, channel name, description), held in
the discovery ledger like every link, sealed under its link secret. The link is
minted by the owner or a co-publisher. `Invite people` sends a one-use link to a
contact inside their one-to-one.

**At the channels service.** Each link has an Ed25519 **invite key** derived from
`invite_seed`. `PUT /v1/channels/{id}/invite-keys/{pk}` (publisher proof) registers
it with an expiry and a use count; `DELETE` revokes it. A channel row gains
`private BOOLEAN`. `POST .../subscribers` on a private channel needs
`x-lakko-channel-invite-proof`, signed by a registered key over `(channel_id,
subscriber_pk, time, nonce)`, naming the key so one signature is checked. Every
refusal is one `401 invite-proof-required`, before a token is spent.

**Who invited whom.** The service can refuse a subscriber without learning it. It
sees, at the moment of the request, one per-link key; the row it stores carries no
trace of which. An invite key names no account. It is registered under the owner
key or a co-publisher key, which name none either.

**At the publisher.** The door alone is not enough: the server is not trusted, and a
row is what the publisher seals to. A subscriber row carries a `sealed_ticket`: a
sealed box to the channel's admission key, holding the invite public key and its
signature over `(channel_id, subscriber_pk)`. Sealed, so a copy of the table does
not show which subscribers came through one link. `admitNewSubscribers` becomes:
read rows after a cursor (a paged list, new), open each ticket, check the invite key
is one of this channel's live keys, seal only then. A row with no valid ticket is
removed.

**The admission key (decided 2026-11-02, maintainer).** The key a ticket is sealed to
is a random seed with a version, minted by the owner, not derived from the owner's
account seed, so co-publishers can admit subscribers too. The owner hands the seed to
each co-publisher sealed to that person's account key. The link's preview carries the
public half and the version. When a co-publisher leaves or is removed, the owner
mints the next version and hands it to the ones who remain; new links carry it. Older
versions are kept so tickets from older links still open. A removed co-publisher can
open those too, which tells it which random subscriber key came by which link it
already knew: nothing about a person. The link records already sync across the owner's devices (ADR-0079);
co-publishers' keys are read from the service.

**A revoked invitation.** Revoke deletes the ledger row and the invite key. A new
subscribe is refused at the door; a row that got in and has not been admitted is
refused by the publisher; someone already admitted stays, as a revoked group link
removes nobody. A person who holds a revoked link sees the invalid-link state.

**Removing a subscriber from a private channel.** `DELETE .../subscribers/{pk}`
accepts a publisher proof, and a rotation follows at once. That is the only rotation a
person ever asks for, so no "Refresh now" remains (section 12). On the removed device
the channel ends as in section 9, when `GET .../envelope` answers that the row is gone.

**Delivering the seed.** Per-subscriber sealing stays for the first build, and it
does not meet the scale requirement: 80 MB a rotation on a phone at one million. A
private channel moves to the one-way key tree of ADR-0086
(`lakko-circle::tree`): an invited subscriber gets a leaf key once, the seed for an
epoch is one envelope under the tree's key, and a removal is about 1.4 KB of path
(extrapolated from 662 bytes at 8 levels and 1,046 at 14 to 20 levels, arithmetic).
The tree's edges are about 63 MB at 2^20 leaves (30 bytes an edge, from the crate's
measured sizes, arithmetic), stored once. Step 8 of the plan, before release.

**The read key to a co-publisher (phase B, 2026-11-06).** A co-publisher must hold
the channel's read key before the channel goes public, and the admission seed is sent
only for a private channel, so the read key travels on its own: the owner sends each
co-publisher `lakko-contacts::channel_read_key`, a fixed 155-byte envelope
`lakko/channel-read-key/v1\0 ‖ 0x01 ‖ channel_id(16) ‖ member_account_key(32) ‖
sealed(80)`, the 32-byte read key sealed to that person's account X25519 key with the
clear header as AAD, as a PublisherGroup application message. It carries no version
because the read key never changes. The receiver keeps it only when it is addressed to
its own account and the MLS sender resolves to the owner. Dispatched as
`IncomingEnvelope::ChannelReadKey`; fuzz `channel_read_key_decode`.

**Earlier channel seeds to a new publisher (amended 2026-11-08, maintainer decision).**
Before this, a person added as a publisher joined the PublisherGroup at a later epoch and
held no seed of an earlier channel epoch: the owner's epoch-0 channel record and every
earlier post were unreadable to them, and an empty new channel read "Earlier posts aren't
available." The rule "newcomers read nothing from before" (section 8) stays for
subscribers; **a publisher now reads what the owner's device still can.** After it
approves a publisher (and beside the read key), the owner's device sends that person
`lakko-contacts::channel_epoch_seeds`, as a PublisherGroup application message:

```text
"lakko/channel-epoch-seeds/v1\0" (29) ‖ version(1)=0x01 ‖ channel_id(16)
  ‖ member_account_key(32) ‖ count(1) ‖ sealed(48 + 36 × count)
sealed = eph_pub(32) ‖ ChaCha20-Poly1305(count × (epoch_id(4 BE) ‖ seed(32))) ‖ tag(16),
         AAD = every byte before it
```

One envelope carries 1 to 64 seeds (`CHANNEL_EPOCH_SEEDS_MAX`), strictly ascending by
epoch, sealed once to the member's account X25519 key with the `crate::envelope` box; the
count is in the clear so the length is checked exactly before anything is opened, and the
plaintext is checked again after (count, strictly ascending, no duplicate). 64 is chosen,
not measured: the channels service keeps a subscriber's envelopes for the newest 4 epochs
(`LAKKO_CHANNELS_ENVELOPE_RETENTION_EPOCHS`, ADR-0031), which bounds what a subscriber can
fetch, not what the owner holds; the owner's device keeps every channel seed it filed in its
local epoch map, and posts are kept 30 days (`LAKKO_CHANNELS_RETENTION_SECS`); at the
default rotation of one a day plus one epoch per publisher change, a channel's live posts
span about 31 epochs, so 64 covers them with room and keeps one envelope at most 2,431 bytes. A
channel rotating faster (the 1,000-post rule) can hold readable posts older than the 64th
epoch; the owner sends its newest 64 and older posts read "Earlier posts aren't available."
on the new publisher's device. One envelope per (channel, publisher, send); the owner may
send a second one later with seeds the first lacked.

*Sender rule (the owner's device).* After approving a publisher, beside the read key, send
the newest 64 seeds of its epoch map that it has not yet recorded as sent to that person,
and record per (channel, publisher account key, epoch) what it sent once the publish
succeeded; a failed send is retried on the next pass of the publisher loop. A publisher
removed and added again is a new hand-over. A co-publisher never sends one.

*Receiver rule.* A device keeps the seeds only when: it arrived on the PublisherGroup
journal of the channel `channel_id` names and this device publishes that channel;
`member_account_key` is this device's own account key; the MLS sender resolves at receipt
(C-68) to the channel's owner; and the box opens and the plaintext is well formed. It adds
a seed only for an epoch it holds none of, and never replaces a seed it holds. Anything
else is dropped whole. A wrong seed from the owner at worst fails to open a post, which
then shows as unreadable, never as another post.

Dispatched as `IncomingEnvelope::ChannelEpochSeeds`; fuzz `channel_epoch_seeds_decode`.
The iOS notification extension recognises it only, to suppress it. No server, no
`lakko-mls` and no wire change elsewhere; invariant #2 holds (the server relays ciphertext
it cannot open) and #5 (no new server state). What it gives up: a publisher reads the
earlier posts the owner could read, so adding a publisher is now also handing over the
channel's recent history, which the UX states (section 15.2.1).

**Invite links in the link register (phase B, 2026-11-06).** The ADR-0079 register now
holds `/g/` links: kinds `group` (3), `channel` (4) and `publisher` (5), each with its
target (the 32-byte group id, or the 16-byte channel id), the sealed capability the
ledger holds (so another device can show and re-share the link) and, for a private
channel subscriber link only, the 32-byte invite seed and its admission-key version
(secret, left out of `Debug`). A `/g/` record is filed under `group_link_record_id`,
the first 16 bytes of `BLAKE3-derive-key("lakko/sync/group-link-id/v1", ledger_id)`,
so every device files one link under one key. Revocation stays final. Envelope version
`0x04`, persisted state version 11; older ones are refused (flag day). A `/g/` record
never answers a contact request (the verdict is `Dead`).

### 8. Changing the flag

Only the owner changes it. Each direction is one signed `PUT /discovery/handle`,
then the in-group fact (section 11), then whatever the kind needs. The change is
recorded as owed before the first step and retried.

| | Public to private | Private to public |
|---|---|---|
| Name | stays, stops answering at once | stays, starts answering |
| Record, terms | deleted | written |
| Group: links already out | keep working: they are invitations | keep working |
| Group: pending requests | stay until an admin answers; a request already asked is not thrown away | stay |
| Group: new knock from a stale record | dropped, no capability; the asker waits out its 7 days | taken |
| Channel: subscribers | stay; a new epoch, the public seed deleted, the seed sealed to each key once (ADR-0030 stage and commit) | stay; a new epoch, the read key and public seed published |
| Channel: Discover | listing withdrawn | listed (section 16) |
| History | newcomers read nothing from before, as always; a new publisher reads the seeds the owner hands over (section 7, 2026-11-08) | same |

Going private at one million subscribers costs one 80 MB reseal, or one tree build.
Stated in the UI, resumable.

### 9. When a membership ends

Decision 3 reverses ADR-0045 UX sections 6.2.4 to 6.2.7. The trigger is a state, not
an event: **after applying a commit, the group is not active and the frozen roster
holds no leaf of this account.** `is_active` alone is not enough: a device whose own
leaf was retired while the account stays in the group (ADR-0050) is re-admitted, and
must not lose its history.

The device, in this order, from one durable owed record:

1. Records the end (title and left-or-removed, for the notice).
2. Publishes a `conversation_ended(ref)` register into the account's synced state
   (`lakko-sync`, ADR-0058): strictly newer stamp wins, in the snapshot, cleared by a
   later join.
3. Stops the poll, the beacon and the shrink rotation, and retires the push
   subscription (UX 6.2.8, unchanged).
4. Wipes: the message log entry, the MLS group through a new `Group::delete`, the
   ADR-0040 outbox entries, the group name, handle and role caches, pins, read
   cursor, draft, feed candidates, saved messages from that conversation, reach
   grants learned there, its knock and journal cursors, and cached media.
5. Deletes the conversation vault and its index entry (ADR-0064, when built), so a
   re-add finds nothing.

A device that hears of it from the register does 3 to 5 without applying the commit.
This is what reaches a device offline past the journal's 30-day retention.

**Built 2026-10-31 in the core, with one change to step 2.** The register carries the
MLS epoch of the end, and of a join, and the fold orders it by `(epoch, stamp,
canonical bytes)`, not by the stamp alone. By stamps a device that was offline across
a removal and a re-add would apply the old removal when it came back, write "ended"
with the newest stamp of all, and delete the group from every device that had rejoined.
A group keeps its id through both events and its epoch only rises, so the epoch says
which came later whatever the clocks say. A channel has no epoch and writes 0, so
there the stamp decides, as written above. `Group::delete` became two entry points:
`delete` for a group that ended here, and `delete_after_ended_elsewhere(identity,
ended_epoch)` for a device acting on the register, which refuses an active group above
that epoch. A device that holds such a group has come back since, and answers with a
`joined` register.

The register is never dropped: the journal's 30 days bound what a device can read, not
how long it can stay offline, and a device past that horizon catches up from the
snapshot, so no age can be shown safe. What bounds it is a client-side limit of 4096
registers (`CONVERSATION_ENDS_MAX_CLIENT`, chosen, about 230 KiB at rest), past which
a device still wipes a conversation but stops telling its other devices, and a first
join writes no register at all. Whether a person left or was removed is not in the
register, so a device that hears of it from the register shows the notice for a leave
made on another device.

**Re-added.** A Welcome makes a new conversation under the same group id with empty
stores, which is what "only new messages" means. The register is cleared by a stamp
newer than the end.

**Findability.** A private name never answers, for an ex-member or anyone. An
ex-member may still hold a live link; it can ask, and the admin decides. A public
group can be found and asked again.

**Leaving and unsubscribing** do the same (maintainer, 2026-10-31). One rule is simpler
than two, and a person who left has no way to send to the group, so a transcript kept
is a transcript that ends mid-air.

**A closed app.** The commit is applied at the next start. Until then a wake still
reaches the device, the background path's throwaway group can no longer open rows
after the removal, and the notification is the generic one, once or twice until the
ADR-0043 rotation stops the wakes. C-59 forbids the background path from writing, so
it cannot wipe, and it does not need to learn more.

**Not promised.** A modified client, a powered-off device, a screenshot or a forward
keep what they hold. The wipe is what our clients do, not something the server can
enforce. A sole device offline past 30 days hears nothing and stays in the ADR-0064
waiting state with its history; wiping on a refused read would turn a server fault
into data loss.

### 10. A solo group

`Group::create` already works with one member. The client founding path takes zero
picks. Consequences, none of them a new mechanism:

- Other devices of the account join in the founding commit (ADR-0050).
- The ADR-0057 claim, the handle claim and the routing secret happen at creation.
- The owner is rank 0 and sweeps its own first add. The first add is an ordinary add.
- `people` counts accounts, not leaves. With one person: no call controls, no beacon
  (`should_emit_beacon` is told nothing new; the ticker does not start), no
  transfer control.
- The creation row is a timeline row, so the group sorts by it (`navigation.md`
  section 2.2).
- An owner alone can leave only by deleting: **Delete group** retires the name and
  wipes as section 9. With other people in the group an owner transfers first.

### 11. The handle inside the group

Every member sees the name and the flag. A new envelope
`lakko/conversation-handle/v1\0 ‖ version ‖ updated_at_ms ‖ flag ‖ handle_len ‖ handle
‖ cert(169) ‖ sig(64)`, on an ordinary MLS application message. `sig` is by the handle
key over the group id and the fields, and the certificate binds the name to that key,
so **any member can relay it**: a newcomer gets it at the growth re-send (ADR-0048
path) whether or not the owner is online. Strictly newer stamp wins, carried verbatim
on a re-send (the ADR-0044 lesson). No role check is needed at receipt, and no
`GroupContext` extension: ADR-0051 rejected one because a change would need a commit.
Bounded, fail-closed, dispatched as `IncomingEnvelope::ConversationHandle`, suppressed
by the three background filters, fuzz target `conversation_handle_decode`.

A channel carries the same fields in its metadata record (ADR-0051 v3, flag day).

### 12. A channel is a conversation

One conversation screen, one info pattern, one message body. `/channels/:id`,
`ChannelsLayout`, `PublishComposer`, `PostCard`, `ManagePanel` and the channel-specific
header go; a channel opens in the view a group opens in.

| | Group | Channel |
|---|---|---|
| Row, ordering, folders | the same | the same |
| Header, timeline, day separators, media viewer | the same | the same |
| Composer, attachments, voice, video note, GIF, emoji | the same, for everyone | the same, for the owner and admins; everyone else has no composer and no replacement bar |
| Message menu | the same | Reply is absent; Edit and Delete for the owner and publishers; React as section 13a |
| Call controls | the same | none |
| Comments | none | under each post, section 13a |
| Info screen | `lakko_ui_rules::info` | the same function, section 15 lists the blocks |
| Owner, admins | the same words | the same words |
| Only a channel has | | no sender on a post (the channel is the sender), a subscriber count in place of a member list on the header line, Subscribe and Unsubscribe in place of Join and Leave |

A sender is shown on no post because a post names a key, not a person; which admin
wrote it is not in the wire and is not added.

What the Manage cards did moves or goes:

| Card | Where it goes |
|---|---|
| Channel (name, description) | rows on the info screen, as a group's rename |
| List in Discover | folded into the flag (section 16) |
| Subscribers | the `Subscribers` row (section 15) |
| Co-publishers | `Admins` (section 15) |
| Protection, "Refresh now" | **gone.** Rotation runs by itself; the one case a person acts on is removing someone, which rotates |

**The machinery leaves the screen.** The rotation cron and the co-publisher knock poll
move out of `RotationPanel` and `CoPublishersPanel` into app-wide loops beside
`SubscriberAdmission`, on all three clients, so a channel rotates whether or not its
owner opens its screen. Each pass reads only channels this device publishes; the rotation
pass runs only for channels this device owns (2026-11-08, section 15).

### 13. A post carries what a message can

**The plaintext of a post is the chat envelope.** `classify_post_plaintext` keeps its
domain check for the metadata record and otherwise hands the bytes to
`lakko_attachments::parse_body`, so a post is `MessageBody::Chat` with text and
`AttachmentRef`s, ADR-0047 metadata included: photo, file, voice, video note, GIF. One
codec, one bound set, one fuzz target, one inline-media rule. `compose_post` is
unchanged, since it seals bytes; the `ChannelPost` wire and its signature do not move.
`MAX_PLAINTEXT_LEN` (16 MiB) is ample: refs are small.

- **The file is an ordinary attachment.** `encrypt_attachment`: a random key per file,
  the ciphertext uploaded to `/blob`, the key and blob id inside the sealed post. The
  epoch seed protects the post, and the post protects the file: whoever opens the post
  can open the file, as with a group's epoch. No key is derived from the seed, so a
  forward out of the channel needs no special case, and a seed rotation re-encrypts
  nothing.
- **A subscriber who joined later** reads what its seeds open. Public: every post in the
  retained 30 days, files included. Private: posts from the epoch current when it was
  let in. Nobody reads a post past the retention, and a file then has no reader.
- **The ADR-0065 rules apply, with a shorter life.** A year's idle sweep would keep a
  file 335 days past the last post that names it. A blob upload gains an optional
  `idle_days`, one byte in the index row; a channel file sets 45 (30 days of posts and
  15 of margin, chosen, not measured). Upload and read passes are as for chat. A
  `Delete` removes the file under the uploader's delete key, as in a chat; a file of a
  deleted post uploaded by another admin stays until its idle life ends.
- **Edit and delete** are the ADR-0023 ops with the post's server cursor as the target.
  A reader applies one from any publisher key the post signature accepts, so an admin
  can delete another's post, as an admin of a channel should.
- **Typing and read receipts** do not exist in a channel, and a post takes no reply: a
  subscriber writes comments (section 13a).

### 13a. Comments and reactions

A subscriber holds a read key and no MLS leaf, which is what lets a channel have a
million of them: a leaf per subscriber would send every device a commit per join. So a
subscriber's write cannot be a group message. A reaction delivered to every subscriber
would be a thousand reactions times a million devices on one post. It goes to the channel
service, and readers get a total.

**What the maintainer refused (2026-11-01):** a per-channel subscriber key on every write.
It would hand the service one person's activity in a channel, and that profile is what an
IP address and a time link to a person. So nothing here reuses a key from post to post.

**A key per write (decided 2026-11-02, maintainer).** Every reaction and every comment
is signed by a key of its own, so the service cannot link even one person's reaction and
comment under one post. A reaction's key is `BLAKE3-derive-key(
"lakko/channel/reaction-write-key/v1", account_seed || channel_id || post_cursor ||
label)`: adding and taking off one reaction use one key, two reactions of one person on
one post have unrelated keys. A comment's key is `BLAKE3-derive-key(
"lakko/channel/comment-write-key/v1", account_seed || channel_id || post_cursor ||
comment_nonce)`, the 16-byte nonce random and kept in the account's own-comments register
(below), so any of the account's devices can delete the comment. Every write carries the
`x-lakko-channel-write-proof` header: a signature by the write key over the channel, the
post, the write, a time and a nonce; the service refuses a nonce it has seen (a captured
add cannot be replayed after the reaction was taken off).

**Reactions.** A row is `(post, writer_key, label)`, unique. `label =
BLAKE3-keyed(reaction_key, post_cursor || emoji)`, 16 bytes, where `reaction_key` derives
from the epoch seed of the post, so the service holds no emoji and cannot compare labels
between posts. Taking a reaction off is a signed delete of that row by the same key, which
needs no token.

- **Any emoji, as in a chat (decided 2026-11-02, maintainer).** A reaction is any emoji
  the chat rule accepts. Readers get `{label: count}` for the posts in view (at most 50 a
  call, at most 64 labels a post), in one batched, cacheable call, and resolve labels by
  trying every emoji their app knows (the bundled emoji set and their recent ones). An
  unknown label is dropped, so junk from someone with no seed shows nothing, and an emoji
  an older app does not know is not shown there.
- **Allowed reactions.** The owner sets all, none, or a list of up to 100 in the channel
  metadata. Readers and the composer hide what is not allowed. The service cannot enforce
  it: it sees labels only.
- **At most 3 reactions per person on a post (decided 2026-11-02, maintainer).** The app
  enforces it from the own-reactions register; nobody else can count a person's
  reactions, since each has its own key. A modified app can add more; two devices adding
  at the same moment can briefly leave four.
- **The account's own reactions and comments** live in two `lakko-sync` registers, shared
  among its devices over the DeviceGroup and in the backup, and are never asked of the
  service: a request naming several write keys would link exactly what they keep apart.
  The registers are bounded to the channel's retained posts: a post past its 30 days takes
  its entries with it. Since phase B (2026-11-06) the reaction register also holds the account's
  reactions on comments: its key is `(channel, post cursor, comment cursor or none, emoji)`,
  three reactions a comment counted apart from the post, and a comment's entries go with
  its post. The envelope (`lakko/sync/channel-reaction/v1`) is version `0x02`, a target
  byte and a comment cursor after the post cursor; `0x01` is refused, and the persisted
  state is version 12.

**Who may write.** One anonymous token per added reaction and per comment, in contexts of
their own (`channel-react`, `channel-comment`, ADR-0054), so the service learns the writer
holds a token and not whose. A person out of `channel-react` tokens does not see a failure:
the reaction is drawn at once and leaves with the next refill. Quotas are chosen with room
to spare and are not measured. A token does not prove one person one reaction: a modified
client can use another derivation and add up to its quota. That inflates a counter and is
accepted. So is a second effect: a token holder can add labels no emoji produces, and since
a reader gets at most 64 labels a post, a flood of them can push real reactions out of a
busy post's counts. The order at the service is proof, nonce, the post exists, then the
token, so a malformed or misdirected write spends nothing; a nonce row (kept 240 s) is
written for any validly signed request, bounded by the request rate in front of the
service.

**Comments.** A row is `(post, cursor, writer_key, sealed)`, at most 2,048 bytes.
`sealed` is an AEAD under a key from the post's epoch seed over the name, an avatar
reference, the text, a reply target and the signature of a per-channel author key, which
exists only inside the seal. An author's delete is signed by that comment's write key; a publisher's
is signed by a publisher proof, as a post's is. Pages are 20 rows, cursor first. Comments
take no attachments in this version: a file would need a pass and a lifetime of its own.

**Banning.** The service has no key to refuse, by design. In a private channel a ban is
the existing removal: no seed from the next epoch. In a public channel reading cannot be
refused: the ban is a sealed, publisher-signed list of author keys, carried like the
metadata record and re-issued after a rotation, and every reader hides what those keys
wrote. A public ban hides comments; reactions cannot be banned, because no one knows whose
they are. Up to 1,024 keys a list (chosen); oldest go first.

**What the service still learns.**

- For a post: how many rows it has, and how they split across labels. Which emoji is
  hidden, and the post itself is ciphertext. Hiding the split needs additive homomorphic
  encryption and a zero-knowledge proof per write, or a client adds a thousand at once;
  that surface is out of proportion to what it hides, and was declined (maintainer,
  2026-11-01). The total cannot be hidden at all: the service stores each write. It could
  only be blurred by dummy writes, which makes counters inexact.
- The address and time of a write, as with every request, until a relay stands in front
  (ADR-0086's release gate is that relay; the same one takes these writes).
- In a public channel the read key is public, so an operator who reads it opens comments
  and labels like any reader. A comment in a public channel is public speech under a name
  its author typed. It carries no account key, for that reason.

**Budget.** Arithmetic, assumed, not measured. A reaction row is about 64 bytes (16 of
post id, 32 of key, 16 of label): 640 KB at 10,000 reactions. A reader gets at most 64
labels with a count, about 2 KB for a post, usually far less. A comment page at the cap is
41 KB. Nothing a reader does grows with the number of reactions or comments on the post.

**Phase B (2026-11-06): comment counts, reactions on comments, a deleted channel.**
The counts read `GET /v1/channels/{id}/reactions?posts=` returns each post's comment count beside
its labels, from a counter the service maintains in the same transaction as each
comment add and delete (`channel_comment_counts`), never a `COUNT(*)`. A comment takes
reactions the way a post does: `lakko_channels::CommentReactionKey` (HKDF info
`lakko/channel/comment-reaction-key/v1`, labels over `post ‖ comment ‖ emoji`, so a
comment label never equals a post label), a write key per (account, channel, post,
comment, label) under `lakko/channel/comment-reaction-write-key/v1`, write-proof ops 5
(add) and 6 (delete) whose subject is `comment_cursor ‖ label`, one `channel-react`
token per add. Routes: `POST .../posts/{post}/comments/{comment}/reactions` (201, 200
on a repeat; 404 `unknown-comment`), `DELETE .../comments/{comment}/reactions/{writer}/{label}`
(204), and the batched `GET .../posts/{post}/comment-reactions?comments=` (at most 50
cursors; `bad-comments` / `too-many-comments`). A deleted comment takes its reactions
with it. The owner deletes a channel with `DELETE /v1/channels/{id}` (owner proof over
an empty body, 204): the rows go and a tombstone stays (`channel_deleted`); from then
on every route that names the channel answers `410 channel-deleted`, a repeat delete
answers 204 without a proof, and a create of that id is refused with the same 410.
Migration `0012`. The service still holds no key and reads no row.

### 14. Inviting people to a channel

For the owner and admins, public and private, from one row, `Invite people`.

- **From contacts.** The Add members screen ([`conversation-info.md`](../ux/conversation-info.md)
  section 2.4) with its network half off: contacts only, because the invitation is a
  message in the one-to-one and a stranger has none. It differs from a group in one
  way, and for a reason: a group's add puts a person in, while a channel's invitation
  waits for them to press Subscribe, since a read-only stream they never asked for is
  spam, and since subscribing needs a key only their device makes.
- **The message.** The chat envelope gains `channel_invite`, a sealed link wire, shown
  as a card with `Subscribe` (a one-use link per person when private, the channel's
  record when public). A contact with no one-to-one gets one started.
- **By link.** The section 7 link, for public channels too: there it carries the read
  key and needs no invite key. `Copy link`, expiry, use limit, list and `Revoke` are the
  group's.

- **A publisher's invitation (phase B, 2026-11-06).** The co-publisher link (preview
  kind 2) now carries the name of the publisher who made it, under the display-name
  rule (`compose_co_publisher_invite_capability(..., inviter_name)`), and the channel
  invitation card opens kind 2 as well as kind 3, so `Add publisher` can send the card
  in the one-to-one. A channel preview carries no member count.

### 15. Subscribers and admins

**What a publisher may see.** The service holds per-channel random keys and knows no
one. The only names are ADR-0060 member cards, and a card is a person's choice. So:

- **A count.** `GET .../subscribers/count` (publisher proof), a maintained counter, not
  a `COUNT(*)`. An exact number to the owner and admins; the band to everyone else, from
  ADR-0059 section 4.
- **Names where a person shares one.** The `Subscribers` row opens the member list, the
  group's member screen, read from `GET .../members` (paged, ciphertext). The rest are a
  count: "{n} others". A private channel's unnamed subscribers show the link they came
  by, read from their ticket on the publisher's own device, so each has a row to
  `Remove`.
- **Nothing about seed delivery.** No "Ready", no "Setting up", no retry. A failure is
  retried by the loop, and the person it affects sees their own waiting state.
- **At a million subscribers** the row is a number. The screen pages through cards as
  scrolled, a page at a time; it is not a roster and cannot be searched past what is
  loaded, and nobody reads the key list. Public channels offer no `Remove`.

**Co-publishers stay co-publishers (corrected 2026-11-01); on the screen they are
publishers.** A publisher posts as the owner does, edits and deletes posts, deletes
comments, bans, invites and removes subscribers; the owner alone renames, adds and removes
publishers and changes the flag. The limit: a PublisherGroup seeds no role table, so "only the owner adds a
publisher" is held by the client and by the appointment the service checks, not by every
peer as in a group. An unappointed member would get the seed and could not post. A role
table in the PublisherGroup would make it a peer rule and is a `lakko-mls` change; it is
not proposed here.

- `Add publisher` (owner) picks a contact and sends a one-use link in the one-to-one; the
  owner's device approves that person's request without asking, since it named them.
  The `Join as co-publisher` menu item goes: a link opens the page.
- `Remove publisher` revokes the appointment, removes the leaf and rotates. `DELETE
  .../copublishers/{pk}` exists and no client flow calls it; a member list on the
  PublisherGroup FFI is what it needs.

**Removing a publisher and stepping down (2026-11-06, phase B; an invariant #3 change).**
`PublisherGroup::remove_publisher(owner, account_key)` removes every leaf of that
account in one commit, merged at once. Only the owner (the account whose seed derives
the PublisherGroup id, `is_owned_by`) may call it, and the owner's own account key is
refused. A co-publisher leaves with `PublisherGroup::step_down`: OpenMLS does not let a
member commit its own removal, and a table-less group stores no proposals, so the
step-down is an encrypted application message `lakko/publisher-step-down/v1\0 ‖ 0x01 ‖
channel_id` (46 bytes) that the owner's device applies as `remove_publisher` of the
account the MLS sender resolves to at receipt (C-68); the owner cannot step down.
Either way the next epoch's channel seed is out of the removed person's reach (RFC 9420
section 12.4), and the owner then rotates and reseals as after any membership change.
Both rules are held by the owner's client: peers do not enforce them, the same limit
this section states for adding. Property tests in `tests/publisher_group_removal.rs`:
a removed publisher cannot derive the next epoch's seed, and after a step-down the
owner rotates with the remaining publishers. `PublisherGroup::members()` lists the
publishers with their account keys, which is the member list the `Publishers` block
and `DELETE .../copublishers/{pk}` needed.

**A day, not 7 days (amended 2026-11-12, maintainer decision).** A device asks once per
group, and once per channel for a publisher request, per day: "No answer yet." and `Ask
again` appear a day after the request, not 7 days. Seen walking base scenario 20: the page
a group's invite link opens offered `Request again` at once (the September invite-page
spec), while the Requests sent list said "Waiting for an admin." for the same request, so a
refused person could re-send without limit. The invite page now follows section 6 of the
UX spec: "Request sent. An admin will see it when they next open Lakko.", no button until
the day is over, and the chat opens by itself when the join lands. Where this ADR says 7
days for a request below, read a day; the DS row lifetime of 7 days is unchanged (a second
request from the same device is the same joiner to the admin's pending list, ADR-0027
dedup by signature key).

**A publisher request with no answer (amended 2026-11-08, maintainer decision).** A
publisher request the owner's device rejected or never answered follows a group request
(UX section 6.2, applied in UX section 15.2.2): the invitation card shows the waiting line
for 7 days, then "No answer yet." with `Ask again`; a rejection sends nothing, so the
person is never told of a refusal. `Ask again` re-sends the same request through the same
`/g/` link the card carries: fetch the ledger row again, consume it again (the ledger
counts a joiner once, ADR-0028 OQ1, so a one-use link is not used up), and knock again
as the first `Accept` did, replacing the device's pending-request record and restarting the 7 days.
A device asks again at most once per channel per 7 days. When the ledger answers the link
is gone, revoked, expired or used up, the card shows "This invite link is invalid or has
expired." and keeps the earlier pending record, so a late approval still lands. No server
or core change.

**Only the owner rotates (amended 2026-11-08, maintainer decision).** The channel key is
rotated by the owner's device alone: the scheduled rotation (24 h or 1,000 posts, ADR-0031
`rotation_policy`) and the rotation after a membership change (an approved, removed or
stepped-down publisher). A co-publisher's device never runs the rotation policy and never
commits a rotation. The reason is the DS: a PublisherGroup journal accepts a commit only
from the owner (its registrar chain, ADR-0057), so a co-publisher's rotation merged
locally and refused on the wire forks that device's group from the owner's, and its
later posts are sealed under a seed no subscriber holds. The cost: a channel whose owner
does not open Lakko does not rotate, on schedule or after a membership change, until the
owner returns (a removal is itself the owner's commit, so the rotation after it runs on
the owner's device at once; a step-down waits for the owner's device anyway). No core or
server change; the clients drop the rotation pass for channels they
do not own.

**A publisher asks the owner to rotate (amended 2026-11-08, maintainer decision).** A
publisher may remove a subscriber (this section), and a removal is followed by a rotation
so the removed key reads nothing new; a co-publisher can no longer rotate, so it asks.
`lakko-contacts::channel_rotate_request` is a fixed 49-byte envelope
`lakko/channel-rotate-request/v1\0 ‖ 0x01 ‖ channel_id(16)`, sent by the publisher as a
PublisherGroup application message right after the removal; it carries no subscriber, no
reason and no time. The owner's device rotates at its next publisher pass when it read one
on that channel's PublisherGroup from a current publisher (the MLS sender resolved at
receipt, C-68) and drops one from anyone else; requests that arrive before the rotation
collapse into it. Dispatched as `IncomingEnvelope::ChannelRotateRequest` (the channel id);
fuzz `channel_rotate_request_decode`; recognised only by the iOS notification extension.
The cost, stated in the UX (section 15.2): a subscriber a publisher removes stops reading
new posts only once the owner's device next opens Lakko. The removal at the channels
service is immediate, so a removed subscriber stops receiving new seed envelopes at once;
what it keeps until the rotation is the current seed, which opens what is posted under it.

The info blocks, in `lakko_ui_rules::info`:

| Block | Who sees it |
|---|---|
| Name, @name, header line, description | everyone |
| Rename, description | owner |
| Group type (`Who can subscribe`, @name, topic, language) | owner |
| Invite people, invite links | owner, publishers |
| Subscribers | owner and publishers: the exact count and the ban list; subscribers: the member list as today |
| Publishers | owner; publishers read-only |
| `Members can find me` | everyone, per ADR-0060 |
| Unsubscribe | a subscriber |
| Delete channel | the owner, when no publisher remains |

### 16. `listed` folds into the flag

It fits ADR-0059. A listing is "the public copy of the metadata the owner chooses to
publish" (section 3 there), and that is what going public now does. `listed` was a
second answer to one question.

- **Public means listed.** Going public publishes the listing as well as the record.
  `List in Discover` goes; a private channel is never listed, as before.
- **Topic and language** move to the channel's type screen, shown when public. Topic
  defaults to Other (id 0), language to the app's language, so no choice blocks anyone.
- Metadata's `listed` byte becomes the flag of section 11. `Share my channels` shares the
  public channels a person follows.
- Listing stays where it is, in `services/channels`, with its owner key, ranking and
  moderation. A channel the operator blocks from Discover is still found by search.
- **Cost:** an owner cannot be findable by name and absent from suggestions. Nobody
  asked for that split, and it can come back as a switch if someone does.

### 17. Migration

Flag day, as usual: no production users. Test stacks are reset. A test group that has
no handle needs no compatibility code, because a group whose lease lapsed is the same
state: the owner's device asks for an @name (UX section 4.4).

### 18. The photo

Added 2026-11-02 (maintainer: groups and channels have photos too). The five choices
of 18.8 were decided by the maintainer the same day; the mechanism is proposed. UX:
[`group-and-channel-privacy.md`](../ux/group-and-channel-privacy.md) section 4.5.

**The photo of a group or channel is a profile photo with a different owner.** The
bytes are what [ADR-0084](0084-profile-photo.md) uploads for a person: a square JPEG
still (at most 640 px, 256 KiB) and optionally a square H.264 MP4 without sound (at
most 480 px, 4 MiB, 10 s; the video is allowed, decided 2026-11-02, maintainer), each
encrypted with `encrypt_attachment` under a fresh key
and stored as a blob. What changes is who may set it, how the reference travels and
who keeps the file alive.

**One codec.** The ADR-0084 avatar field, `avatar_len(1: 0 | 84 | 170) ‖ avatar` with
its bounds (square, sizes, duration; any violation refuses the whole envelope), moves
out of `lakko-contacts::profile` into a module of its own, `lakko-contacts::avatar`,
used unchanged by the profile and by every carrier below. The profile wire does not
change. `lakko-channels` already depends on `lakko-contacts`, so the channel record
uses the same module.

#### 18.1 To the members of a group: an envelope of its own

```text
envelope = "lakko/group-photo/v1\0" ‖ version(1)=0x01 ‖ updated_at_ms(8, BE)
           ‖ avatar_len(1: 0 | 84 | 170) ‖ avatar
```

Exactly 31, 115 or 201 bytes. `avatar_len = 0` removes the photo. Bounded, fail-closed,
dispatched as `IncomingEnvelope::GroupPhoto`, never rendered, fuzz target
`group_photo_decode`, the cross-kind separator test extended.

It follows ADR-0051's group name in everything but its content:

- **Authority at receipt.** The receiver resolves the sender to its account key (C-68)
  and reads the live role table: owner or admin, accepted if newer; anyone else, an
  unresolvable sender or an unreadable table, dropped. The read-decide-write runs
  under the per-group lock (C-62 sub-invariant 5). A one-to-one drops it, as it drops a
  group name.
- **Order.** Strictly newer `updated_at_ms` wins, per group; equal is not newer. The
  stamp is stored and re-emitted verbatim on a re-send (the ADR-0044 lesson).
- **Carriers and timing.** Sent after the creating commit when the group was created
  with a photo, on every change, and on growth, re-sent by the owner and admins that
  hold one on the name's schedule; the device that committed the add sends at once
  (ADR-0051 section 3 as amended 2026-10-31).
- **Background filters.** The web service worker, the Android background decrypt and
  the iOS NSE recognise and suppress it; the extension surface gets decode only.
- **Store.** `{ avatar, updated_at_ms }` per `(account, group)`, beside the name.

**Why not a second version of the group name envelope.** A name and a photo are
changed independently, often by different admins. One record with one stamp means the
admin who renames sends the photo it last saw, and the admin who changes the photo
sends the name it last saw; the later stamp wins whole, so a rename made a moment
before a photo change is undone on every device. Two envelopes keep two registers.
The cost is one more journal row per growth event, at most 201 bytes of envelope.

**Why not the conversation-handle envelope (section 11).** It is signed by the handle
key, which only the owner holds (section 3), and admins set the photo. Its relay rule
(any member may relay, because the signature is checkable) does not carry over: a
photo envelope has no signature a member could check, only the sender's role.

**Why not a `GroupContext` extension.** ADR-0051's reason: every change would be a
commit, a proposal and an owner sweep under ADR-0045, and a `lakko-mls` change.

#### 18.2 To the subscribers of a channel: channel metadata v3

The v3 record (sections 11 and 16, not shipped) gains the field after the handle:

```text
... ‖ public ‖ handle_len ‖ handle ‖ avatar_len(1: 0 | 84 | 170) ‖ avatar ‖ reactions_mode ‖ ...
```

At most 171 bytes more per record. The record keeps its one stamp: it has one writer
(the owner renames, describes and sets the photo, section 15), and a publisher's
re-issue after a rotation carries the stored record, photo included, verbatim. As for
the name, "only the owner" is held by the clients; the service checks only that a
publisher signed the post (section 15's limit, unchanged). A subscriber learns the
photo with the name from the newest record, and a new subscriber from the record of the
newest epoch.

#### 18.3 In the public record, and what public means for the key

The handle record payload (section 4) gains the field after the size band, with the
first byte moved from `0x41` to `0x42` (flag day; a `0x41` payload is refused):

```text
payload = 0x42 ‖ kind ‖ updated_at_ms ‖ id_len ‖ id ‖ key ‖ handle_len ‖ handle
          ‖ name_len ‖ name ‖ description_len ‖ description ‖ size_band
          ‖ avatar_len(1: 0 | 84 | 170) ‖ avatar
```

`HANDLE_RECORD_PAYLOAD_MAX_LEN` goes from 1,391 to 1,562 bytes and
`PUBLIC_RECORD_MAX_LEN` from 7,411 to 7,582 (arithmetic: the handle record is the
larger of the two payload kinds); the `public_records.record` column is widened to
match, by a migration the existing pinning test checks.

**The blob key is then public.** A public record opens for anyone holding a
`discovery-lookup` token and a search term, and for the operator, who holds the OPRF
key, for any name it can guess. So the photo of a public group or channel is a public
file: anyone who opened the record can download and decrypt it while it lives. That is
the point of a public photo, the same class as the public name, and it is said in the
dialog that makes a group public (UX section 4.1).

**One blob, not a public copy beside a private one.** A separate copy for the record
would hide which addresses fetch the members' copy. It hides nothing new: the record
already names the group id, and the Delivery Service already sees which addresses read
that journal (class (c), section 4). For a public channel the metadata record is
readable under the public read key anyway.

**Going private re-keys the photo** (decided 2026-11-02, maintainer). The flag change (section 8) gains one owed step:
the owner's device uploads the current photo again under a fresh key and sends it as an
ordinary change with a new stamp (a group: the envelope; a channel: the record of the
new epoch), so a key that was published opens nothing current. The published blob is
deleted if this account uploaded it (18.5), else it ages out. Someone who already saw
the photo still has it, as with the name.

#### 18.4 In invite links

The sealed link preview (`lakko-contacts::group_invite`) gains `avatar_len ‖ avatar` at
the end of kinds 1 (a group), 2 (a co-publisher link) and 3 (a channel subscriber
invitation), so the invite page and the channel link page show the photo the link was
made with. A link holder can then fetch that photo, which is what holding the link
already allows for the name. A visitor who is not signed in has no read pass and sees
initials (ADR-0065 section 5).

Bounds, arithmetic: the largest preview (kind 3) grows from 1,366 to 1,537 bytes of
plaintext, `MAX_WIRE_LEN` from 1,794 to 1,965, the `/g/` ledger column
(`group_invites.sealed_capability`, migration `0036`, 1,824 bytes) to 1,995. A knock
carries the capability: a key package of at most 4,096 bytes, a 134-byte reply grant, a
profile of at most 926 bytes and the capability of at most 1,965 fit the 16 KiB row.

#### 18.5 Keeping the file, and deleting it

**Who keeps it alive.** ADR-0065 deletes a blob nobody accessed for 365 days. Member
devices download the photo once and cache it (ADR-0084 section 3), so a settled group
reads its photo rarely and the file would expire under people who still see it. The
keepers do not depend on the setter: **every device of the owner and of each admin**
(a channel: of the owner and each publisher) adds the current photo's blob ids to one
keep source, `conversation-photos`, sent on the ADR-0065 tick. When the setter leaves,
the remaining owner and admins keep it; a group always has an owner (C-63). Decided
2026-11-02, maintainer.

**Keeping is automatic.** No person does anything for it: there is no control, no
prompt and no state on any screen. The device works out from its own role in each group
or channel whether it keeps that photo, and the keep tick sends it. The tick runs
whenever Lakko is open on one of those devices (ADR-0065: foreground, every 10 minutes,
each source due every 30 days); a closed app sends nothing, which is what the 365-day
lifetime allows for. A device that stops being owner or admin drops the photo from its
source at the next tick.

One source, not one per conversation: a tick sends at most 4 sources and each is due
every 30 days, so 10,000 per-conversation sources would need 84 ticks a day, 14 hours of
foreground at one tick per 10 minutes (arithmetic on `MAX_SOURCES_PER_TICK` and
`KEEP_INTERVAL_MS`). One source is batched by 256 ids: an admin of 10,000 conversations
with a photo and a video each keeps 20,000 ids, 79 requests and about 1.3 MB a month
(66 JSON bytes an id). The service writes at most one access per blob per day, so more
keepers add requests, not index writes.

**Who deletes it.** The ADR-0065 delete key stays the uploader's (derived from the
uploading account's lineage seed and the blob id). A device that uploads a group or
channel photo keeps a small durable record of the blob ids of the photos it set. When a newer photo replaces or removes one
it recorded (its own change or another admin's, seen on receipt), the device deletes the
old blob, retried from the durable list. A replaced photo whose uploader is no longer in
the group, or whose uploading device is gone, is not deleted: nobody keeps it any more,
and it expires after 365 days unread. `Delete group` deletes the photos the owner's
device set. Decided 2026-11-02, maintainer.

#### 18.6 Budget

Arithmetic on the bounds above, not measured. A real still is usually far under 256
KiB; the ceilings are what the bounds allow.

| One device | Figure |
|---|---|
| Envelope or field held per conversation | at most 201 bytes; 2 MB at 10,000 conversations |
| Download per photo change | at most 256 KiB, plus at most 4 MiB for a video opened in a large view |
| Cached stills, 10,000 conversations, all drawn | at most 2.44 GiB (10,000 x 256 KiB) |
| Keep traffic, admin of 10,000 photos with video | 79 requests, about 1.3 MB a month |

The cache line is why decision 4 of 18.8 bounds the cache: today's avatar cache never
evicts.

| Server | Figure |
|---|---|
| A photo | stored once, however many members: at most 256 KiB, plus 4 MiB with a video; 2 index rows |
| Per 1% of conversations with a still, at 1B accounts (2B conversations, the Budget assumption) | 20M stills, at most 5.24 TB (4.77 TiB) |
| The same with a video each | at most 83.9 TB (76.3 TiB) more |
| Egress per change, channel of 1M subscribers, one device each | at most 262 GB (244 GiB) for the still |
| Journal | one row per change; one more row per holder per growth event, on the name's schedule |
| Channel record | at most 171 bytes more per re-issue; 5 KB a channel over 30 days at one rotation a day |
| Discovery | at most 171 bytes a public record; 17.1 GB at 1B accounts (100M public) |
| Search responses | at most 3,420 bytes more for 20 records; at most 6.84 TB a day more at 1B accounts at the Budget's search rate |
| `/g/` ledger | at most 171 bytes a link |

The egress line is the one that matters at scale, and it is the same as a channel's
media (Budget): ciphertext, immutable, content-addressed, so a cache in front of
`/blob` serves it once a read pass is checked there.

#### 18.7 What the servers learn

- Storage: a blob's size, day and last access, as for any file. Every member device
  fetches the same blob id, so an operator watching addresses and times can cluster a
  group's members by its photo (ADR-0084's residual, now per group). For a public group
  the record already names the journal, so the photo adds nothing there.
- A keep source batches the photos one person administers under one read pass, so the
  storage service sees that one device keeps these blob ids together (ADR-0065 residual
  1). For a private group a blob id names nothing the service can map to a group.
  Spreading a source's batches over several hourly passes would blur it; not proposed
  here.
- Discovery: the photo of a public group or channel, which is public.

#### 18.8 Decisions

All five decided 2026-11-02, maintainer.

1. **Video: allowed.** A group or channel may have a short video, as a person may: the
   same codec, crop and camera screens and cache, and no second rule. Cost: up to 4 MiB
   per photo and the egress line of 18.6.
2. **Keepers: the owner and admins** (a channel: the owner and publishers), whoever set
   the photo, automatically with no action by the person (18.5). Not chosen: every
   member device.
3. **Going private re-uploads the photo under a fresh key** (18.3), one upload owed with
   the flag change. Not chosen: keeping the published blob.
4. **One size-bounded avatar cache per client**, for people, groups and channels alike,
   the least recently drawn evicted first. The bound is chosen at build time and stated
   as chosen. An evicted photo is downloaded again on its next display, which counts as
   an access (ADR-0065 section 2).
5. **The uploader's devices delete a replaced photo** (18.5); a photo whose uploader has
   left expires after 365 days unread. Not chosen: a delete key shared by the owner and
   admins.

## Budget

The photo's budget is section 18.6.

Arithmetic on stated assumptions, none of it measured. Row sizes exclude database
overhead and indexes. Assumptions (chosen): 2 groups and channels per account, 5%
public, 9 search terms, 2 searches per account per day, 0.01 new conversations per
account per day. A typical record is 1,076 bytes (10 + 9 x 92 + 2 + 28 + 208), the
maximum 7,378 (65 terms, a 1,024-byte description). A term row is 80 bytes.

| Server | 1M accounts | 100M | 1B |
|---|---|---|---|
| Conversations / public | 2M / 100k | 200M / 10M | 2B / 100M |
| Private, per conversation | 194 bytes | 194 bytes | 194 bytes |
| Public, per conversation | 1,990 bytes (12,772 at most) | same | same |
| Discovery storage | 568 MB (1.6 GB at most) | 57 GB (165 GB) | 568 GB (1.6 TB) |
| Searches, average | 23/s | 2,315/s | 23,148/s |
| Claims / renewals, average | 0.1/s / 0.1/s | 12/s / 13/s | 116/s / 129/s |
| Search responses a day | 43 GB (295 GB) | 4.3 TB (30 TB) | 43 TB (295 TB) |

Search responses assume every search returns the full 20 rows, which is the upper
bound for a name and far above a `@name` lookup. A search spends two tokens, one fewer
than today.

| One device | Figure | Basis |
|---|---|---|
| Held per conversation | at most 60 bytes plus a 169-byte certificate | handle, flag, stamp |
| At 10,000 conversations | about 2 MB | arithmetic |
| Renewals, owner of 10,000 | 56 writes a day, about 17 KB | 300 bytes each, chosen |
| Ended conversations in synced state | about 500 KB per 10,000 | 50 bytes each, chosen |
| A search | at most 148 KB, typically 22 KB | 20 records |
| Admin reading a backlog | at most about 1 MB per 64 requests | row bound |

| One channel at 1,000,000 subscribers | Per-subscriber today | This ADR |
|---|---|---|
| Publisher upload per rotation | 80 MB, 200 calls | public: 64 bytes; private with tree: one envelope |
| Publisher reads while open | 386 GB a day | public: none; private: new rows only |
| Stored envelopes | 320 MB | public: about 2 KB; private with tree: edges 63 MB, once |
| Subscriber row | 64 MB | unchanged |
| A person approving | 116 days | not offered |

A file is stored once however many subscribe, so the storage line does not move with
subscribers and the egress line does. Assumptions (chosen): a channel posts 10 images of
200 KB, each with a 20 KB thumbnail, and one 8 MB video a day; a subscriber opens 30% of
it.

| One channel's media | At 1,000 subscribers | At 1,000,000 |
|---|---|---|
| New ciphertext a day | 10.2 MB | 10.2 MB |
| Held, 45-day idle life | 459 MB | 459 MB |
| Index rows held | 945 | 945 |
| Egress a day, 30% | 3 GB | 3 TB, about 35 MB/s on average |
| Index writes a day | at most one per live blob, 945 | the same |

The stored bytes are small and the bandwidth is not, and it is what a channel of this
size costs in any design: ciphertext is immutable and content-addressed, so a cache in
front of `/blob` can serve it, once a read pass is checked there. Today the blob store is
one filesystem on one host (`services/storage`), and it will not carry that egress.

| One public group | Figure |
|---|---|
| DS knock queue | at most 64 MiB (4,096 x 16 KiB), usually far less |
| Tokens to fill it | 4,096, about 3 accounts for a week at the quota |

## Alternatives

1. **Separate namespaces** (today). Cheaper at the server, and it hands out
   impersonation: a group can take the name of a person. Rejected.
2. **The handle key shared with admins, as a random in-group secret.** A removed
   admin would keep authority over the name, the race ADR-0057 records for equal
   ranks, and survives an owner's key reset only by paying that. Rejected: section 3
   accepts up to 210 days for an orphaned name instead.
3. **A session on discovery writes, with the owner's account on the row.** Simple,
   and it materialises who owns which group. Rejected by ADR-0078.
4. **No lease.** A dead name would be held forever, with no account to clean up
   after. Rejected.
5. **A separate "is this name free" route for live checks.** Same bit as a refused
   claim, at the cheaper token. Rejected; the live check is an ordinary lookup.
6. **Keep the external proposal in the knock and retry on a stale epoch.** The joiner
   cannot see a refusal, and a public group would throw away its queue with each
   admission. Rejected.
7. **Per-link knock keys** (ADR-0078 section 4 as written). A public group has no
   link; two rules for one row. Rejected for one versioned key.
8. **A channel kept as its own product, with cards fixed up.** Three clients would go on
   maintaining a second screen vocabulary, and attachments, voice and media would be
   written twice. Rejected by decision 5.
9. **A channel's file key derived from the epoch seed.** No key in the post, and a
   rotation would orphan files or re-encrypt them, and a forward would have to re-seal.
   Rejected: the attachment path is already built and proven.
10. **A separate post format for media.** A second codec, second bounds, second fuzz
   target for what the chat envelope already carries. Rejected.
11. **Keep `listed` as its own switch.** Two answers to "may strangers find this".
   Rejected.
12. **A public channel keeps per-subscriber sealing, automated.** It is what exists and
   it fails at the size the requirement names (section 6). Rejected.
13. **Human approval on a public channel.** Rejected: 116 days at a million.
14. **A plaintext record for public groups.** Smaller (no term slots), and it would
    hand the term-to-name mapping to a table copy. Rejected for one published shape;
    the cost is at most 7.4 KB a record.
15. **An exact-only search for groups, with name search only through Discover.** No
    term rows, and a public group you half remember cannot be found. Rejected by
    decision 8.
16. **Keep the transcript after a removal** (ADR-0045 UX 6.2). Reversed by decision 3.
17. **Wipe on `is_active` alone.** Destroys the history of a device about to be
    re-admitted. Rejected.
18. **Wipe when the DS refuses a read.** A server fault would delete a person's
    history. Rejected.

## Consequences

- **Positive:** a name means one thing. A public group or channel is reachable with
  no account linked to it on any server. Requests survive commits and absent admins.
  A million-subscriber public channel costs its publisher 64 bytes a rotation and
  needs nobody online.
- **Positive:** the known stale-knock loss and the cleartext knock row both go, for
  link joins too.
- **Positive:** channels drop a second screen vocabulary, and get media by reusing the
  chat body. A channel rotates whether or not its owner opens its screen.
- **Negative (2026-11-08):** only the owner's device rotates the channel key, so a
  channel whose owner does not open Lakko does not rotate (neither on schedule nor after
  a membership change) until the owner returns; its publishers keep posting under the
  current key meanwhile.
- **Negative:** every private group takes a global name. The client's proposed name
  hides that, and a private group can still take a good name from the public pool.
- **Negative:** a refused claim says a name is held (section 2).
- **Negative:** a person cannot be told a request was declined; a request simply goes
  unanswered and expires.
- **Negative:** a flood fills a group's request queue for about 3 accounts a week.
- **Negative:** a public channel has no removal, and its posts are readable by anyone
  who subscribes, the operator included. True today, stated now.
- **Negative:** a person's history is destroyed by someone else's action. Decided.
- **Negative:** an orphaned name of a reset owner stays taken for up to 210 days.
- **Negative:** channels have no reactions, and a person who lists no name is a number.
- **Negative:** a large channel's bandwidth is real and the blob store has no answer for
  it yet. A channel's photo adds to it (section 18.6).
- **Negative:** the photo of a public group or channel is a public file, and a photo
  replaced by someone who has left is not deleted until it expires (section 18).
- **Neutral:** a public group's request needs an admin. A popular group waits on its
  admins.

**Residuals.**

- A malicious discovery can serve another record under a public name, as it can for a
  person. Nothing about an existing group is exposed; a joiner compares the handle in
  the group's envelope. Key transparency for handles is not designed here.
- A reject is local to the admin device that pressed it (ADR-0027): another admin
  still sees the request.
- A removed person can ask again in a public group: there is no ban list.
- Claim timing beside a DS claim from one address links a name to a journal (class
  (c)). A private row holds no id, so what links is timing alone.
- The operator holds the OPRF key and can open a public record for any name it can
  guess, and test private names by trying them against `handles`. That reveals only
  which guessed names are held.
- Admins can read knock rows and subscribers' tickets, as those who run a group are
  expected to.
- A read pass links the files one device reads within an hour (ADR-0065), so it links the
  channels those files belong to for whoever reads the service live (class (b)).
- "Admin" in a channel is enforced by the client and the service, not by every peer
  (section 15).
- A file of a deleted post uploaded by another admin stays until its idle life ends.
- Members fetching one photo blob can be clustered by address; a keep source links the
  photos one person administers under one pass (section 18.7).

## Invariants

- **#1:** no personal data on the server. A public name and description are what the
  owner publishes.
- **#2:** the record, knock rows and seed envelopes are ciphertext; a public record is
  public by the owner's choice, the same class as a person in Everyone and an ADR-0059
  listing.
- **#2, the photo:** the blob is ciphertext and its key travels inside an MLS message, a
  sealed post or a sealed preview; in a public record the key is public by the owner's
  choice, as the name is (section 18.3).
- **#3:** touched by step 3 (`retain_join_request`, removing `process_knock` and
  `create_external_join_proposal`) and step 5 (`Group::delete`). Both need property
  tests; G-39 grows. A solo group needs a test and no change.
- **#4:** untouched: a failed claim creates nothing, an invalid ticket admits nobody.
- **#5:** improved at two points: `channel_names` loses its clear id, and knock rows
  stop naming the joiner. No table pairs an account with a group or channel.

#5 also holds for section 13a: no key spans two posts, and a write names no account.
To be indexed when built: a handle row names no account; a private row answers no
lookup; a seed is sealed to a private channel's subscriber only with a valid ticket;
the end of a membership is applied only when no leaf of the account remains.
`lakko-security` before merge: the statements, the sealed knock, the ticket and the
wipe.

## Plan

1. **Core:** the namespace rule on every name, handle statements, certificate, the
   record payload, the conversation handle envelope, channel metadata v3, subscriber
   invite preview kind, invite proof and ticket, the `suggest_handle` and info rules; fuzz
   targets `handle_statement_decode`,
   `handle_record_decode`, `conversation_handle_decode`, `sealed_knock_open`,
   `channel_invite_proof_parse`, `admission_ticket_open`.
2. **Discovery:** `name_claims`, `handles`, the generalised record and term tables,
   the routes, the lease pruner, the retired hold; `channel_names` and `blind_channel`
   removed.
3. **`lakko-mls`:** `retain_join_request`, approve from a retained package on both the
   ordinary and the publisher path; removal of the external proposal path. Property
   tests: a request survives any number of commits; a commit in a knock row does
   nothing; the cap holds.
4. **DS:** the journal knock cap, `DELETE knocks`.
5. **`lakko-mls` and `lakko-sync`:** `Group::delete`, the `conversation_ended` register.
6. **Channels:** public seed routes, `private`, invite keys and proof, sealed tickets,
   the paged list, the subscriber counter, subscriber removal by a publisher, a public
   listing as the flag.
7. **Posts and storage:** `classify_post_plaintext` over the chat envelope, `channel_invite`
   in the chat envelope, `idle_days` on a blob.
7a. **Comments and reactions (section 13a):** post-key derivation, labels, the comment seal,
   the ban list, the own-reactions register in `lakko-sync`, two token contexts, fuzz targets `channel_comment_open` and
   `channel_ban_list_decode`; the channels routes for rows and batched counts.
7b. **The photo (section 18):** `lakko-contacts::avatar` (the ADR-0084 codec moved, no
   profile wire change), the `group_photo` envelope and its dispatch arm, the avatar
   field in channel metadata v3, in the handle record payload (`0x42`) and in link
   preview kinds 1 to 3; fuzz `group_photo_decode` and the existing record, preview and
   metadata targets extended; both FFI surfaces and the NSE decode arm; discovery
   migrations widening `public_records.record` to 7,582 bytes and
   `group_invites.sealed_capability` to 1,995. Clients: the avatar component's
   conversation lookup, the per-group photo store, the owed send, the
   `conversation-photos` keep source, the delete-on-replace record, the re-key on going
   private, one size-bounded avatar cache per client (least recently drawn evicted
   first, replacing the unbounded ADR-0084 cache), and UX section 4.5.
8. **FFI, then web, iOS and Android:** create forms, info, search rows, cards, requests,
   the backlog, invitations, the wipe, the solo group.
9. **Channels as conversations, on all three:** the shared view and composer, the info
   blocks, the app-wide rotation and admin loops; `ManagePanel`, `RotationPanel`,
   `PublishComposer`, `PostCard` and `ChannelsLayout` removed.
10. **Private channels on the key tree**, before release.
11. `lakko-security`, docs reconcile.

Steps 1 to 5 can land without channels; steps 6 and 7 stand alone behind the flag day.

## Open questions

1. The lease and hold lengths, chosen here.
2. Whether a public group should offer "let anyone join", with an admin device
   approving automatically. Not designed: it needs an admin online and does not change
   the bounds.
3. A ban list for public groups.
4. A shareable address for a public name. The web client keeps the address bar at the
   origin, and a name in a path reaches a server. Not designed.
5. Ownership transfer for channels has a server route and no flow.
6. Decided 2026-11-01 (maintainer): reactions and comments, section 13a. Whether the 64
   emoji should grow, and whether a contact's comment should be recognisable, stay open.
7. A role table in the PublisherGroup, so that peers enforce who is an admin (section 15).
8. Decided 2026-10-31 (maintainer): a public group counts as any group for "Members of
   my groups" (ADR-0060). The person's own setting governs, and it reaches every member
   of a public group they join. The proposal to start it off in a public group was
   declined.
9. Decided 2026-11-02 (maintainer): the photo, the five choices of section 18.8.

## References

- ADR-0010, ADR-0027, ADR-0028, ADR-0031, ADR-0045, ADR-0050, ADR-0051, ADR-0053,
  ADR-0054, ADR-0057, ADR-0058, ADR-0059, ADR-0060, ADR-0064, ADR-0076, ADR-0078,
  ADR-0079, ADR-0086
- `services/discovery/src/registry.rs`, `services/discovery/src/channel_names.rs`,
  `services/ds/src/access.rs`, `services/channels/src/api.rs`,
  `crates/lakko-mls/src/group.rs`, `apps/web/src/chat/client.ts`,
  `apps/web/src/channels/subscriber-admission.ts`, `crates/lakko-channels/src/post.rs`,
  `crates/lakko-channels/src/meta.rs`, `apps/web/src/search/people-search.ts`
- RFC 9420 section 12.1.8 (external proposals)
