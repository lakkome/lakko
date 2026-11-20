<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0059: Discover - an opt-in channel directory, topic buckets, search, and channels shared in groups

- **Status:** Accepted 2026-09-29 (the maintainer chose to build it)
- **Date:** 2026-09-28
- **Context:** product completeness, the recommendation half of the Feed.
  UX spec: [`docs/ux/feed.md`](../ux/feed.md) section 6. The on-device feed
  itself is [ADR-0063](0063-on-device-feed.md).

## Context

The maintainer chose three sources of channel suggestions (2026-09-28): a
**topic shortlist** the server builds from public signals, **channels people
in your groups follow** shared inside the group by those who opt in, and an
**explicit search**. Posts of a suggested channel stay end-to-end encrypted:
a suggestion shows name, description, topic, language and a rough
subscriber count, and reading posts needs a subscription. A whole-directory
download was rejected by the maintainer as not scaling. Random topic
buckets are mixed into every shortlist fetch by default.

What exists, verified in the tree on 2026-09-28:

- **Handles.** `services/discovery` stores `(oprf_hash, channel_id)`
  (`migrations/0009_channels.sql`), first come first served, registered
  with one `discovery-register` token and bound to no account
  (`src/channel_names.rs`). The server never sees a handle in clear.
- **`channel_id`** is 16 random bytes the publisher mints on the client;
  the PublisherGroup id is `"lakko/pubgrp/v2/" ‖ channel_id`
  (`crates/lakko-mls/src/publisher_group.rs`).
- **`services/channels` authorises nobody.** `create_channel` takes only
  `{channel_id}`; publishing, rotating (all three routes), delivering
  envelopes, registering a subscriber and listing subscribers take no
  session, signature or owner key (`src/api.rs`, router). `channels` has
  `id, current_epoch, next_cursor, created_at`. Post authenticity is
  checked only by readers, against each post's Ed25519 signature. This was
  not recorded as a gap; it is minted here as **G-43** (see Consequences).
- **The server can count subscribers**: `channel_subscribers(channel_id,
  subscriber_id, registered_at, last_active_at)`, and
  `GET /v1/channels/{id}/subscribers` returns the whole key list to anyone.
  Because registration is unauthenticated, **a count is forgeable** by
  anyone at no cost.
- **Channel metadata** (ADR-0051) is an encrypted record inside an ordinary
  post: `lakko/channel-meta/v1\0 ‖ version ‖ updated_at_ms ‖ name ‖
  description`.
- **Anonymous tokens** (ADR-0054): contexts `1..=5`, wire bytes frozen,
  a new context is an additive byte.
- **ADR-0057** derives a per-group owner root key from the owner's account
  seed (`BLAKE3-derive-key("lakko/ds/journal-root/v1", account_seed ‖
  group_id)`, `crates/lakko-ds-proof/src/statement.rs`); not yet wired into
  clients.

## Decision

**A channel's owner may publish a listing, signed by a per-channel owner
key registered when the channel is created, to a directory in
`services/channels`; the server ranks listings per (topic, language)
bucket from token-gated subscriber counts only and serves fixed-size
buckets anonymously; search is rate-limited by an anonymous token; and
members who opt in share the listed channels they follow inside a group as
an encrypted envelope that the server never sees.**

### 1. The owner key

- `owner_seed = BLAKE3-derive-key("lakko/channels/owner/v1", account_seed ‖
  channel_id)`, an Ed25519 key. Only the account that created the channel
  can derive it; a co-publisher cannot, and reads the subscriber list with a
  key of its own the owner appoints (Plan, co-publisher keys). A separate label from ADR-0057's
  journal root so one key never signs for two services.
- `POST /v1/channels` gains `owner_pubkey`; the server stores it
  (`channels.owner_pubkey`). No migration story: no production data.
- Listing requests (section 3) are signed by this key, with the ADR-0053
  request-proof shape (method, path, body hash, time, nonce) under its own
  domain, verified against the stored key.

The same key is the natural gate for the rest of G-43 (publish, rotate,
deliver). This ADR requires it only for the directory; closing G-43 is a
separate change that reuses it (section 8).

### 2. Channel metadata v2

The ADR-0051 record gains `topic (u8)`, `language (2-3 ASCII lowercase
letters, ISO 639)` and `listed (bool)`, version byte `0x02`, `0x01`
refused (flag day). Subscribers therefore learn, end-to-end encrypted from
the channel itself, whether it is listed; this is what lets a member share
only listed channels without asking the server (section 6).

### 3. The listing

A listing is the public copy of the metadata the owner chooses to publish:

