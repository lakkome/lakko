<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0058: The feed is ranked on the device, by one shared rule engine

- **Status:** Proposed
- **Date:** 2026-09-28
- **Context:** product completeness, new destination. UX spec:
  [`docs/ux/feed.md`](../ux/feed.md). Recommendations (Discover) are a
  separate decision, [ADR-0059](0059-channel-discover.md) (to be written);
  this ADR covers everything that needs no server change.

## Context

The maintainer asked for a Feed: an algorithmic timeline of what happens
across the person's groups and channels, with an algorithm the person can
choose or write, in the manner of Bluesky custom feeds. The UX spec fixes
the product: five destinations, four presets plus declarative personal
rules, reactions as signals, replies gathered under their parent, no
one-to-one chats, and items seen in the feed counting as read.

Constraints:

1. **Invariants #2 and #5.** The server reads no message and must not
   learn what a person reads or cares about. Bluesky's feed generators are
   services that read the public firehose; Lakko has no public firehose and
   no service may read a group. Every feed is therefore computed on the
   device from messages it already decrypted.
2. **Three clients, one rule.** A feed that orders the same messages
   differently on two phones is the drift class `lakko-ui-rules` was
   created to prevent (its row in CLAUDE.md records three hand-written
   copies of one rule disagreeing on three points).
3. **Rules arrive from other people.** A shared feed is a file or a link
   someone else made, so its parser is attacker-reachable.
4. **No production data.** Formats change without migration.

What exists, verified in the tree on 2026-09-28:

- **Group messages are already on the device.** Each client keeps one
  encrypted per-account message log across all groups: web
  `apps/web/src/chat/message-log-store.ts` (IndexedDB `lakko-message-log`,
  AES-GCM under the non-extractable device root key), iOS
  `apps/ios/Lakko/Chat/MessageLogStore.swift` (AES-256-GCM, Keychain key),
  Android `apps/android/.../chat/MessageLogStore.kt` (AES-256-GCM,
  AndroidKeyStore). Rows carry the DS message id, direction, text,
  attachments, time, the author's signature key (C-68), edited, deleted,
  reactions, reply and forward. The log is **one blob per account,
  re-sealed whole on every write**, and nothing prunes it.
- **Channel posts are not on the device.** The channel log on all three
  holds channels, keys, seeds and ADR-0051 metadata; posts are re-read from
  `GET /v1/channels/:id/posts` whenever a channel is opened (the native
  `ChannelsUiState` files say so; web `channel-log-codec.ts` has no post
  field).
- **Channel posts have no reactions and no replies.** A channel is
  one-way; `lakko-channels` carries no such field.
- **Every group is already polled in the foreground** on all three (web
  `BackgroundReceiveContext`, iOS `ForegroundReceiveScheduler`, Android
  `pollAllGroups`). Channels are not.
- **Read state.** All three send, as the ADR-0024 read receipt, the
  highest received message id of the open chat, and the account's own
  devices share a DeviceGroup `ReadCursor` (`schemas/mls-app/devicegroup_v1.proto`)
  of the same shape. Unread counts are incoming rows above that cursor
  (web adds a local `lastViewedMessageId`; iOS and Android an in-memory
  counter).
- **No mention exists on the wire.** No envelope field carries one.

## Decision

**A feed is a pure function, in a new crate `lakko-feed`, from a
declarative rule and a list of candidate messages the client assembles
from its local logs, to an ordered list of indices; the clients add a
local channel-post log, poll channels in the foreground, and derive read
state from a per-message seen set whose contiguous prefix is the only thing
that leaves the device.**

### 1. `lakko-feed`, the engine

A new crate, not a module of `lakko-ui-rules`: that crate states that
nothing in it is security-relevant and nothing should become so, and this
one owns a parser for files strangers send. Dependencies: `thiserror`
only. `#![forbid(unsafe_code)]`.

**Rule.** `FeedRule`:

| Field | Type | Bound |
|---|---|---|
| `name` | string | display-name rule of ADR-0048, 1-64 scalars |
| `icon` | u8 | index into a fixed icon list, unknown = default |
| `kinds` | {groups, channels} | at least one |
| `include`, `exclude` | conversation ids | each <= 256 ids, <= 64 bytes each; **local only** |
| `include_muted` | bool | |
| `weights` | recency, reactions, replies, from_contacts, mentions_me, has_media | each 0..=100 |
| `half_life` | FewHours / Day / Week | |
| `window_days` | u8 | 1..=30 |
| `media_only`, `hide_own` | bool | |
| `min_reactions` | u16 | |
| `hide_words`, `only_words` | strings | each <= 32 entries, <= 64 bytes, no line breaks |
| `suggestions` | Never / Rarely / Sometimes | read by ADR-0059; carried here |

