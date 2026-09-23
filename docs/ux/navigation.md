<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Navigation: one conversation list, and Calls back

Date: 2026-09-23. Scope: web, iOS, Android. Supersedes the destination list in
[`client-redesign.md`](client-redesign.md) section "Navigation" and updates
section 4 of the design system.

## 0. Why

Two things are wrong with the shipped navigation, and they have opposite
causes.

**Chats, Groups and Channels are three destinations for one activity.** A
person does not think "I will go to the Groups place"; they think of a
conversation and look for it. Splitting by container means the newest thing
that happened to you is in one of three lists and you have to remember which.
Every other messenger merged these years ago, and the reason is not fashion:
the list is sorted by *when*, and three lists cannot be.

**Calls was removed on a premise that no longer holds.**
[`client-redesign.md`](client-redesign.md) line 190 removed the destination
and moved call links into the Contacts "+" menu. The stated ground, recorded
at line 115, was that "Calls has no history, only a create-link panel" - so
the destination held nothing worth a tab. That was true of what existed. It
was never an argument that call history should not exist, and the design
system's own information architecture (`design-system.md` section 4) kept
**Calls: recent calls, "Create call link", join by link** as destination 4
throughout. The two documents disagreed and the code followed the wrong one.

## 1. Destinations

Four, in this order, identical on the three clients:

1. **Chats** - every conversation: one-to-one, groups, channels. The home.
2. **Contacts** - accepted contacts, incoming requests, add by username.
3. **Calls** - recent calls, newest first. Create a call link, join by link.
4. **Settings** - unchanged.

Gone as destinations: **Groups** and **Channels**. Both remain as *kinds* of
conversation inside Chats, and both keep their creation flows, reached from
the create menu in section 3.

The app opens on Chats. There is no home screen.

## 2. The conversation list

### 2.1 Row model

**One row, drawn identically for all three kinds.** A one-to-one chat, a
group and a channel are the same row: same avatar treatment, same title, same
second line, same trailing column. Nothing in the list says which kind a row
is - no badge, no label, no differing avatar shape.

| part | content |
|---|---|
| avatar | the conversation's picture, else its initials |
| title | contact's display name, group name, or channel name; falling back to `@username` / `@handle` and then to the short key |
| second line | the last message or post, prefixed with the sender's name where there is one to name, and `You: ` when it is ours |
| right, top | time of the last activity |
| right, bottom | unread badge, or the muted glyph |

A conversation the person has left, or a channel they unsubscribed from, is
not in the list.

### 2.2 Ordering

1. Pinned, newest first.
2. Everything else, newest first.
3. Archived, newest first, below a divider - or hidden behind an "Archived"
   row when there are more than five, on the narrow layouts.

"Newest" is the last activity the person can see: a message, a post, a call
row, a system row the timeline draws. A row that draws nothing does not move
a conversation to the top.

This is the rule web and Android already apply (`pinned desc`, then
`archived asc`, then activity); it carries unchanged, and what changes is
only the set it sorts - chats, groups and channels together rather than one
kind at a time.

### 2.3 States

- **Default** - the list.
- **Empty** (no conversations at all): the wordmark, one line - "No
  conversations yet" - and a button that opens the **same create menu** as
  section 3. Not a different set of choices in a different place.
- **Empty after search**: "Nothing matches". What else this state offers
  depends on section 4, which is not settled.
- **Loading**: the list is local, so it is drawn from disk immediately and
  there is no spinner. A conversation whose content has not arrived yet shows
  its title and no second line rather than a placeholder shimmer.

## 3. The create menu

One button, top right of the Chats list, an unambiguous "compose" affordance
(not a bare `+`, which on the natives reads as "add contact"). Tapping it
opens a menu of exactly three items, in this order:

1. **New chat** - pick from contacts. A contact with no conversation yet
   starts one; a contact with one opens it.
2. **Create group** - the existing new-group flow.
3. **Create channel** - the existing new-channel flow.

Each item is a row with a leading icon and a label. No descriptions; if an
item needs a sentence to explain it, the label is wrong.

The empty state's button opens this same menu, and so does the keyboard
shortcut on web.

**Create call link** is NOT in this menu. It belongs to Calls (section 5),
which is where a person goes when they are thinking about calling. It was put
in the Contacts "+" menu by the redesign only because Calls had been removed;
with Calls back, it goes home.

## 4. Search - OPEN, needs a decision before anything is built

**Deferred.** The brief asks for search that covers users, groups and
channels together; how that field behaves is not settled and nothing should
be built against this section until it is.

What is NOT open is the constraint any answer has to live with, because it
is a property of the system rather than a design choice.

### 4.1 The network lookup can only ever be exact