`lakko/channel-listing/v1\0 ‖ version ‖ channel_id(16) ‖ handle ‖ name ‖
description ‖ topic ‖ language ‖ updated_at_ms`, the bounds of ADR-0051
plus the canonical username rule for the handle (3-32, `[a-z0-9_]`).
Codec in `lakko-channels::listing`, bounded, fail-closed, canonical, fuzz
target `channel_listing_decode`.

- `PUT /v1/channels/{id}/listing` (owner-signed): store or replace;
  `DELETE` withdraws. A listing whose `updated_at_ms` is not newer than the
  stored one is refused.
- The server cannot verify that the handle resolves to this channel (it
  sees only OPRF hashes). **The client verifies**: before subscribing from
  a card, it resolves the handle through the normal OPRF lookup and refuses
  if the `channel_id` differs ("This channel is no longer available"). A
  card never shows a handle it has not verified as a link; until then it
  shows the name only.
- Listing a channel is free of tokens: it needs the owner key of an
  existing channel, and one channel has one listing.

### 4. Subscriber counts that cost something

- `POST /v1/channels/{id}/subscribers` spends one anonymous token of a new
  context **`ChannelSubscribe = 6`** (the verifier spends it only after the
  body validates). Tokens are rationed per account and accounts cost the
  signup proof of work (ADR-0054), so a fake count now costs accounts.
- Subscriptions registered without a token (none, after the flag day) do
  not exist; the directory counts `channel_subscribers` rows.
- Counts are published only in bands (`<10`, `10+`, `100+`, `1k+`, `10k+`,
  `100k+`); the exact count stops being public: `GET .../subscribers`
  becomes owner-signed (the publisher needs the keys to reseal; nobody else
  does).

### 5. Buckets, decoys and search

- **Topics**: a fixed list of 24 (a chosen number), defined once in
  `lakko-channels::listing::TOPICS` with stable `u8` ids and English names
  the clients localise; unknown id = "Other". Languages: ISO 639 codes.
- **Bucket**: `GET /v1/directory/bucket/{topic}/{language}` returns the top
  100 listings (chosen) of that bucket, ordered by a score over public
  signals only: banded count and 7-day growth of token-gated registrations.
  No session, no token, no cookie; responses are identical for every
  caller and cacheable for 1 hour, so a cache or CDN in front removes even
  the per-request view. Each bucket is at most 100 listings of at most
  about 1.4 KB (arithmetic on the ADR-0051 bounds, not a measurement).
- **Decoys**: the client fetches its picked buckets padded with random
  buckets to a fixed set of 8 per refresh (chosen), in random order, at
  most once a day; more than 8 picked means more rounds, each padded. The
  server sees 8 buckets from an address and cannot tell which were picked.
  The switch "Mix in random topics" drops the padding.
- **Search**: `GET /v1/directory/search?q=` over listing name, handle and
  description, top 24 results, one token of a new context
  **`DirectorySearch = 7`**. The query is not logged. The UI states it is
  sent.
- **Ranking for the person** happens on the device (ADR-0063's engine
  scores cards against the person's picked topics, language and
  not-interested set). The server's bucket order is only the order of
  public popularity.

### 6. Channels shared in groups

A new service envelope `lakko-contacts::shared_channels`:
`lakko/shared-channels/v1\0 ‖ version ‖ updated_at_ms(8 BE) ‖ count(1) ‖
[channel_id(16) ‖ handle_len ‖ handle ‖ name_len ‖ name]`, at most 32
channels, bounded, fail-closed, canonical, fuzz target
`shared_channels_decode`, dispatched as `IncomingEnvelope::SharedChannels`
and suppressed in the three background decrypt paths like the other
service envelopes.

- Sent by a member whose "Share my channels with this group" switch is on:
  after the switch goes on, when the member subscribes or unsubscribes, and
  on group growth (throttled like the profile republish). An empty list is
  sent when the switch goes off.
- Only channels whose own metadata (section 2) says `listed` are included,
  so a channel reached through a private handle is never shared.
- The receiver stores the newest list per member, keyed on the member's
  **account key** resolved at receipt (C-68), strictly newer stamp wins,
  under the per-group lock (C-62 sub-invariant 5). Any member may send one;
  the list speaks only for its sender.
- A card from this source shows name and handle from the envelope; the
  description is fetched only when the person opens the card (one
  `GET /v1/directory/listing/{channel_id}`, which tells the server that
  someone looked at that channel, as any card view does).

### 7. Moderation

