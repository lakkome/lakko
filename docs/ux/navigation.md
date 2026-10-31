<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Navigation: one conversation list, and Calls back

Date: 2026-09-23. Scope: web, iOS, Android. Supersedes the destination list in
[`client-redesign.md`](client-redesign.md) section "Navigation" and updates
section 4 of the design system.

**How to read this.** Everything below is a requirement for all three
clients. Where a platform genuinely has to differ, the difference is stated
inline, at the requirement it belongs to, marked **Per platform**. There is
no mapping table at the end and no per-client section: a spec whose body
describes one client and whose tail translates it makes that client's
accidents into everyone's contract.

What is BUILT, on which client, is not here - that is
[`PRODUCT-READINESS.md`](../PRODUCT-READINESS.md), whose job is to be honest
about the product rather than about the intent.

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

Five, in this order, identical on the three clients:

1. **Chats** - every conversation: one-to-one, groups, channels. The home.
2. **Feed** - messages from groups and channel posts, ranked by rules the
   person picks or writes, plus channel suggestions. Specified in
   [`feed.md`](feed.md) (added 2026-09-28; this list read "Four" before).
3. **Contacts** - accepted contacts, incoming requests, add by username.
4. **Calls** - recent calls, newest first. Create a call link, join by link.
5. **Settings** - unchanged.

Gone as destinations: **Groups** and **Channels**. Both remain as *kinds* of
conversation inside Chats, and both keep their creation flows, reached from
the create menu in section 3.

The app opens on Chats. There is no home screen.

**Web: the address bar shows the origin only** (`https://lakko.me/`, on the
test stack `https://test.lakko.me/`), whatever screen is open. A chat or group
id, an invite secret or a notification parameter never appears in the address
bar, the browser's history list, a bookmark or a screenshot. The screen the
user is on lives in the browser's session history instead, so back and
forward move between screens and a reload keeps the screen. A link opened
from outside (`/i/`, `/c/`, `/g/`, a notification) is read once on entry and
cleared from the address bar before anything renders
(`apps/web/src/routes/origin-history.ts`).

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
not in the list. **Amended 2026-10-31
([`group-and-channel-privacy.md`](group-and-channel-privacy.md) section 9):** the same
holds when the person was removed from a group, and the history goes with the row, on
every device of the person.

Search can also show a public group or channel the person is not in; that is a search
result, not a row of this list. While the person has a request waiting, a `Requests
sent` row stands above the list (same spec, section 6.2).

**Per platform.** Opening a row is a click or a tap; the row's own menu is a
right-click on the web and a long press on both natives. That is the whole
difference - the row itself is the same row.

### 2.2 Ordering

1. Pinned, newest first.
2. Everything else, newest first.
3. Archived, newest first, below a divider - or hidden behind an "Archived"
   row when there are more than five, on the narrow layouts.

"Newest" is the last activity the person can see: a message, a post, a call
row, a system row the timeline draws. A row that draws nothing does not move
a conversation to the top.

A group's creation counts as activity: its timeline draws a row, so a new group of
one sorts to the top like any other new conversation (2026-10-31).

This is the rule web and Android already apply (`pinned desc`, then
`archived asc`, then activity); it carries unchanged, and what changes is
only the set it sorts - chats, groups and channels together rather than one
kind at a time.

### 2.3 States

- **Default** - the list.
- **Empty** (no conversations at all): the wordmark, one line - "No
  conversations yet" - and a button that opens the **same create menu** as
  section 3. Not a different set of choices in a different place.
- **Empty after search**: "No results", plus the single "Look up @name" row
  of section 4.3 when the text could be a name.
- **Loading**: the list is local, so it is drawn from disk immediately and
  there is no spinner. A conversation whose content has not arrived yet shows
  its title and no second line rather than a placeholder shimmer.

## 3. The create menu

One button, top right of the Chats list, an unambiguous "compose" affordance
(not a bare `+`, which reads as "add contact"). Tapping it opens a menu of
exactly three items, in this order:

1. **New chat** - pick from contacts. A contact with no conversation yet
   starts one; a contact with one opens it.
