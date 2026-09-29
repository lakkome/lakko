<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0058: The feed is ranked on the device, by one shared rule engine

- **Status:** Accepted (maintainer, 2026-09-28). S1 (the `lakko-feed` crate) and S2 (both FFI surfaces) built the same day; this text was corrected to what they built
- **Date:** 2026-09-28
- **Context:** product completeness, new destination. UX spec:
  [`docs/ux/feed.md`](../ux/feed.md). Recommendations (Discover) are a
  separate decision, [ADR-0059](0059-channel-discover.md);
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
and `lakko-contacts`, for the display-name rule (a feed's name) and the
canonical username rule (mentions), so neither is spelled twice.
`#![forbid(unsafe_code)]`.

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
| `for_me` | bool | only mentions, replies to the person, and the person's own messages that drew a reaction (the "For me" preset) |
| `min_reactions` | u16 | |
| `hide_words`, `only_words` | strings | each <= 32 entries, <= 64 bytes, trimmed, lowercase, no control characters, no duplicates |
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

**Conversations and candidates.** The client passes its conversations
once, as `FeedConversation { id, kind (Group | Channel), muted, audience }`
(the id is compared with the rule's include / exclude lists; `audience` is
the member count), and one `FeedCandidate` per message in the window, with
only what ranking needs: `conversation` (index into that list), `message_id`,
`reply_to` (message id in the same conversation, optional), `time_ms`,
`from_self`, `from_contact`, `mentions_me`, `replies_to_me`,
`reactions_distinct` (people, not emoji), `has_media`, and `text` (for the
word filters; never logged).

**Order.** `rank(rule, conversations, candidates, now_ms) ->
Vec<FeedEntry { index, replies: Vec<u32> }>`:

1. filter: kind, include/exclude, muted, window, media_only, hide_own,
   for_me, min_reactions, words (case-insensitive, whole words: no letter,
   digit or `_` directly before or after); a candidate whose conversation
   index is out of range is dropped, and of two with the same conversation
   and message id only the first is kept;
2. fold: a reply joins the top of its reply chain (followed at most 8
   steps; a cycle leaves each message standing alone), replies oldest
   first; a reply whose parent is absent or filtered out stays an item of
   its own;
3. score each item: `recency * decay(age, half_life) + reactions *
   e(reactions_distinct, audience) + replies * e(reply_count, audience) +
   from_contacts * [from_contact] + mentions_me * [mentions_me or
   replies_to_me] + has_media * [has_media]`, each component in fixed point
   `0..=65536`: `decay = 2^(-age / half_life)` from a 16-step table with
   linear interpolation, and `e(n, a) = n / (n + 1 + isqrt(a))` for a group
   (so the same count weighs more in a smaller group) and 0 for a channel.
   **Integer arithmetic only**: floating-point `ln` / `exp` go through each
   platform's maths library, and wasm, iOS and Android need not agree on
   the last bit, which would reorder near-equal items differently on
   different phones. These shapes are **chosen, not measured**; they are
   constants in the crate so a change is one reviewed diff;
4. sort by score descending, then time descending, then conversation,
   then message id: total and deterministic.

`rank` is total over arbitrary input and never panics, and its order does
not depend on the order the client passes candidates in (both property
tests).

**Read state.** `contiguous_seen(ids_ascending, seen) -> Option<i64>`:
the highest id such that every id at or below it in that conversation is
seen. One definition, used by the three clients for the read receipt, the
DeviceGroup `ReadCursor` and the unread count.

Both FFI surfaces expose `FeedRule` (construct, encode, decode, export),
the presets, `FeedConversation`, `FeedCandidate`, `rank`,
`mentions_username` and `contiguous_seen`, following the
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
  property tests, fuzz target. **Built 2026-09-28**: 31 tests (28 in the
  crate, among them order-independence and no-duplicate-index property
  tests and an unseen-never-claimed property test for `contiguous_seen`,
  plus 3 property tests driving the fuzz harness's assertions over random
  bytes and damaged valid encodings); fuzz target `feed_rule_decode` builds
  but was not run (cargo-fuzz is not installed where it was built).
- **S2** both FFI surfaces. **Built 2026-09-28**: `feed.rs` on UniFFI
  (records + `encodeFeedRule` / `decodeFeedRule` / `validateFeedRule` /
  `exportFeedRule` / `feedPresetRule` / `feedPresets` / `rankFeed` /
  `mentionsUsername` / `contiguousSeen` / `normalizeFeedWord` and the
  bounds; the kind enum is `FeedConversationKind` because `ConversationKind`
  is taken by `lakko-ui-rules`; a refusal is `FfiError::FeedRule`, appended,
  message `"<code>: <reason>"`) and on wasm (a `FeedRule` class with getters
  and setters, conversation ids as arrays of `Uint8Array`, a refusal throws
  an `Error` with an own `feedRuleError` code). Not on the extension
  surface. Verified: clippy clean on both crates, `cargo test` 273
  (UniFFI) and 179 (wasm) passed, the wasm crate builds for `wasm32`, the
  Swift and Kotlin bindings regenerate with the new functions.
- **S3** web: post log, channel polling, seen set and the new read state,
  the Feed destination, editor, share and import; measure a feed build.
  **Built so far (2026-09-29):** the Feed destination (preset switcher,
  items with reactions and folded replies, open in chat, the "show new"
  pill, empty and first-run states); the channel-post log
  (`apps/web/src/feed/channel-post-log.ts` + `-store.ts`, IndexedDB
  `lakko-channel-post-log`, sealed under the device root key, wiped with
  the channel log on `forgetDevice`) with a cap of **500 posts per
  channel** (chosen) beside the 30-day retention; and the foreground
  channel sync (`ChannelPostSync.tsx`, once a minute while the tab is
  visible, on hydration and on becoming visible; a post whose epoch is
  newer than every held seed stops the cursor until the seed arrives, an
  older one without a seed is passed over; a metadata record read on the
  way is folded into the channel's name). Verified: `tsc` adds no error,
  `vitest` feed suites 20 passed, `vite build` succeeds. Not run in a
  browser: the preview host cannot read the external volume the worktree
  lives on. Seen and read state, the same day: the message log's group
  slot gains `feedSeen` (the ids above `lastViewedMessageId` the feed
  showed; the chat view's own "everything it holds is seen" is unchanged
  and the view's write keeps the ids it does not cover); a group message
  fully on screen in the feed for 2 s is filed through the serialized
  `updateMessageLog`, `contiguousSeen` moves `lastViewedMessageId` when the
  set is contiguous, and only then does the feed send the group a read
  receipt (on the no-wake control path) and the other devices a
  `ReadCursor` at that watermark; unread counts skip `feedSeen` ids
  (`feed/feed-seen.ts`, `feed-seen-writer.ts`; 9 tests). Built
  differently from section 5: the seen set is the watermark plus loose
  ids rather than ranges, since the ids fold into the watermark as soon as
  they are contiguous. Not built: a seen set for channel posts (no receipt
  depends on it; it would feed an unread count channels do not show).
  Own feeds, share and import (S3d), the same day: `feed/feed-draft.ts`
  moves every rule field between the core's `FeedRule` and plain data and
  turns the core's refusal codes into sentences (no bound of its own);
  `feed/FeedEditor.tsx` is the UX spec 4.1 editor (sources with
  per-conversation Only / Never, the six weights, fade, look-back, filters,
  word lists through `normalizeFeedWord`, suggestions) with a live preview
  ranked by the core and Save disabled on the core's `validate`; saved
  feeds sit in the switcher after the presets with Edit and Share, stored
  per account in IndexedDB `lakko-feed-state` under the device root key
  (at most 20, chosen; wiped on `forgetDevice`); Share gives the stripped
  export as a link `<origin>/feed/import#<base64url>` and a `.lakkofeed`
  file; `/feed/import` opens either, shows the rule summary and saves only
  on confirmation, refusing anything the core refuses whole (`feed-draft`
  and store codec: 10 tests). Not built: unpinning a preset, feed icons,
  the "New" divider, the item menu, native deep links for the import
  link. Measured (S3e), the same day, on synthetic logs in Node v25.8.1
  on an Apple M3 Pro with the release wasm, not on a phone: at 10k
  candidates the build takes 8.8 ms and ranking 5.6-13.6 ms; at 100k,
  73.5 ms and 72.7-184.7 ms; at 300k, 226.4 ms and 287.4-708.4 ms. Both
  grow linearly. Latest is the slowest preset because it returns the most
  entries: at 100k, 46.6 ms of its time is marshalling across the wasm
  boundary and 88.2 ms is `rankFeed`. The native engine (release) ranks
  100k in 13.9-34.7 ms. A dev build of the wasm was about 4x slower, so
  numbers from `vite dev` do not describe the product.
- **S4** iOS and Android, the same. Built the same day on both, mirroring
  the web states, flows and copy: Feed as the second of five tabs, the
  channel-post log and its foreground sync (60 s and on resume, never in
  the background), seen marks filing into the message log with the read
  receipt and `ReadCursor` sent only when the watermark advances, own
  feeds with the editor, share as a link and a `.lakkofeed` file, and
  import from `/feed/import#...` or an in-app file picker. Both stores are
  sealed under a device-bound key (Android Keystore, iOS Keychain wrap
  key) and wiped on `forgetDevice`. Verified only in scratch copies,
  because the worktree does not compile on either platform for an
  unrelated reason (the sealed-invite-link bindings of `39dd75bd` are not
  ported to the native clients): iOS `xcodebuild` succeeded and
  `FeedTests` 20 passed; Android `:app:compileDebugKotlin` succeeded and
  16 feed unit tests passed, with the core behind a fake since the host
  JVM has no `.so`. Not run on a device or simulator UI. Not built:
  opening a `.lakkofeed` from another app (no file-type registration),
  the items listed as not built for S3d, and a phone measurement.
- **S5** docs reconcile; ADR-0059 lands the Discover preset.

## Open questions

1. ~~The per-channel cap of the post log (section 3).~~ 500 posts per
   channel, chosen at S3; not measured.
2. ~~Whether the feed build needs an index rather than a scan of the
   log.~~ Not at the sizes a device holds today: the scan is linear, and at 100k
   candidates on a laptop the build plus ranking takes about 150-260 ms
   (S3e above), paid when the feed opens or its inputs change. Revisit
   with a phone measurement at S4; if the budget is exceeded there, the
   first lever is returning fewer entries across the boundary, not an
   index.
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