**Superseded as authority by [ADR-0098](0098-reporting-and-moderation.md)
step 5 (2026-11-20).** The operator's shared-secret route
`POST /internal/v1/channels/{id}/moderation` is removed. The channels
service now applies only a moderator statement signed under the pinned
moderation root (`POST /internal/moderation/statement`, delist / block the
`@name` / remove / restore), and the shared secret stays only as transport
authentication between hosts. A removed channel answers
`410 {"code":"channel-removed","log_index":n}` on every route. The text
below is history.

- The owner withdraws a listing at any time.
- The operator can remove a listing and block a `channel_id` from being
  listed again, through the internal listener only (the push-gw internal
  listener pattern), with a reason code kept in the row. There is no
  public report flow in this ADR (open question 2).

### 8. Out of scope, stated

- Closing G-43 for publish / rotate / deliver: same owner key, separate
  change, needed anyway. Built 2026-10-01, see section 9.
- Post previews before subscribing (the maintainer chose E2EE).
- Reactions to channel posts.
- Private information retrieval for buckets.

### 9. Closing G-43 (amendment, 2026-10-01)

Every write a publisher makes now carries the proof section 1 introduced:
`POST .../posts`, `POST .../rotate`, `POST .../rotate/stage`,
`POST .../rotate/commit` and `POST .../subscribers/<key>/envelope` go
through `require_publisher` (`services/channels/src/owner_auth.rs`), the
gate the subscriber list already used: an owner proof, or a co-publisher
proof by a key the owner appointed, over the method, the routed path, the
query and the exact body bytes, inside the 120 s window, with a nonce that
is consumed once. Every refusal is the same `401 owner-proof-required`.

- Co-publishers are accepted on all five, because a co-publisher posts,
  runs the rotation cron and lets subscribers in exactly as the owner does.
  A co-publisher whose key the owner has not appointed yet is refused until
  it is.
- The proof binds the body, so a stage chunk is hashed in full; nothing is
  parsed before the gate passes.
- The client signs with the role it holds for the channel: the owner key if
  it created the channel, its co-publisher key otherwise, so a large stage
  chunk is not uploaded twice to find out.
- Reads stay open: posts and envelopes are sealed, and the epoch is public.
  `POST .../subscribers` is the token-gated subscribe of section 4.
- What this closes: an unauthorised rotation or staged commit, an envelope
  filed by someone who does not hold the seed, and junk posts that every
  reader had to fetch and drop. Post authenticity is unchanged: readers
  still verify each post's signature.
- No migration: the owner key and the co-publisher table already exist
  (migrations `0007`, `0009`), and a channel without an owner key is
  refused (there is no production data).

## Alternatives

1. **Download the whole directory** and rank on the device (no leak at
   all). Rejected by the maintainer: it does not scale.
2. **Server-side personalised recommendations.** The server would learn
   interests per person; rejected by invariant #5.
3. **Private information retrieval** for buckets. Leaks nothing, but the
   computation and bandwidth are out of proportion for a first version.
4. **Sign listings with the owner's account identity key.** No new key,
   but it publicly links the channel to the owner's account, which the
   directory must not do. Rejected.
5. **Sign listings with a publisher post key.** Those keys change with the
   epoch and co-publishers hold their own, so the server would have no
   stable key to check. Rejected.
6. **Let the server check the handle.** It sees only OPRF outputs and
   would have to learn handles in clear to check; the client-side check
   costs one lookup at subscribe time. Rejected.
7. **Share every subscribed channel in groups.** Simpler, but it would
   reveal channels joined through private handles. Rejected by the spec.
8. **Rank buckets on raw counts as they are today.** Anyone can inflate
   them; rejected, hence section 4.

## Consequences

- **Positive:** suggestions exist without the server learning which
  channels a person reads; the shortlist view is coarse and blurred, the
  group-shared view is invisible to the server, search is explicit.
- **Positive:** subscriber counts stop being forgeable for free and stop
  being public in full.
- **Negative:** a new server-held artifact (listings), plaintext by the
  owner's choice. Its content is the owner's, not any reader's.
- **Negative:** an address fetching buckets reveals a set of 8 topics that
  includes the real ones; a person who turns decoys off reveals exactly
  their topics. Over many days, the stable part of the set can be
  separated from the random part unless the random part is chosen with the
  real one in mind; open question 1.
- **Negative:** a search reveals its query, as stated in the UI.
- **Negative:** subscribing now spends a token; a person subscribing to
  many channels in a day can run out of quota.
- **Negative:** flag day for channel metadata v2, the owner key at channel
  creation, the subscriber token, and the owner-signed subscriber list.
- **New gap G-43:** `services/channels` authorises no publisher operation.
  Posts are protected by reader-side signatures, but anyone who knows a
  `channel_id` (public through a handle lookup) can rotate, stage, commit
  and deliver envelopes, which can break subscribers' key delivery. Not
  caused by this ADR; recorded here because Discover makes channel ids
  easier to find.

