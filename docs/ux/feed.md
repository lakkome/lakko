<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: Feed

**Status:** design, the input to the Feed ADR and the three client builds.
**Owner surface:** cross-platform (web SolidJS / iOS SwiftUI / Android
Compose): a new top-level destination **Feed**, the feed editor, the
Discover surface, and three small additions elsewhere: a "List in
Discover" section on the channel manage screen, a "Share my channels"
switch in group info, and a Settings row.
**Decisions (maintainer, 2026-09-28):**

- Feed is a **fifth destination**, next to Chats. Chats stays the home.
- It draws from the person's **groups and channels**, plus
  **recommendations**.
- The algorithm is **presets plus the person's own rules**, one
  declarative format shared by the three clients.
- **Reactions are signals** (they move items up and show as counts);
  **replies gather under the message they answer**. Neither is an item of
  its own.
- Recommendations come from three places: a **topic shortlist** the server
  builds from public signals, **channels people in your groups follow**
  (shared inside the group, only by people who choose to), and an
  **explicit search**.
- Posts of a recommended channel **stay end-to-end encrypted**. A
  recommendation shows the channel's name, description and topic; to read
  posts, you subscribe.

## 1. Why this exists, and what it cannot be

A person in many groups and channels has no way to see "what is happening"
across them without opening each one. A feed answers that.

Twitter and Bluesky build feeds on a server that reads every post. Lakko's
server reads none, so **every feed is built on the device** from messages
the device has already received and decrypted. Three consequences shape
everything below,
shaping what the person sees (the Feed itself carries no sentence about it,
maintainer 2026-10-31: it told a person nothing they could act on):

1. **A feed shows what this device has.** A new device, or one that was
   offline for a month, has a short feed. Messages expire from the server
   after 30 days, so there is no "load older from the server" for
   something the device never fetched.
2. **The server cannot rank for you.** "Your own algorithm" is a set of
   rules the device runs, not a service you subscribe to. Rules can be
   shared as a file; they contain no messages.
3. **Recommendations cost a little privacy.** The
   shortlist tells the server which topics you picked; a search tells it
   what you typed; group-shared channels tell it nothing.

## 2. Where it sits

Destinations, in this order, identical on the three clients:

**Chats · Feed · Contacts · Calls · Settings**

Feed is second so it is one tap from the home without displacing it. The
app still opens on Chats. `navigation.md` section 1 is amended in the same
change.

## 3. What an item is

An **item** is one message in a group or one post in a channel. One-to-one
chats are **not** in the feed: a feed is for skimming many voices, and a
private conversation surfacing next to a channel post reads as a leak even
when it is not one. No rule can turn them on (maintainer, 2026-09-28).

An item shows:

- the **source** (group or channel name, its avatar), then the **author**
  (for a group; a channel post shows the channel only);
- the text, attachments as in the chat timeline (media plays inline under
  the same MIME rule as ADR-0047), a forwarded label if forwarded;
- for a group message, a **reaction summary** (the top three emoji with
  counts, as in the chat) and a **reply line**: "3 replies" plus the newest reply's author and first
  line. Expanding shows the replies in order, one level deep (ADR-0052
  replies are one level deep already). A channel post has neither: a
  channel is one-way, subscribers cannot react or reply to it
  (`lakko-channels` carries no such field), so a channel post is ranked on
  recency and on the rules' source weights only. Reactions to channel posts
  are a separate feature, not part of this spec;
- the time, and a quiet **"why"** line when the feed's rules put it there
  for a reason other than recency: "Popular in Book club", "Mentions you",
  "Reply to your message".

A reply is not an item of its own: it joins its parent. If the parent is
not on this device (it expired, or it predates the device), the reply is
shown alone with the quote it carries (ADR-0052 keeps a snapshot of the
quoted text), marked "Reply to an earlier message".

**Layout, identical on the three clients (maintainer, 2026-10-31).** One
item reads like a post, left aligned, never centred:

```
[avatar]  Source name                      14:32
          Author (groups only)
          Forwarded from ...
          Text, as many lines as it has up to 8, wrapped, left aligned
          [photo / video / GIF / voice, in the text column]
          Why line
          3 reactions row
          ----------
          3 replies   Anna: newest reply
```

- The **source avatar** (40 px, the same tone and initials as the
  conversation's row in Chats) is the left column; everything else sits in
  the column beside it, so a person scans a column of names.
- A card is as tall as its content. It never shrinks to fit the list, and
  nothing inside it is cut by the card's edge. Text longer than 8 lines ends
  in an ellipsis; opening the item shows the rest.
- Media is the same bubble as in the chat, inline, playing in place. The
  whole first row and text open the conversation at that message; the media
  and the reply row keep their own actions.
- Wide screens keep the content width of 720 px; the column does not stretch.

Edits show the current text with "(edited)"; a deleted message leaves the
feed. Service envelopes (names, pins, beacons, profiles) never appear.

## 4. Feeds: presets and your own

The top of the Feed destination is a **feed switcher**: a horizontal row
of the person's pinned feeds. The first four are presets and cannot be
deleted, only unpinned:

| Preset | What it does |
|---|---|
| **Latest** | Every group and channel, newest first. No ranking. |
| **Popular** | The last 3 days. Group messages ranked by reactions and replies, weighed against the group's size (3 reactions in a 5-person group count for more than 3 in a 200-person group); channel posts, which have no reactions, interleaved by recency. |
| **For me** | Messages that mention you, reply to you, or react to yours. |
| **Discover** | Recommendations only (section 6). |

**New feed** (at the end of the switcher, and in the switcher's menu)
opens the editor, starting from a preset or from blank.

### 4.1 The editor

One screen, four sections, a live preview underneath that re-ranks as you
change things. Everything is optional; a blank feed is "Latest".

1. **Sources**: all groups / all channels / both (default both); then
   include or exclude specific conversations; include muted conversations
   (off by default).
2. **What counts**: sliders from "ignore" to "a lot" for: how recent,
   reactions, replies, from your contacts, mentions you, has photos or
   video. Plus how fast things fade: "a few hours / a day / a week".
3. **Filters**: only with media; at least N reactions; hide words (one per
   line); only these words; hide my own messages.
4. **Suggestions**: how often a suggested channel appears in this feed:
   never / rarely / sometimes (default never; Discover is always).

Name and icon at the top; Save pins it to the switcher.

**Icons.** A feed's icon is one of twelve, chosen from a grid under the
name and shown on its chip in the switcher before the name. The rule file
carries the index, so a shared feed keeps its icon; an index a client does
not know draws the default. The list, in index order, by meaning (each
client maps it to its own symbol set):

| Index | Meaning | | Index | Meaning |
|---|---|---|---|---|
| 0 | feed (default) | | 6 | music |
| 1 | star | | 7 | camera |
| 2 | heart | | 8 | globe |
| 3 | lightning | | 9 | speech bubble |
| 4 | book | | 10 | bell |
| 5 | code | | 11 | flag |

Presets keep their own fixed icons and are not editable.

**Share feed** writes the rules as a small file (and a `lakko.me/feed/import#…`
link that carries the file in the URL fragment, never the path). Specific
conversations are **stripped** on export: they mean nothing to someone
else and naming them would leak what you are in. The importer sees a
preview of the rules, and "Sources: all groups and channels" if they were
stripped, before saving.

Feeds live on this device. Following the person to their other devices is
the same question as the rest of per-device state and is not in this spec.

## 5. Reading and acting

- **Tap an item**: opens the conversation scrolled to that message (the
  same jump as tapping a reply quote).
- **React** from the item: the same reaction row as the chat, sent into the
  source conversation.
- **Reply**: opens the conversation with the reply strip already set
  (message-actions.md 3.1). The feed has no composer of its own; writing
  happens where the audience is visible.
- **Long-press / right-click** an item opens its menu: **Open**, **Reply**
  (group messages only; a channel post has no reply), **Copy** (the text),
  **Forward** (the same picker as the chat, message-actions.md), **Hide
  from feeds** (see below). Discover cards keep their own buttons and have
  no item menu.
- **Hide from feeds** ("Mute this conversation in feeds") takes that group
  or channel out of every feed on this device: the presets, the person's
  own feeds and the suggestions. It is not a chat mute: notifications,
  unread counts and the chat itself are unchanged. A toast "Hidden from
  feeds." with **Undo** follows. The feed menu gains **Hidden from feeds**,
  a list of these conversations with **Show again** on each, the most
  recently hidden first; empty state "Nothing is hidden from your feeds."
  The list stays on this device and keeps the 1000 most recently hidden
  (hiding one more drops the oldest). Search in Discover still finds a
  hidden channel: the person asked for it by name.
- **New since you last looked**: a thin divider "New" above the first item
  that was not in this feed the last time the person left the Feed screen
  (per feed, on this device). Nothing new, no divider; the first visit has
  none.
- **Show N new**: while the person is scrolled away from the top, new
  items do not shift the list. A pill "Show N new" (N = items that arrived
  since) sits at the top; tapping it scrolls to the top with them in
  place. At the top, new items simply appear. Native clients also offer
  pull to refresh, which runs the same check and lands at the top.
- **Opening a shared feed from another app**: a `.lakkofeed` file opened
  from Files, mail or a messenger (iOS "Open in Lakko", Android "Open
  with", web as an installed app's file handler) lands on the same import
  screen as the link, with the same preview and the same refusal ("This
  file isn't a feed Lakko can open.").

**Reading in the feed counts as reading** (maintainer, 2026-09-28). An
item that was fully on screen for a moment (2 seconds, a chosen value, not
measured) counts as seen: it no longer counts toward the chat's unread
badge, and it can move the read receipt the group sees.

A read receipt is a watermark, not a per-message mark (ADR-0024: "read up
to message N"). A ranked feed shows a subset, out of order, so sending the
highest seen id would tell the group the person read messages that never
reached their screen. The rule is therefore: **the watermark for a
conversation moves to the highest message id such that every message up to
it in that conversation was seen**, in the feed or in the chat. Messages
the feed filtered out are not seen. The unread badge counts the unseen
ones, which may now sit below messages already seen.

**Channel posts.** A post is read once, wherever it is read: a channel
post seen in the feed (the same 2 seconds) is read in the channel too, and
one read in the channel no longer counts as new in the feed. A channel
has no read receipts, so nothing leaves the device. The channel's count of
new posts, shown on its row in the list like a chat's unread badge, is the
number of posts newer than the newest one read; reading the newest post
resets it to zero, even if older ones were never on screen (unlike a
chat's watermark above, which must not run past an unseen message because
others see it). The person's own posts are read. The count stays on this
device.

## 6. Discover

Discover is both a preset feed and the place suggestions come from.
Suggestions appear as **channel cards**, never as posts (the posts are
encrypted until you subscribe):

- avatar, name, `@handle`, description (two lines, expandable), topic,
  language, a subscriber count shown in rough bands ("1k+", "10k+");
- a **reason line**: "Followed by 3 people in Book club", "Popular in
  Technology", "Matches your search";
- **Subscribe** (primary), **Not interested** (ghost). "Not interested"
  hides the channel on this device; its menu also offers "Hide this
  topic".

### 6.1 Topics you pick

The first time the person opens Discover, one card:

> **Pick a few topics.** Lakko downloads suggestions for them, mixed with
> a few random topics.
> [topic chips] [Language] [Save] [Not now]

Topics come from a fixed list (the same list a channel owner picks from).
Along with the picked topics the client downloads a few random ones and
discards them (maintainer, 2026-09-28: on by default); a switch in the
Discover menu, "Mix in random topics", turns it off. No line sits under
the switch (maintainer decision, 2026-10-28).
The language defaults to the app language. The choice is editable from the
Discover menu. With no topics picked, the shortlist is not downloaded at
all and Discover shows only group-shared suggestions and search.

### 6.2 Search

A search field at the top of Discover, with no helper text under it
(maintainer decision, 2026-10-28). Results are channel cards.
Search is Discover's own: the global search of `navigation.md` section 4
(exact names only) is unchanged.

Search results can include channels this device already holds, which the
suggestions leave out. Such a card says so instead of offering to
subscribe: the reason line reads "Your channel" (a channel this device can
post to, as owner or co-publisher) or "You follow this channel", and its
only action is **Open**, which opens the channel; no Subscribe, no Not
interested, no Hide this topic.

### 6.3 Channels people in your groups follow

Group info gains a switch, **Share my channels with this group**, off by
default, with one line under it: "People in this group can see which
channels you follow." When on, the person's
subscribed channels that are **listed in Discover** are shared with that
group; unlisted channels are never shared, since their handle may be a
secret invitation.

A Settings row, **Settings → Security → Channels I share**, lists every
group the switch is on in, so it can be turned off in one place.

A card's reason line counts people, never names them unless they are the
person's contacts: "Followed by Anna and 2 others in Book club". Such a
card shows name and handle only; the description loads when the person
opens the card.

### 6.4 Listing a channel

The channel manage screen gains **List in Discover**, for the channel's
**owner only** (ADR-0059: the listing is signed by a key only the account
that created the channel can derive; co-publishers see the state, not the
switch):

- off by default;
- when turned on: pick a topic and a language, then a dialog: "Anyone can
  find this channel's name, description and subscriber count. Posts stay
  encrypted." [Cancel] [List];
- the name and description listed are the ones set under ADR-0051; editing
  them updates the listing;
- turning it off removes the listing.

## 7. States

| Surface | State | What the person sees |
|---|---|---|
| Feed | no groups or channels at all | Empty state: "Your feed fills up from your groups and channels." + "Discover channels" (primary) |
| Feed | has sources, nothing received yet | "Nothing here yet. New messages from your groups and channels will show up here." |
| Feed | loading (first build after launch) | skeleton items, never a spinner over an empty screen |
| Feed | a feed whose filters match nothing | "No messages match this feed." + "Edit feed" |
| Feed | refresh failed (network): no group and no channel could be reached in one check (a check where some answered is not a failure) | the existing items stay; a banner "Couldn't check for new messages." + Retry; the banner goes at the next check that reaches anything |
| Feed | a source can no longer be read (left the group, unsubscribed, removed) | its items leave the feed; no message |
| Feed | caught up | at the end: "You're up to date." |
| Editor | invalid rule (e.g. empty "only these words" with the filter on) | inline error under the field; Save disabled with the reason shown |
| Import | the file is not a feed, too large, or a newer version | "This file isn't a feed Lakko can open." (no partial import) |
| Discover | no topics picked | the pick-topics card, group-shared suggestions below it |
| Discover | suggestions unavailable (server not configured, or offline) | "Suggestions aren't available right now." Search is disabled with the same line; group-shared suggestions still show |
| Discover | search returned nothing | "No channels found." |
| Discover | a card's channel can no longer be subscribed to (delisted, deleted) | Subscribe fails: "This channel is no longer available." The card is removed |
| Discover | rate-limited | "Too many searches. Try again in a minute." |
| Discover | subscribe allowance used up for today | "You've subscribed to a lot of channels today. Try again tomorrow." |
| Discover | the handle on a card does not lead to that channel | Subscribe refused: "This channel is no longer available." The card is removed |
| Listing | refused by the server | "Couldn't list this channel. Try again." The switch returns to off |

## 8. Cross-platform shape

One mental model. Mechanics may differ, vocabulary and states may not.

| | Web | iOS | Android |
|---|---|---|---|
| destination | nav rail / drawer entry | `TabView` tab | `NavigationBar` item |
| feed switcher | chips row, horizontally scrollable | horizontal chips in a `ScrollView` under the title | `ScrollableTabRow` |
| item menu | right-click / hover | context menu | long-press dropdown |
| refresh | "Show N new" pill | pull to refresh + pill | pull to refresh + pill |
| editor | full-screen panel with preview on the right (wide) or below (narrow) | pushed screen, preview below | pushed screen, preview below |
| share | download file + copy link | share sheet | share sheet |

The ranking itself is **not** written three times: the clients hand the
rule and the candidate items to one core function and draw the order it
returns (the `lakko-ui-rules` pattern). A feed that orders differently on
two phones is the drift class that shared rule exists to prevent.

## 9. What the data has to provide

This section is the input to the ADR. Nothing here exists today unless
said so.

1. **Received messages, on the device.** Group messages already are: each
   client keeps one encrypted per-account message log across all groups
   (web `chat/message-log-store.ts`, iOS `Chat/MessageLogStore.swift`,
   Android `chat/MessageLogStore.kt`) whose rows carry the message id,
   author signature key, time, text, attachments, reactions, reply,
   forward, edited and deleted. **Channel posts are not stored on any
   client**: the channel log holds keys, seeds and metadata, and posts are
   re-read from the server each time. The feed needs a local post log with
   the same shape and the same encryption.
2. **Mentions do not exist on the wire.** No envelope field carries one.
   Either the "mentions you" signal is derived locally (the person's
   `@username` or display name in the text), or the envelope gains an
   optional mentions field. The ADR decides; the UI works with either.
3. **Freshness across every conversation.** All three clients already
   poll every group in the foreground (web `BackgroundReceiveContext`, iOS
   `ForegroundReceiveScheduler`, Android `pollAllGroups`). Channels are
   fetched only when opened; the feed needs them polled too. The WebSocket
   notices of ADR-0055 would carry both.
4. **One shared ranking core.** A versioned, bounded feed-rule format
   (encode / decode, fuzzed, since it arrives from other people's files),
   the four presets as constants, and a total, deterministic ordering
   function over candidate items returning indices. On both FFI surfaces.
5. **A channel directory on the server.** Listing record: handle, name,
   description, topic, language, a banded subscriber count, signed by a
   key the channel's owner holds, so the server cannot invent or alter a
   listing. Routes: publish / withdraw a listing, fetch a topic×language
   bucket (fixed size, server-ranked by public signals only), search. The
   anonymous-token scheme of ADR-0054 is the natural rate limit. This is
   the one new thing the server stores; it concerns channels whose owners
   asked to be listed and no person's reading.
6. **A fixed topic list**, defined once and shared by the directory and the
   three clients.
7. **A group envelope carrying the listed channels a member follows.** The
   profile / group-name envelope shape: bounded, domain-separated,
   suppressed in background previews, replaced by the newer one per member
   (keyed on the member's account key), cleared when the switch goes off.
8. **Per-device state**: the feed rules, pinned feeds, the "last looked"
   point per feed, not-interested channels and topics, picked topics.
9. **A per-message "seen" set per conversation**, fed by both the chat and
   the feed, from which the unread count and the contiguous read watermark
   (section 5) are derived. Today all three clients send the highest
   received message id of the open chat as the watermark (web
   `routes/Chat.tsx` via `shouldSendReadReceipt`, iOS and Android
   `ChatViewModel`), so everything received counts as read the moment the
   chat is open; the same goes for the cross-device read cursor the
   account's own devices share. That still works for a chat (it shows every
   message), but
   once the feed marks some messages seen out of order, both sides must
   derive the watermark and the badge from this set, on all three clients,
   or the feed and the chat disagree about what is read.
10. **Decoy bucket choice**: how many random buckets, drawn how, so that
    the requested set does not single out the real ones (the ADR sizes it).

## 10. Decided (maintainer, 2026-09-28)

1. **Read receipts from the feed**: yes, for items that were on screen,
   under the contiguous-watermark rule of section 5.
2. **One-to-one chats**: never in the feed, no rule can add them.
3. **Random topic downloads**: on by default, a switch turns them off.

## 11. Not in this spec

- Posting to the feed itself; there is no public timeline of the person.
- Feeds following the person across devices.
- Third-party feed generators (the maintainer chose declarative rules).
- Previews of recommended channels' posts (they stay encrypted).
- Moderation of the directory beyond the owner's listing switch; the ADR
  must at least say how an abusive listing is removed.