Lakko's discovery is a blinded lookup: the client normalises the username,
blinds it, and the server evaluates without learning what was asked
(`lakko-discovery-client`, invariant #5). That mechanism answers exactly one
question - "does this exact name exist" - and it cannot answer "which names
start with these letters", because a prefix search is the server learning
the prefix. The same is true of channel handles (`blind_channel`).

Anything the person already has on this device - their conversations, their
contacts - can be matched however loosely we like, because that runs here.
Anyone they do not yet know can only be reached by typing the whole
username.

### 4.2 Why this is worth settling before it is built

Today the two behaviours are in two different places, and the question never
arises: the chat list's **Search** is a local filter that never touches the
network, and the network lookup is a separate **Add by username** form in
Contacts with an `@alice` prompt. The honesty is an accident of layout, not
a rule anyone wrote.

Merging them into one field is what removes that accident. A single box
asking for "users, groups and channels" invites a client to send whatever
was typed to discovery and show nothing when a partial name comes back
empty - which reads to the person as a broken app rather than as a fact
about how the name has to be typed.

So the open question is not "fuzzy or exact" - that is answered. It is how
one field presents two different kinds of reach without lying about either.

## 5. Calls

### 5.1 The list

Recent calls, newest first, flat - not grouped by conversation. Day
separators (`Today`, `Yesterday`, a date) the same as the chat timeline.

Row:

- avatar and title, as section 2.1, of the conversation the call belongs to;
- second line: the outcome and the time, in the words the conversation
  already uses - `Outgoing call, 12:04`, `Incoming call, 12:04`,
  `Missed call`, `Declined`, with the duration when there was one;
- a direction glyph at the leading edge: out, in, missed;
- trailing: a call button that calls back **in the same mode** the row
  records - a video call re-dials video.

A missed or declined row is tinted with the danger token, as
`CallTimelineRow` already does in the conversation. Tapping the row opens the
conversation; tapping the call button calls. Those are two different targets
and the hit areas must not overlap.

### 5.2 States

- **Default** - the list.
- **Empty**: "No calls yet" and the two actions below.
- The screen carries **Create call link** and **Join by link**, as the design
  system's IA has always said.

### 5.3 A missed call elsewhere

Unchanged from [`calls.md`](calls.md) section 6 and still owed on two
clients: a missed call raises a notification (iOS does, web and Android do
not), and the conversation shows unread with `Missed call` (both natives do,
web does not - its call log is a store the chat list never reads). Those are
defects against the existing spec, not new work from this one, and they are
tracked in `PRODUCT-READINESS.md`.

## 6. Data each client must expose

This is what has to exist before the screens can be built, and it is where
the three clients currently differ most.

### 6.1 The merged list

Each client needs ONE ordered sequence of conversations carrying, per row:
kind, id, title, avatar source, last-activity timestamp, preview line,
unread count, pinned, muted, archived.

- **Web** has chats and groups in one store already, and channels in a
  separate one (`channels/`). They must be merged into one sorted sequence.
- **iOS / Android** have the same split. The chat list is built from
  `ChatThread`s; channels are their own screen and their own model.

None of the three has a channel's "last post time" in the same shape as a
chat's "last message time". That mapping is the first thing to build.

### 6.2 The calls list

A **flat, cross-conversation, time-ordered** sequence, which none of the
three can produce today:

- **Web** keeps a per-group call log (`calls/call-log-store.ts`, encrypted,
  capped at 200 per group). A global list is a flatten-and-sort over every
  group's list - correct, and cheap at this size.
- **iOS / Android** keep call rows as *system rows inside each
  conversation's message log* (`CallTranscriptRow`, `CallTranscriptEntry`).
  Producing a global list means scanning every conversation's whole
  transcript on every open, which is the wrong cost and gets worse with use.
  **The natives need a call index** - the same shape the web store already
  has - written at the moment a call row is written. That is the one piece of
  new persistence this redesign requires.

Either way the row needs: conversation id, kind, outcome, whether it was a
video call, start time, duration. The natives' current row carries the human
label rather than the parts (`callRowText` returns "Missed call"), so the
index must carry the parts and let the label be derived - a list cannot sort
or filter on a sentence.

### 6.3 Search

Nothing to specify until section 4 is settled. Worth recording now: matching
over titles and contact names is local and needs no new data, the exact
lookup reuses the existing discovery client unchanged, and searching message
TEXT would need an index no client has.

## 7. Not in this spec

- **Search**, in full - see section 4. The constraint is recorded; the
  behaviour is not decided.
- **Message search** in particular. It needs a local index no client has.
- **Folders / custom filters.** The merged list is the point; a filter
  mechanism on top of it is a separate decision.
- **Call history sync across devices.** The calls list is this device's, like
  the message log. Making it follow the person is the same question as
  cross-device message history and belongs with it.

## 7.1 What is built, on web

- **Calls** is a destination again and lists real history, day by day.
- **Chats and groups are one list.** The pane took a `kind` prop and was
  rendered twice; the prop is gone, a row routes by its own kind, and the
  rail's Groups entry is folded into Chats.
- **The create menu** is the three items, opened by the compose button and by
  the empty state alike.
- **Channels are in the list**, ordered with everything else by when
  something last happened in them. Their row is shorter by necessity rather
  than by design: a channel is not the MLS-group shape the row is written
  against, so it carries no unread badge and no pin / mute / archive, which
  are keyed on a conversation group id it does not have. A badge that can
  never be non-zero would be a worse lie than none.
- **The Channels destination stays anyway**, and this is the one place the
  merge is blocked on something else: that screen holds the only way to find
  a channel by `@handle`, which is a network lookup and therefore part of
  the open question in section 4. Removing the destination first would
  delete the entry point without replacing it.
- **Search** is untouched and still the local filter it was: section 4 is
  open.
- **iOS and Android have none of this yet.**

## 8. Cross-platform shape

| | web | iOS | Android |
|---|---|---|---|
| destinations | left rail at >= 720 px, bottom bar below | bottom `TabView` | bottom `NavigationBar` |
| create menu | popover under the compose button | `Menu` on the toolbar button | `DropdownMenu` on the top-app-bar action |
| search | *deferred, section 4* | *deferred, section 4* | *deferred, section 4* |
| row press | click opens; right-click the row menu | tap opens; long-press the row menu | tap opens; long-press the row menu |

The three implement the same model: same four destinations in the same
order, one row shape that says nothing about the kind of conversation, the
same ordering rule and the same three create items.