## Invariants

- **#1:** listings carry no personal data; a handle and a name the owner
  chose to publish.
- **#2:** channel content stays encrypted; the listing is a public copy of
  metadata the owner published deliberately, the same class as a public
  KeyPackage.
- **#4:** untouched.
- **#5:** the server learns no one's subscriptions or interests beyond the
  padded bucket set and explicit searches, both stated in the UI; group
  sharing is end-to-end encrypted. `GET .../subscribers` stops being public,
  which narrows what #5 already allowed.
- **Security pass:** the owner key and its request proofs, the token gate
  on subscribe and the new envelope parsers warrant `lakko-security`
  before merge.

## Plan

- **S1** core: owner key derivation, `lakko-channels::listing` (codec,
  topics), metadata v2, `lakko-contacts::shared_channels`, token contexts
  6 and 7, fuzz targets; both FFI surfaces.
  **S1 built 2026-09-29; notes, including where it differs from the text:**
  - *Owner proof.* A sibling of the ADR-0053 proof in
    `lakko-channels::owner`, not a parameterised `lakko-ds-proof`: domain
    `lakko/channels/owner-proof/v1`, header `x-lakko-channel-owner-proof:
    v1.<unix_ms>.<nonce>.<sig>`, canonical bytes `domain ‖ 0 ‖ method ‖ 0 ‖
    path ‖ 0 ‖ query ‖ 0 ‖ BLAKE3(body) ‖ unix_ms(8 BE) ‖ nonce(16)`, the
    same 120 s window. It carries **no epoch** (an owner key does not
    rotate); the channel is bound through the path. The owner seed takes the
    account identity's 32-byte Ed25519 seed, the input ADR-0057 uses.
    Derivation and canonical bytes are golden-pinned.
  - *Metadata v2 layout.* The three fields are appended after the
    description: `… ‖ description ‖ topic(1) ‖ language_len(1) ‖ language ‖
    listed(1)`. **An unset language is an empty string** (`language_len` 0)
    in the metadata record; a listing requires 2-3 letters. `listed` must be
    `0x00` or `0x01`. Metadata and listing are not coupled: a record may say
    `listed` with no language.
  - *Topics.* The 24 entries include "Other" as id 0 (ids 0-23, dense).
    Any topic byte decodes and is kept verbatim (canonical encoding); an
    unknown id only *reads* as "Other" (`topic_name`).
  - *One name rule.* The ADR-0051 channel name / description rules moved
    from `lakko-channels::meta` to `lakko-contacts::channel_text` (re-exported
    by `meta` under the old names, no input changed), because the
    shared-channels envelope must refuse exactly what the channel's record
    refuses and `lakko-contacts` cannot depend on `lakko-channels`.
    `lakko-channels` now depends on `lakko-contacts` (and `base64`).
  - *Shared channels.* The envelope refuses a `channel_id` listed twice.
    The NSE extension surface returns no fields on decode (like the profile
    arm): which channels a person follows is not the extension's to learn.
  - *Token quotas.* `channel-subscribe` 20 and `directory-search` 30 per
    account per epoch, chosen, not measured (open question 4).