2. **Create group** - the new-group flow. Nobody has to be picked (2026-10-31).
3. **Create channel** - the new-channel flow. Both ask for an `@name` and Public or
   Private ([`group-and-channel-privacy.md`](group-and-channel-privacy.md) section 3).

Each item is a row with a leading icon and a label. No descriptions; if an
item needs a sentence to explain it, the label is wrong.

**Per platform.** The menu is whatever that platform's people already know a
menu to be - a popover under the button on the web, a `Menu` on the toolbar
button on iOS, a `DropdownMenu` on the top-app-bar action on Android. Same
three items, same order, same labels.

The empty state's button opens this same menu, and so does the keyboard
shortcut on web.

**Create call link** is NOT in this menu. It belongs to Calls (section 5),
which is where a person goes when they are thinking about calling. It was put
in the Contacts "+" menu by the redesign only because Calls had been removed;
with Calls back, it goes home.

## 4. Search

The field at the top of the Chats list, because a group and a channel are
kinds of conversation rather than kinds of contact, and because finding a
person to talk to starts a conversation too. Contacts is about people you
already know.

**Per platform.** Pinned above the list on the web, `.searchable` on iOS, a
search bar above the list on Android - the same field where each platform
puts one.

**Also with an empty list (2026-09-30).** The field is there before the
first conversation exists: finding the first person to talk to is exactly
what a new account needs it for. With nothing typed the empty state shows as
before (its sentence and its New button); once something is typed the empty
state gives way to what the search finds, the people half included
([`who-can-find-me.md`](who-can-find-me.md) section 6.1). The folder bar
stays hidden with an empty list.

The LOCAL half is settled and built: fuzzy over the titles and previews of
what this device already holds. **How the whole field should present itself
once it covers messages as well is still open** - that part is deferred.

What is NOT open is the constraint any answer has to live with, because it
is a property of the system rather than a design choice.

### 4.1 The network lookup can only ever be exact

**Amended 2026-10-31
([ADR-0087](../adr/0087-group-and-channel-handles-and-privacy.md)):** this section
describes the username and handle lookup. Since ADR-0060 a name is also found by the
start of a word, from 3 letters, for those who published their terms, and ADR-0087
extends that to public groups and channels. An `@name` is still exact. People, groups and
channels now share one set of names, so section 4.3's "two namespaces" is one, and search
results are specified in [`group-and-channel-privacy.md`](group-and-channel-privacy.md)
section 5.

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

### 4.3 There is ONE network lookup, not one per kind

**Amended 2026-10-25:** there is no row to press and no hand-over to a
Contacts form. The field asks the network by itself as the person types, by
username and by name, and Add contact on a result sends the request; see
[`who-can-find-me.md`](who-can-find-me.md) section 6.1, which supersedes the
row, the "Add by username" form and the introduction step described below.

Looking up `@username` and looking up a channel's `@handle` are the same
mechanism over two namespaces - blind the name, ask, get an exact answer or
nothing - and a group handle would be a third namespace and the same
mechanism again. The person typing does not know which namespace the name
they were given lives in, and should not have to.

So it is one row, "Look up @name", offered under the conversation list when
nothing local matches and the text could be a name. It asks both namespaces
and routes what it finds: a channel opens its subscribe page, a person hands
the name to Contacts, which is where the contact request lives because that
flow asks for an introduction this field has no room for.

Two things it must keep doing. A name that cannot be canonical never costs a
lookup - the same `[a-z0-9_]`, 3-32 rule the blinder enforces is checked
first. And a network failure says so: telling someone their friend does not
exist because the connection dropped is the worst thing this can do.

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
- **Empty**: "No calls yet" and the action below.
- The screen carries ONE action, **New call**, which starts a group video
  call and lands the person in it, plus **Join by link** for a link that did
  not open by itself. [`call-links.md`](call-links.md) owns that decision and
  the reasoning; what matters here is that the destination named after the
  act performs the act, which it did not when this section was first written
  - it offered only two ways to handle a LINK, so placing a call meant
  leaving for Contacts or a chat.

### 5.3 A missed call elsewhere

