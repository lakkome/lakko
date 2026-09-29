<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0059: Discover - an opt-in channel directory, topic buckets, search, and channels shared in groups

- **Status:** Accepted 2026-09-29 (the maintainer chose to build it)
- **Date:** 2026-09-28
- **Context:** product completeness, the recommendation half of the Feed.
  UX spec: [`docs/ux/feed.md`](../ux/feed.md) section 6. The on-device feed
  itself is [ADR-0058](0058-on-device-feed.md).

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
  can derive it; a co-publisher cannot. A separate label from ADR-0057's
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
- **Ranking for the person** happens on the device (ADR-0058's engine
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

- The owner withdraws a listing at any time.
- The operator can remove a listing and block a `channel_id` from being
  listed again, through the internal listener only (the push-gw internal
  listener pattern), with a reason code kept in the row. There is no
  public report flow in this ADR (open question 2).

### 8. Out of scope, stated

- Closing G-43 for publish / rotate / deliver: same owner key, separate
  change, needed anyway.
- Post previews before subscribing (the maintainer chose E2EE).
- Reactions to channel posts.
- Private information retrieval for buckets.

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
- **S3** web: List in Discover, Discover preset, cards, picked topics,
  decoys, search, Share my channels.
- **S4** iOS and Android, the same.
- **S5** `lakko-security`, docs reconcile, G-43 follow-up.

## Open questions

1. How to draw decoy buckets so that the real ones cannot be separated
   over many refreshes (for example a stable random set per device per
   month rather than a fresh one per day).
2. A public report flow for listings, and who acts on reports.
3. Whether the bucket score should also use listing age, to keep new
   channels visible.
4. The per-account daily quota for `ChannelSubscribe` and
   `DirectorySearch` tokens.

## References

- [`docs/ux/feed.md`](../ux/feed.md), ADR-0058
- ADR-0010 (channels), ADR-0028 / ADR-0048 / ADR-0051 (envelope pattern,
  display-name rule, channel metadata), ADR-0053 (request proofs),
  ADR-0054 (anonymous tokens), ADR-0057 (per-group owner root)
- `services/channels/src/api.rs`, `services/discovery/src/channel_names.rs`