- **S2** `services/channels`: `owner_pubkey`, owner-signed listing and
  subscriber list, token-gated subscribe, directory tables, bucket ranking,
  search, internal moderation routes; DB-gated tests.
  **S2 built 2026-09-29 (service + minimal client call sites); notes,
  including where it differs from the text:**
  - *Owner key on the wire.* `POST /v1/channels` takes
    `{channel_id, owner_pubkey_hex}` (64 hex, must decode as an Ed25519
    point, else `400 bad-owner-pubkey`). A second create with the same key is
    idempotent; another key for an existing id is `409 channel-exists` and
    changes nothing. Migration `0007` adds `channels.owner_pubkey` NOT NULL
    (flag day, no back-fill).
  - *Owner-signed routes* (`PUT` / `DELETE .../listing`, `GET
    .../subscribers`) go through one gate (`src/owner_auth.rs`): parse the
    header, look up the owner key, window + signature over the routed path,
    then consume the nonce (`channel_owner_nonces`, keyed on the nonce,
    pruned after twice the window by the existing prune loop). The nonce is
    recorded only after the signature verified, so a stranger cannot fill the
    table. Every failure, an unknown channel included, is one `401
    owner-proof-required`; a store failure is a 500.
  - *Listing wire.* `PUT` body = the canonical `lakko/channel-listing/v1`
    bytes, `application/octet-stream`. Refused: codec failure `400
    bad-listing`, another channel id `400 listing-channel-mismatch`, not
    strictly newer `409 stale-listing`, blocked `403 listing-blocked`.
    `DELETE` is idempotent (204). A topic id outside the list is filed under
    Other (0) for bucketing; the stored bytes keep it verbatim. Directory
    items are `{channel_id, listing_b64, subscribers}`, the band only;
    clients decode the bytes with the shared codec.
  - *Subscribe* checks the body and the channel's existence before spending
    the `channel-subscribe` token, so a malformed or misdirected request
    costs none. The SQL store now keeps a subscriber's first `registered_at`
    across re-subscribes (the growth signal counts new subscribers).
  - *Score (chosen, not measured):* subscriber band index x 100 + band index
    of the last 7 days' registrations x 60 + a fresh-listing bonus of 150
    decaying linearly to 0 over 14 days from the **first** listing
    (`channels.first_listed_at_ms`, kept across a withdrawal so relisting
    does not renew it). Ties: first-listed time, then channel id
    (`src/directory.rs`). Bucket responses `Cache-Control: public,
    max-age=3600`; a single listing `public, max-age=300` (chosen); search
    `no-store`, 2-64 characters, a case-insensitive substring over name,
    handle and description ranked by the same score. The SQL store reads
    every listing of a bucket with two `COUNT(*)` subqueries each; fine at
    test scale, not measured at directory scale.
  - *Moderation:* `POST /internal/v1/channels/{id}/moderation` with
    `{action: remove|block|unblock, reason}` on `LAKKO_CHANNELS_INTERNAL_BIND`
    (default `127.0.0.1:8093`), secret `LAKKO_CHANNELS_INTERNAL_SECRET`
    (base64, >= 16 bytes, constant-time); unset, the listener does not
    start. A removal keeps a block in force; a block may name a channel not
    created yet (`channel_moderation`, no foreign key).
  - *Tokens without a database.* `lakko-anon-token-server` gained
    `TokenVerifier::in_memory` so the service's in-memory mode (dev, tests)
    spends through the same gate; the SQL mode uses the byte-identical
    spent-set migration `0008`.
  - *nginx* logs neither directory-search location (the query is in the
    query string).
  - **Finding: co-publishers lose the subscriber list.** The list is now
    owner-signed and a co-publisher cannot derive the owner key, so a
    co-publisher's rotation fanout gets `401`. The owner's own rotation and
    the reseal-after-add (owner-side) are unaffected. Decided and built the same
    day: owner-appointed co-publisher keys (below).
  - *Clients (minimal):* web, iOS and Android send `owner_pubkey_hex` at
    creation, sign the subscriber list, and spend a `channel-subscribe`
    token on subscribe; the three token pools know `channel-subscribe` and
    `directory-search`. A reader screen no longer re-registers a channel this
    device already follows on every open (it would spend a token each time);
    the missing-seed re-sync still re-registers.