**Wire.** `DOMAIN "lakko/feed-rule/v1\0" ‖ version(1) ‖ fields`, bounded,
fail-closed on any violation, canonical (`encode(decode(x)) == x`),
<= 4096 bytes so a link fits. `export(rule)` returns the rule with
`include` / `exclude` emptied and a flag set so the importer can say
"Sources: all groups and channels"; the in-device store keeps the full
rule in the same encoding. A shared file (`.lakkofeed`) and a link
(`https://lakko.me/feed#<base64url>`, the fragment never reaches a server)
carry the exported bytes. Fuzz target `feed_rule_decode`, asserting
round-trip stability and that an exported rule never carries a
conversation id.

**Presets.** `Latest`, `Popular`, `ForMe` as constant rules (Discover is
ADR-0059's) so the three clients cannot disagree about what "Popular"
means.

**Candidate.** The client builds one `FeedCandidate` per message in the
window, with only what ranking needs: `kind`, `conversation` (index into
the client's own list, so no id crosses the boundary twice), `message_id`,
`reply_to` (message id in the same conversation, optional), `time_ms`,
`from_self`, `from_contact`, `mentions_me`, `replies_to_me`,
`reactions_distinct` (people, not emoji), `audience` (member count;
0 for a channel), `has_media`, `muted`, and `text` (for the word filters;
never logged).

**Order.** `rank(rule, candidates, now_ms) -> Vec<FeedEntry { index,
replies: Vec<u32> }>`:

1. filter: kind, include/exclude, muted, window, media_only, hide_own,
   words (case-insensitive, on word boundaries), min_reactions;
2. fold: a candidate whose `reply_to` names another candidate joins that
   parent's `replies` in time order; a reply whose parent is absent stays
   an item of its own;
3. score each item: `recency * decay(age, half_life) + reactions *
   e(reactions_distinct, audience) + replies * e(reply_count, audience) +
   from_contacts * [from_contact] + mentions_me * [mentions_me or
   replies_to_me] + has_media * [has_media]`, with `decay` exponential and
   `e(n, a) = ln(1 + n) / ln(2 + a)` for a group (so the same count weighs
   more in a smaller group) and 0 for a channel. These shapes are
   **chosen, not measured**; they are constants in the crate so a change is
   one reviewed diff;
4. sort by score descending, then time descending, then conversation,
   then message id: total and deterministic.

`rank` is total over arbitrary input and never panics (a property test).

**Read state.** `contiguous_seen(ids_ascending, seen) -> Option<i64>`:
the highest id such that every id at or below it in that conversation is
seen. One definition, used by the three clients for the read receipt, the
DeviceGroup `ReadCursor` and the unread count.

Both FFI surfaces expose `FeedRule` (construct, encode, decode, export),
the presets, `FeedCandidate`, `rank` and `contiguous_seen`, following the
`orderConversations` shape. Not on the extension surface: the notification
process ranks nothing.

### 2. Mentions are derived on the device

`mentions_me` is true when the text contains `@<own username>` on a token
boundary (the username rule of `lakko-contacts::username`, so the match is
exact). `replies_to_me` is true when `reply_to` names one of the person's
own messages. A display-name match does not count: names are not unique and
would mark strangers' messages. No envelope change.

### 3. A channel-post log on each client

Each client adds an encrypted per-account channel-post log beside the
channel log, same shape as the message log, same wrap key class: post
counter, epoch, time, text, attachments, publisher. It is filled wherever
posts are decrypted today (opening a channel) and by the foreground poll
(section 4). Bounded per channel by the server's 30-day post retention
(the log never keeps what the server no longer could serve) and by a
per-channel count cap, a chosen value set in the client and recorded in
the spec when built.

### 4. Channels join the foreground poll

The existing foreground receive loop on each client also fetches new posts
of subscribed channels, after the groups, at a lower cadence (a channel
post is not a conversation turn). When ADR-0055's socket exists its
notices replace the timer for both.

### 5. Seen, unread and the read receipt

- The message log and the post log gain a per-conversation **seen set**
  (stored as ranges of message ids).
- An open chat marks every message it has received as seen, which is the
  meaning "highest received" gives it today; this ADR keeps the chat's
  behaviour and only adds the feed as a second source of "seen". The feed marks an item seen after it has been
  fully on screen for 2 seconds (a chosen value).
- The read receipt (ADR-0024) and the DeviceGroup `ReadCursor` carry
  `contiguous_seen` instead of "highest received". Unread = incoming
  messages not in the seen set.
- Channels send no read receipt (there is no channel read receipt today;
  this adds none).

Seeing an item in the feed can therefore move a group's read receipt, which
the maintainer chose (UX spec section 10). It never moves it past a
message the person did not see.

### 6. Per-device feed state

Pinned feeds (full rules, encoded as above), the "last looked" point per
feed, and not-interested items live in an encrypted per-account store like
the others. They do not follow the person to other devices; that is the
same open question as the rest of per-device state.

### 7. What the feed does not do

- no server change, no new endpoint, no new wire envelope;
- no `lakko-mls` change (invariant #3 untouched);
- no ranking in a background or extension process;
- one-to-one chats are never candidates: the client does not pass them,
  and `FeedCandidate.kind` has no value for them.

## Alternatives

1. **Rank on a server** (Bluesky's model). Impossible without the server
   reading groups; rejected by invariants #2 and #5.
2. **Rank in each client.** Three implementations of a scoring function
   with floating-point, tie-breaks and word matching will not agree;
   rejected for the `lakko-ui-rules` reason.
3. **Put it in `lakko-ui-rules`.** Smaller change, but that crate is
   declared free of anything security-relevant, and the rule parser takes
   attacker-supplied bytes. A separate crate keeps that boundary true.
4. **Scripts or wasm modules as feeds.** Closest to "any algorithm", but it
   is foreign code running over decrypted messages with a path to leak
   them; the maintainer chose declarative rules.
5. **JSON rules.** Easier to read in a text editor. Rejected for one
   canonical, bounded encoding shared by file and link; a JSON file would
   still need the same bounds and a canonical form to round-trip.
6. **Mentions on the wire.** An optional field in the chat envelope would
   let a display-name mention count and would mark mentions the sender
   intended. It is a wire change for a signal the username match mostly
   covers; deferred until the local match proves insufficient.
7. **Per-message read marks on the wire** instead of a contiguous
   watermark. Exact, but it changes ADR-0024's envelope and multiplies read
   traffic by the number of items seen; the contiguous watermark keeps the
   wire and never over-claims.
8. **Keep "highest received" as the watermark and let the feed not count
   as reading.** Simplest; the maintainer chose the opposite.

## Consequences

- **Positive:** the feed is built with no server change and no new
  artifact any server holds; the ordering is identical on the three
  clients; shared rules carry no message and no conversation id.
- **Negative:** a feed shows only what this device received. A new device
  or one offline past the 30-day journal and post retention has a short
  feed, and the spec says so in the UI.
- **Negative:** the message log is one blob re-sealed whole on every write
  and never pruned. Reading it for every feed build, and adding a post log
  of the same shape, grows that cost with use. Not measured. The first
  client sprint must measure a feed build over a large log; if it is too
  slow, splitting the log per conversation is the likely remedy, and that
  is a change to all three stores.
- **Negative:** the read receipt semantics change on all three clients
  together, or the clients disagree about who read what.
- **Negative:** foreground channel polling is new traffic to
  `services/channels`, paid by every client with subscriptions.
- **Neutral:** ranking weights are constants anyone can read in the crate;
  a person's personal weights never leave the device except in a file they
  choose to share.

## Invariants

- **#1 / #2:** untouched; no server change.
- **#3:** untouched; no `lakko-mls` change.
- **#4:** not applicable (no transport or handshake).
- **#5:** untouched. A read receipt already told a group how far a member
  read; it now tells it the same with fewer false claims.
- New parser on attacker-supplied bytes: fuzzed; `lakko-security` is not
  required by any invariant but may be asked for the codec.

## Plan

- **S1** `lakko-feed`: rule codec, export, presets, `rank`, `contiguous_seen`,
  property tests, fuzz target.
- **S2** both FFI surfaces.
- **S3** web: post log, channel polling, seen set and the new read state,
  the Feed destination, editor, share and import; measure a feed build.
- **S4** iOS and Android, the same.
- **S5** docs reconcile; ADR-0059 lands the Discover preset.

## Open questions

1. The per-channel cap of the post log (section 3).
2. Whether the feed build needs an index rather than a scan of the log;
   decided by the S3 measurement.
3. Whether display-name mentions matter enough to put mentions on the
   wire (alternative 6).

## References

- [`docs/ux/feed.md`](../ux/feed.md), [`docs/ux/navigation.md`](../ux/navigation.md)
- ADR-0010 (channels), ADR-0023 / ADR-0024 (reactions, read receipts),
  ADR-0048 (profile names, the display-name rule), ADR-0051 (channel
  metadata), ADR-0052 (reply), ADR-0055 (WebSocket delivery, proposed)
- `crates/lakko-ui-rules` (the one-rule-three-clients precedent)
- Bluesky custom feeds (the product precedent; its server-side model is
  what this ADR rejects)