Unchanged from [`calls.md`](calls.md) section 6 and still owed on two
clients: a missed call raises a notification (iOS does, web and Android do
not), and the conversation shows unread with `Missed call` (both natives do,
web does not - its call log is a store the chat list never reads). Those are
defects against the existing spec, not new work from this one, and they are
tracked in `PRODUCT-READINESS.md`.

## 6. What the data has to provide

Requirements, not an inventory of who has what. Where a client is further
from one than another, that is a fact about today and belongs in
[`PRODUCT-READINESS.md`](../PRODUCT-READINESS.md).

### 6.1 The merged list

ONE ordered sequence of conversations, each row carrying: kind, id, title,
avatar source, last-activity timestamp, preview line, unread count, pinned,
muted, archived.

Chats and groups already share a store everywhere, so they merge by dropping
a filter. Channels do not, and the mapping that costs something is the same
everywhere: a channel's "last post time" has to become the same field as a
chat's "last message time", or the one ordering has nothing to order by.

Pinned, muted, archived and unread are keyed on a conversation id a channel
does not carry. Until that is resolved a channel row shows none of them,
which is honest; inventing a badge that can never be non-zero is not.

### 6.2 The calls list

A **flat, cross-conversation, time-ordered** sequence, holding per row:
conversation id, kind, outcome, whether it was a video call, start time,
duration.

Two requirements follow from the shape rather than from any platform.

**It must be an index, not a scan.** A call belongs to a conversation, so the
tempting place to keep it is inside that conversation's transcript - and then
the Calls screen costs a walk of every transcript on every open, which gets
worse with use. Whatever each client already does, the list reads an index
written when the call row is.

**It must hold the parts, not the sentence.** A list sorts and filters on an
outcome and a timestamp. A row that stores the finished words ("Missed call")
cannot be sorted on, so the index carries the parts and the words are derived
from them.

### 6.3 Search

Matching over titles and previews is local and needs no new data. The network
lookup reuses the existing discovery client unchanged. Searching message TEXT
needs an index that does not exist and is out of this spec.

## 7. Not in this spec

- **Message search.** It needs a local index no client has, and it is what
  the remaining open part of section 4 is about.
- **Folders / custom filters.** The merged list is the point; a filter
  mechanism on top of it is a separate decision - taken in
  [`chat-folders.md`](chat-folders.md): a folder is a saved view of this
  list, and All chats stays this list unchanged.
- **Call history sync across devices.** The calls list is this device's, like
  the message log. Making it follow the person is the same question as
  cross-device message history and belongs with it.

## 8. One model, three implementations

Nothing in this document is a web requirement that the other two approximate.
The four destinations, their order, the one row shape, the ordering rule, the
three create items and the single lookup row are the same on all three; the
only differences are the **Per platform** notes above, and each is there
because the platform gives a person a different idiom for the same act, never
because one client was written first.

The rules that decide any of it - the ordering, what counts as a lookable
name - are shared code rather than three translations of a paragraph. What
this spec requires is only that there be one of each; where that one copy
lives was the maintainer's call, and it followed the project's own precedent -
the Rust core behind both FFI surfaces, which is why `should_request_profiles`
and `acting_committer_verdict` are written once. So they live in
`crates/lakko-ui-rules`: `order_conversations` and `looks_like_a_name`, reached
over wasm on web and over UniFFI on iOS and Android - and, since the info
layer went the same way, `person_info_sections`, `group_info_sections` and
`channel_info_sections` beside them
([`conversation-info.md`](conversation-info.md) section 6, which records the
three divergences the three hand-written copies had already produced).

Two consequences worth stating, because they are what make it a shared rule
rather than a shared file.

`order_conversations` returns the input **indices**, not the rows. A client's
real row carries an avatar, a preview, an unread count and a handle to open,
and none of that has anything to do with the order; handing it all across an
FFI boundary twice, in both directions, to learn a permutation would be waste.
Each client maps its rows down to the six fields the order reads, and reorders
the list it already holds.

`looks_like_a_name` does not restate the username rule - it calls the very
function the discovery blinder calls. A gate that merely agrees with a rule
today is a gate that disagrees with it later, silently, in the direction that
costs a person a proof of work to be told no.