- **Co-publisher keys (maintainer, 2026-09-29; closes the S2 finding).**
  **Built 2026-09-29: core, service, both FFI surfaces, web; iOS and Android
  mirrored but not compiled clean (both apps already fail on the unported
  sealed invite-link bindings, no new error on Android; see below). Not walked
  live.**
  - *The key.* `copublisher_seed = BLAKE3-derive-key("lakko/channels/copublisher/v1",
    account_seed ‖ channel_id)`, Ed25519, derived from the **co-publisher's**
    account seed (`lakko-channels::copublisher`, golden-pinned). A separate
    label from the owner key, so an account never collides with itself.
  - *Appointment.* `PUT` / `DELETE /v1/channels/{id}/copublishers/{key_hex}`,
    owner-signed (the S1 owner proof over an empty body), idempotent both
    ways; the key must decode as an Ed25519 point (`400
    bad-copublisher-key`); at most `MAX_COPUBLISHERS` = 32 per channel
    (chosen, not measured; `409 too-many-copublishers`), counted under the
    channel row's lock. Migration `0009` (`channel_copublishers`, FK cascade).
  - *The co-publisher proof.* A **separate header**,
    `x-lakko-channel-copublisher-proof: v1.<copublisher_key_b64url>.<unix_ms>.<nonce>.<sig>`,
    rather than a key-id field in the owner header: the owner header's shape
    and meaning stay exactly as S1 pinned them, and naming the signing key
    lets the server look up one appointed key instead of trying up to 32 per
    unauthenticated request. Canonical bytes are the owner proof's with the
    domain `lakko/channels/copublisher-proof/v1`, so neither proof verifies as
    the other even under one key (tested). Same 120 s window, **same nonce
    table**, same uniform `401 owner-proof-required`; the nonce is consumed
    only after the key is found appointed and the signature verified.
  - *Where it is accepted.* Only `GET .../subscribers` (the one call the
    rotation fanout needs that S2 gated): `require_publisher` in
    `src/owner_auth.rs` tries the owner header first, then the co-publisher
    header. The listing routes and the appointment routes stay owner-only.
    Rotate / stage / commit / deliver remain ungated (G-43, unchanged).
  - *How the owner learns the key.* The smallest carrier with end-to-end
    authenticity: a fixed 183-byte **binding**
    `"lakko/channels/copublisher-binding/v1\0" ‖ 0x01 ‖ channel_id ‖
    account_pubkey ‖ copublisher_pubkey ‖ sig`, signed by the joiner's
    **account identity key**, sent as a third payload of the joiner's knock
    (ADR-0053; the DS allows four). The owner accepts it only when
    `account_pubkey` equals the joiner key it approves (the external-join
    proposal's leaf key, which MLS already authenticated) and `channel_id` is
    its own channel, so the DS or another knocker can carry it but cannot
    substitute a key. Deviation from the brief: the carrier is the server's
    knock queue, not an MLS application message, because a PublisherGroup
    has no application-message path (ADR-0057's same finding) and adding one
    would touch `lakko-mls`; authenticity comes from the account signature,
    not from the carrier. The binding is not an MLS message, so `processKnock`
    refuses it after the owner has read it. Fuzz target
    `lakko-channels/copublisher_binding_decode` (binding decode + proof
    header parse).
  - *Owner flow.* The poll reads bindings off the knock queue into an
    in-memory map keyed on (group, joiner key); `approve` appoints the bound
    key after the load-bearing reseal and never fails the approve on it. Web
    also remembers an approve whose binding was not read yet and appoints
    when it arrives; iOS reads knocks with a cursor, so a binding read before
    a restart is not kept there and that joiner is not appointed.
  - *Co-publisher flow.* The subscriber-list read tries the owner proof and,
    on `401`, once more with the co-publisher proof (web `listSubscribers`,
    iOS `ChannelsClient.listSubscribers`, Android
    `listSubscribersAsPublisher` from the rotation cron and the reseal).
  - *When the approver is not the owner.* A PublisherGroup has no role
    table, so a co-publisher can approve another. Its reseal works (it reads
    the list under its own appointed key), but it cannot appoint: the
    appointment is `401` and logged, and the newcomer's rotation fanout is
    refused until the owner appoints it. **Unbuilt:** no client notices a
    co-publisher it did not approve, so that newcomer stays unappointed and the
    owner has no flow to appoint it; a member list on the PublisherGroup FFI
    would let the owner's poll appoint every bound member.
  - *Revocation.* `DELETE` is built on all three clients' API layers; no flow
    calls it, since removing a co-publisher from a PublisherGroup has no
    client flow either.
  - FFI (UniFFI + wasm): `channelCoPublisherSigner` (`verifyingKey`, `sign`),
    `channelCoPublisherProofHeaderName`, `channelMaxCoPublishers`,
    `channelCoPublisherPath`, `composeChannelCoPublisherBinding`,
    `decodeChannelCoPublisherBinding`; UniFFI appends
    `FfiError::ChannelCoPublisherBinding` (message `"<code>: <reason>"`), wasm
    throws an `Error` with `channelCoPublisherBindingError`.
- **S3** web: List in Discover, Discover preset, cards, picked topics,
  decoys, search, Share my channels.
  **S3 built 2026-09-29 on web (`apps/web/src/discover/`); tsc, vitest and
  vite build run, no live browser run. Notes, including where it differs:**
  - *List in Discover* (`ListInDiscover.tsx`, `listing.ts`) sits on the manage
    screen. Whether this account is the owner is asked of the server once:
    an owner-signed read of the subscriber list, which a co-publisher's owner
    proof fails with `401` (the client holds no local owner flag). A
    co-publisher sees the state only. Listing PUTs the listing first (a
    refusal returns the switch to off: "Couldn't list this channel. Try
    again.") and then posts metadata v2 with `listed`, topic and language;
    withdrawing DELETEs and posts `listed = false`; a rename of a listed
    channel re-PUTs the listing. A channel with no `@handle` cannot be listed
    (the listing requires one) and the screen says so; there is no flow to
    claim a handle after creation.
  - *Metadata v2 on web.* The device's copy of a channel's metadata now keeps
    topic, language and `listed` (channel log, optional fields); a rename
    keeps them.
  - *Decoys* (`decoys.ts`): one random order of all 24 topics per month and
    language, stored in the sealed per-account prefs row on this device (so
    "per device" is per account on a device); each round takes the next
    unpicked topics from that order, so the same decoys come back every day.
    Decoy buckets are fetched in the person's language. Decoy answers are
    discarded; only picked buckets are cached. Refresh at most daily, or at
    once when the picked set changes; a refresh whose every bucket failed
    shows "Suggestions aren't available right now." and keeps the cache.
  - *Card ranking* (`cards.ts`) is a small local scorer, not the ADR-0063
    engine: `rankFeed` ranks messages of conversations the device holds and
    a card has none of their fields. Score: picked topic 100, language 50,
    group followers 40 each (up to 5), band index x 10, `100 - server rank`;
    ties by name then id. Weights chosen, not measured. A listing whose
    decoded channel id differs from the item's id is dropped.
  - *Handles* show as plain text, never a link. Before subscribing the handle
    is resolved through the OPRF lookup (one `discovery-lookup` token); a
    different or missing channel is "This channel is no longer available."
    and the card leaves for the session.
  - *Mixing:* a saved feed whose rule says `rarely` / `sometimes` shows one
    card after every 20 / 8 items (chosen), never first or last; the presets
    never mix.
  - *Search:* 2-64 characters, one `directory-search` token, "Searches are
    sent to Lakko's server." under the field; a used-up allowance or a `429`
    reads "Too many searches. Try again in a minute." A used-up
    `channel-subscribe` allowance now reads "You've subscribed to a lot of
    channels today. Try again tomorrow." everywhere.
  - *Share my channels* (`SharedChannelsContext.tsx`): the switch sits in
    group info for any group this device is still in, **outside the shared
    `lakko-ui-rules` info blocks** (adding a block is a Rust change, left for
    S4); Settings -> Security -> Channels I share lists the groups it is on
    in. The list is sent on switch-on, an empty list on switch-off, a fresh
    list whenever the followed listed set differs from what was last sent to
    a group (debounced 3 s), and the last list re-sent verbatim with its
    original stamp on group growth (the profile scheduler). Received lists
    go through the sniff before the chat parser in the foreground and the
    background receive, are filed per sender key under `withRoutingRowLock`,
    strictly newer stamp wins, and the service worker suppresses the
    envelope. A group-shared card fetches the listing when "Show
    description" is pressed.
  - Wiped on `forgetDevice` (`lakko-discover` IndexedDB, both rows).
  - *Unbuilt on web:* the per-card long-press menu (Not interested and Hide
    this topic are plain buttons), localised topic names, per-channel
    avatars.
- **S4** iOS and Android, the same.
  **S4 built on Android 2026-09-29 (`apps/android/.../discover/`,
  `ui/discover/`), mirroring the web S3 states, flows and copy.
  `:app:compileDebugKotlin` and `:app:testDebugUnitTest` (2353 tests, 0
  failed, 2 skipped) ran in a scratch copy with the six unported sealed
  invite-link call sites stubbed; no device or emulator run. Notes:**
  - *Seams.* The pure rules (`Decoys.kt`, `Cards.kt`, `SharedChannels.kt`,
    `DiscoverPrefs.kt`) are plain Kotlin ports of the web modules with the
    same chosen weights and constants; the network and the core codec sit
    behind `DiscoverBackend` so `DiscoverModel` runs on the host JVM. New
    host tests: `DiscoverPureTest`, `SharedChannelsTest`,
    `DiscoverRuntimeTest`, and a metadata v2 case in `ChannelMetaTest`.
  - *Storage.* Discover's prefs and the shared-channels row are two sealed
    files under the feed's Keystore key (`lakko-feed/discover-prefs`,
    `discover-shared`), deleted by forget-device with the other per-account
    stores.
  - *Metadata v2.* `SharedChannelMeta`, `ChannelHandle` and the channel log
    carry topic, language and `listed`; `ChannelMetaPublisher.post` keeps
    what the device holds, so a rename or a post-rotation re-issue no longer
    publishes `listed = false` for a listed channel (before this every
    Android record was topic Other, language unset, unlisted).
  - *Shared channels.* One mutex per account row serialises every
    read-decide-write. A receive is launched from the poll rather than run
    under the group lock, because a send holds the row mutex while the
    control-message publish takes the group lock; the strictly-newer rule
    makes launch order irrelevant. The re-send after a change of the followed
    set is debounced 3 s and skipped while the channel list is not bound to
    the signed-in account, so a list read before loading never withdraws
    what was shared. Growth re-sends reuse the profile republish scheduler.
    The background preview filter suppresses the envelope (kind 8).
  - *Placement.* The switch is in the group members sheet above the exit
    block (groups only, not after the membership ended), outside the
    `lakko-ui-rules` info blocks as on web; Channels I share is a Settings
    -> Security row opening its own screen. List in Discover is a card on
    the manage screen; ownership is probed with an owner-signed subscriber
    read as on web.
  - *Also fixed:* `AnonTokenPoolTest.eachContextDrawsItsOwnBatchSize` had
    failed since S2 (it listed only the five older contexts); a subscribe
    refused for the used-up `channel-subscribe` allowance now shows the UX
    line instead of "Could not subscribe.".
  - *Unbuilt on Android:* the same three items as web (Hide this topic is a
    plain button, English topic names, initials avatars); the Feed's "Go to
    chats" empty-state button became "Discover channels", so `onGoToChats`
    is now unused.
  **S4 built on iOS 2026-09-29 (`apps/ios/Lakko/Discover/`,
  `UI/Feed/DiscoverViews.swift`, `UI/Channels/ListInDiscoverPanel.swift`,
  `UI/Chat/ShareChannelsViews.swift`), mirroring the web S3 states, flows and
  copy. `xcodebuild build` of the Lakko scheme (app + NSE) and `xcodebuild
  test -only-testing:LakkoTests` (2015 tests, 0 failures, 2 skipped) ran on
  an iPhone Air simulator in a scratch copy with the pre-existing sealed
  invite-link call sites stubbed (CallInviteComposer, ContactsRepository,
  ChatRepository `GroupInviteMint`, CoPublisher) and
  `InviteLinkShapeTests.swift` removed; no live run on a device or against
  the test stack. `just build-ios-extension` was re-run so the NSE surface
  carries the S1 shared-channels decode. Notes:**
  - *Seams.* The pure rules (`DiscoverLogic.swift`: prefs codec, decoys,
    cards, languages; `SharedChannels.swift`) are Swift ports of the web
    modules with the same chosen weights and constants; new tests
    `DiscoverTests` (24) plus a shared-channels arm in
    `NseServiceEnvelopeTests`.
  - *Storage.* Prefs and the shared-channels row are two sealed files under
    `Application Support/lakko-discover/` with their own Keychain wrap key,
    deleted by forget-device.
  - *Metadata v2.* `SharedChannelMeta`, `ChannelHandle` and the persisted
    channel log carry topic, language and `listed`; every composing path
    (create, rename, the post-rotation re-issue, list / unlist) passes them
    through, so a rename or re-issue keeps a listed channel listed.
  - *Shared channels.* `SharedChannelsHub`, owned by `ChatRepository`, files a
    member's list under the account key resolved at receipt on both receive
    paths (foreground and the session poll share `consumeEvents`) under its
    own per-group `GroupMlsLock`, never the commit chain; a domain match that
    fails its bounds is dropped, a foreign plaintext falls through to the chat
    parser. Growth re-sends reuse the pin re-send scheduler with the stored
    list verbatim; a change of the followed set is re-sent debounced 3 s. The
    NSE suppresses the envelope (kind 8), a domain match included.
  - *Placement.* The switch is a section of the group members sheet above the
    exit block while the membership lasts; Channels I share is a Settings ->
    Security row opening its own screen; List in Discover is a card on the
    manage screen, ownership probed with an owner-signed subscriber read. A
    verified card opens the existing found-channel reader on the Feed stack.
  - *Unbuilt on iOS:* localised topic names, per-channel avatars (initials);
    Not interested and Hide this topic are buttons plus a context menu. The
    Feed's empty-state button became "Discover channels", so `onGoToChats` is
    unused.
- **S5** `lakko-security`, docs reconcile, G-43 follow-up.

## Open questions

1. ~~How to draw decoy buckets.~~ A stable random set per device per
   month (maintainer, 2026-09-29): the decoys repeat every day like the
   real buckets, so repetition does not separate them; a change of picked
   topics remains visible.
2. A public report flow for listings, and who acts on reports.
3. ~~Whether the bucket score should also use listing age.~~ Yes
   (maintainer, 2026-09-29): a temporary bonus for fresh listings so new
   channels are not buried under large ones; its shape is chosen at S2,
   not measured.
4. The per-account daily quota for `ChannelSubscribe` and
   `DirectorySearch` tokens.

## References

- [`docs/ux/feed.md`](../ux/feed.md), ADR-0063
- ADR-0010 (channels), ADR-0028 / ADR-0048 / ADR-0051 (envelope pattern,
  display-name rule, channel metadata), ADR-0053 (request proofs),
  ADR-0054 (anonymous tokens), ADR-0057 (per-group owner root)
- `services/channels/src/api.rs`, `services/discovery/src/channel_names.rs`

The channels migrations of this ADR were renumbered 0005-0007 to 0007-0009 on 2026-10-01 when main's member-card migrations took 0005 and 0006.
