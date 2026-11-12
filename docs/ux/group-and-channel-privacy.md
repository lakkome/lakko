<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Group and channel @names, public and private

Date: 2026-10-31. Scope: web, iOS, Android. Architecture:
[ADR-0087](../adr/0087-group-and-channel-handles-and-privacy.md).

Everything below is a requirement for all three clients. Where a platform has
to differ, the difference is stated inline and marked **Per platform**. What is
built, on which client, is in [`PRODUCT-READINESS.md`](../PRODUCT-READINESS.md);
nothing here is built yet.

## 1. The rules, as the maintainer set them

1. Every group and every channel has an `@name`. One-to-one chats are unchanged.
2. Each is **Public** or **Private**.
   - **Public:** found in search. A person joins by sending a request.
   - **Private:** never in search. A person joins only by an invitation from
     the owner or an admin.
3. A person removed from a group loses it: the row leaves their list and the
   history is wiped from their devices. Added again, they see only new messages.
   A private group cannot be found by them afterwards.
4. A group may be created with nobody but its creator.

On channels, the same day:

5. A channel is a channel, not a group. It shares a group's screen shapes, and the
   difference is who may do what (section 12): the owner and the co-publishers post;
   everyone else comments and reacts. A channel has no calls.
6. A post carries what a message can (section 13).
7. The owner and admins invite people, public or private, from a contact picker and
   by link (section 14).
8. Public channels and groups are found by `@name` and by name (section 5).
9. The subscriber list says only what a person can act on, and the machinery goes
   (section 15).

**In a channel, the request is the subscribe.** A person presses Subscribe on a
public channel and reads at once. No one approves: at a million subscribers an
approval per subscriber cannot work ([ADR-0087](../adr/0087-group-and-channel-handles-and-privacy.md)
section 6; decided by the maintainer 2026-10-31).

**Co-publishers stay co-publishers** (corrected by the maintainer 2026-11-01): they post
as the owner does and moderate; the screen word is **publisher** (section 15).

## 2. The @name

- `[a-z0-9_]`, 3 to 32 characters, the username rule. One set of names for people,
  groups and channels, so `@name` is one thing.
- Written `@name` in the UI, always LTR and Latin.
- A member sees it in group info. A stranger sees it only on a public one.
- A changed `@name` stays reserved for this group for 30 days and stops working at
  once.

### 2.1 The proposed name

Nobody should have to invent a unique name for a family chat. The field is filled
in and needs no attention.

- From the group's name: lower case, ASCII letters and digits kept, spaces become
  `_`, everything else dropped, cut to 26 characters.
- **Private:** plus `_` and 4 random characters: `family_k3x9`.
- **Public:** no suffix: `hiking_club`.
- No usable letters (a Cyrillic or Arabic name, or no name): `group_k3x9`,
  `channel_k3x9`.
- It follows the name field until the person edits it, then stays theirs.
- One rule, in `lakko_ui_rules`, as `suggest_handle(name, kind, private, random)`.

## 3. Creating

### 3.1 New group

Entry: the create menu ([`navigation.md`](navigation.md) section 3). Spec
[`new-group.md`](new-group.md).

```
New group
              ( FW )[✎]           photo, optional: initials of the name until one is chosen
Group name    [                    ]
@name         [@family_k3x9        ]
              Only members see this.
Who can join  [ Private          v ]
              Only people invited by an admin can join. It stays out of search.
Add people (optional)
              [ Search contacts ]
              [ ] Anna Weber
              [ ] Ivan Petrov
[ Create group ]
```

- **Who can join** is a `LakkoPicker` with two values:
  - `Private`, default: "Only people invited by an admin can join. It stays out of
    search."
  - `Public`: "Anyone can find this group and ask to join."
- The helper under `@name` follows it:
  - Private: "Only members see this."
  - Public: "Anyone can find this group by this name."
- **Group name** is optional when Private, required when Public: "A public group
  needs a name."
- **Create group** is enabled when the `@name` is well formed and, if Public, there
  is a name. Nobody has to be picked.
- The photo is optional and never blocks Create. Choosing it is section 4.5.3.

### 3.2 New channel

The same form: the photo (optional, section 4.5.3), `Channel name` (required, as
today), `Description`, `@name`, `Who can subscribe`.

- `Private`, default: "Only people you invite can subscribe. It stays out of search."
- `Public`: "Anyone can find this channel and subscribe. Lakko may suggest it in
  Discover."
- When Public, two more pickers: `Topic` (default Other) and `Language` (default the
  app's language). Neither blocks Create.
- The `@name` helper reads "Only subscribers see this." or "Anyone can find this
  channel by this name."
- The optional `@handle` field and its hint go.

### 3.3 States of both forms

| State | What the person sees |
|---|---|
| Typing a malformed `@name` | Under the field: "3 to 32 characters: letters a-z, digits and _." Create is disabled. |
| Public, the name answers a lookup (500 ms after typing stops, from 3 characters) | Under the field: "That @name is taken." |
| Private, or the lookup answers nothing | Nothing. Whether a name is free is known only at Create. |
| Creating | Button reads "Creating...", the form is disabled. |
| The name is held | Under the field: "That @name isn't available. Try another." Nothing was created, and the form stays filled. |
| Too many today | "You've created a lot of groups today. Try again tomorrow." |
| No network | "Could not reach Lakko. Check your connection and try again." The form stays filled. |
| Secure setup failed | "Couldn't create this group. Nothing was created. Try again." |
| The photo could not be uploaded | Under the photo: "Couldn't upload the photo. Check your connection and try again." Nothing was created, and the form stays filled. Removing the photo lets Create go ahead without it. |
| Sending files is over its limit | Under the photo: "You have reached the limit for sending files. Try again later." As above. |
| Created | The conversation opens. Picks are added through the normal add path, as `new-group.md` says. |

The word "taken" is used only when the person can see the holder; "isn't available"
says nothing about what holds it.

## 4. On the info screens

Group info and channel info gain, in the header, the one line that says what this is
([`conversation-info.md`](conversation-info.md) section 2):

- `Private group · 12 members`
- `Public group · 1,204 members`
- `Private channel`, `Public channel`

Under it, the row **@name** with `family_k3x9` and a copy icon. Copy is `Copy` on
every platform, no system share sheet.

### 4.1 Changing it

The owner sees one row, **Who can join**, showing the current value, opening
**Group type**:

```
Group type
Who can join  [ Public v ]
@name         [@hiking_club        ]
[ Save ]
```

For a channel the screen is **Channel type**, with `Who can subscribe`, `@name`, and
`Topic` and `Language` while public. There is no separate Discover control: public
means Lakko may suggest it, and a private channel is never suggested.

An admin and a member see the header line and the `@name` row, read-only. Saving
asks first.

| Action | Dialog title | Body | Confirm |
|---|---|---|---|
| Private to public, group | Make this group public? | Anyone can find it and ask to join. Its name, photo, description and rough size become visible to everyone. | Make public |
| Public to private, group | Make this group private? | It leaves search. People can join only if the owner or an admin invites them. Everyone in the group stays. | Make private |
| ... with requests waiting | (same) | (same) + "{n} people are waiting to join. You can still answer them." | Make private |
| Private to public, channel | Make this channel public? | Anyone can find it and read it, and Lakko may suggest it in Discover. Its name, photo and description become visible to everyone. Posts from before stay readable only by people who already subscribe. | Make public |
| Public to private, channel | Make this channel private? | It leaves search and Discover. People can subscribe only if you invite them. Everyone subscribed stays. | Make private |
| New `@name` | Change @name? | The old @name stops working now. It stays reserved for this group for 30 days. | Change |

Going private on a large channel: the row shows "Updating subscribers..." with a
count, keeps going when the screen closes, and continues after a restart.

| State | What the screen shows |
|---|---|
| Saving | Save reads "Saving...". |
| A public group with no name | "A public group needs a name." Save disabled, with a link to the name row. |
| Failed | "Couldn't save. Try again." The rows show what is in effect. |
| Offline | "Will apply when you're back online." The chosen value shows; the header line shows what is in effect until it applies. |
| `@name` held | "That @name isn't available. Try another." |

### 4.2 What members see of a change

The header line and the `@name` row update in place. There is no timeline row.

### 4.3 The name in the title

A group's title is its name, then the member-derived title, then `@name`. A solo
group with no name is titled `@name`.

### 4.4 A group whose @name lapsed

A name is renewed quietly while the owner's device is used. If it lapses, because the
owner was away for months:

- The owner sees a banner in group info: "This group has no @name. Choose one." Group
  type opens with the proposed name.
- Others see the header line with no `@name` row.
- Nothing about the conversation changes.

### 4.5 The photo

Added 2026-11-02 (maintainer: groups and channels have photos too; the choices below
decided 2026-11-02, maintainer). A group and a channel have a photo, or a short video
(decided 2026-11-02, maintainer), the way a person does
([`profile-screen.md`](profile-screen.md) section 2). With none, the circle shows the
initials of the title on the tint of its id, as today. Architecture:
[ADR-0087](../adr/0087-group-and-channel-handles-and-privacy.md) section 18.

#### 4.5.1 Who sets it

The same people who rename it.

| | Group | Channel |
|---|---|---|
| Set, change, remove | the owner and admins | the owner |
| Everyone else | sees it | sees it |

Publishers of a channel see the photo and cannot change it, as they cannot rename
(section 15). The photo belongs to the group, not to the person who set it: it stays
when that person leaves, and changes only when someone who may set it changes it.

**Nobody has to keep it.** The devices of the owner and of the admins (a channel: of the
owner and the publishers) keep the photo's file from expiring by themselves, whenever
Lakko is open on them, with no control, prompt or message on any screen (decided
2026-11-02, maintainer). A replaced photo is deleted by the devices of whoever set it;
if that person has left, it expires after a year that nobody opened it (decided
2026-11-02, maintainer). Neither shows anywhere.

#### 4.5.2 Changing it on the info screen

The header avatar of the info screen is 96 pt. For whoever may set the photo it carries
the edit button of Settings > Profile (a 32 pt pencil on its lower trailing edge,
accessible label `Change photo`). Tapping the button, or the avatar itself, opens the
same in-app menu as the profile photo:

| Item | Shown when |
|---|---|
| `Photo or video` | always |
| `Camera` | the device has a camera |
| `Remove photo` | a photo or video is set; destructive colour |

`Photo or video` and `Camera` lead to the profile photo's crop screen and camera screen
unchanged ([`profile-screen.md`](profile-screen.md) sections 2.2 and 2.3): a square,
a still of at most 640 px or a video of at most 10 s, no sound kept, the same message
when a video cannot be used.

**There is no Save.** The info screen has no pending state: `Use` on the crop screen
is the confirmation, and the photo is sent from there, as a rename is sent when the
field closes. `Remove photo` asks first, because everyone loses the photo at once:

| Kind | Dialog title | Body | Confirm |
|---|---|---|---|
| Group | Remove the group photo? | Everyone in the group will see its initials instead. | Remove |
| Channel | Remove the channel photo? | Subscribers will see its initials instead. | Remove |

States of the header after `Use` or `Remove`:

| State | What the header shows |
|---|---|
| Uploading | The new photo, with a progress ring around the circle. The edit button is disabled. |
| Sent | The new photo. No toast and no timeline row, as for a rename (section 4.2). |
| Upload failed | The photo in effect before, and under the header: "Couldn't upload the photo. Check your connection and try again." with `Try again`. Leaving the screen drops the attempt; nothing was sent. |
| Sending files is over its limit | The photo in effect before, and "You have reached the limit for sending files. Try again later." |
| Uploaded, the device went offline | The new photo, and "Will apply when you're back online." under the header. It is sent when the device is back, also after a restart. |
| No longer allowed (the person was demoted while the crop screen was open) | The photo in effect, and "Only the owner and admins can change the photo." (a channel: "Only the owner can change the photo."). Nothing was sent. |
| The photo in effect is gone (its file expired or was deleted) | Initials, and for whoever may set it only: "The photo is no longer available. Choose another." Everyone else sees initials and nothing more. |

Two admins changing the photo at about the same time: the later change wins on every
device, as for the name.

#### 4.5.3 Choosing it at creation

The New group and New channel forms (sections 3.1 and 3.2) open with the avatar slot:
96 pt, the initials of the typed name (or a group or channel glyph before any name), and
the same edit button and menu, without `Remove photo` until one is chosen. After `Use`
the slot shows the chosen photo; choosing again replaces it.

The photo is uploaded when Create is pressed, before anything is created, so a failed
upload creates nothing (the states are in section 3.3). A public group or channel is
published with its photo.

#### 4.5.4 Where it shows

Every circle that stands for the group or channel shows its photo when this device holds
it, else the initials:

- the row in Chats and in a folder, the conversation header, the info header;
- local search results, and the **Groups and channels** rows of network search
  (section 5) for a public one;
- the card (section 5.1), the `Requests sent` list (section 6.2);
- the group invite page and a channel's link page (section 7.2), and the channel
  invitation card in a chat (section 14);
- the Forward picker and any other list of conversations;
- the call screen of a group call ([`navigation.md`](navigation.md) section 5);
- Feed items from groups and channels.

A video plays muted and looped only in the large views: the info header, the card of
section 5.1 and the invite and link pages. Everywhere else its still is shown.

Notifications show no conversation photo, as they show no person's photo: the
background paths never download one.

| State | What the circle shows |
|---|---|
| There is a photo, not downloaded yet | Initials; the photo replaces them when it arrives, no spinner |
| Download or decryption failed | Initials; tried again on the next display after 1 minute |
| Removed | Initials, on every device, when the change arrives |
| Changed | The new photo when it arrives; the old one until then |
| Gone (expired or deleted) | Initials, and no error (section 4.5.2 for the people who may set it) |
| Invite or link page, not signed in | Initials: a photo is downloaded only by a signed-in device |
| Invite or link page, the photo changed since the link was made | The photo the link was made with, or initials once that one is gone |

**A newcomer** gets the photo with the name when they are added, and a new subscriber
with the channel's name. A device added to an account gets it at the same moment.

**A public group or channel** shows its photo to anyone who finds it, before they join.
Going private stops new people seeing it in search, and the owner's device sends the
same photo again in a form only members can open, with nothing to do for anyone
(decided 2026-11-02, maintainer). Someone who already saw the card may still have it,
like the name.

**When a membership ends** (section 9) the downloaded photo goes with the rest of the
conversation.

#### 4.5.5 Per platform

The avatar component that draws a person (web `ui/Avatar.tsx`, iOS `ProfileAvatar`,
Android `LakkoAvatar`) draws a group or channel too, looked up by the conversation
instead of a person's key. Behind it each client has one store of downloaded photos
for people, groups and channels, bounded in size; when it is full, the photo drawn
least recently goes first, and it is downloaded again the next time it is drawn,
showing initials until then (decided 2026-11-02, maintainer). The menu is `LakkoMenu`, the dialog `LakkoDialog`, the crop
and camera screens are the profile photo's. Nothing differs between platforms beyond
the design system's idioms.

## 5. Finding a group or channel

Search is one field ([`who-can-find-me.md`](who-can-find-me.md) section 6.1). It finds
people, groups and channels from one network query, and the person does not choose a
kind.

**What it replaces.** Today Chats search reaches a channel by exact `@handle` only, shows
one line, "Channel @x", with no name, and opens a subscribe page. A channel with no handle
is not found, a channel is not found by its name, and a handle outside the username rule
is accepted at creation and then never asked for. One rule for every `@name` ends the
last of these, and the rows below end the rest.

Public groups and channels appear in the one result list of
[`who-can-find-me.md`](who-can-find-me.md) section 6.1 beside the people found, with no
heading of their own (maintainer, 2026-11-04).

Row: avatar, name, `@name`, second line:

- `Public group · 100+ members`
- `Public channel · 10k+ subscribers`

`Open`, instead of a card, when the person is already in it. A private group or channel
appears only in the local results of its members, like any conversation.

A name is matched by `@name` exactly, and by the start of a word from 3 letters, the
same rule as a person who chose Everyone. Up to 20 come back; more letters narrow it.
The states are those of who-can-find-me section 6.1, word for word, including "Nothing
found" when nothing matches. A private group and a name no one holds read alike.

Discover keeps its own search.

### 5.1 The card

Opening a row opens a card, a screen with a back control.

```
[avatar]
Hiking club
@hiking_club
Public group · 100+ members

Weekend walks around Berlin.

[ Request to join ]
```

For a channel, `Subscribe`: it subscribes at once and opens the channel under its name,
with no second step (2026-11-04). The size is what the owner last published. No member is
listed and no message is shown: those are for members.

| State | What it shows |
|---|---|
| Already in it | `Open`. |
| Request already sent | `Request sent`, disabled. Section 6.2. |
| No longer available | "This group is no longer available." |
| Not reachable | "Could not reach Lakko. Check your connection and try again." `Try again`. |

## 6. Asking to join a public group

### 6.1 Sending

Press `Request to join`. Nothing to fill in: the request carries the person's name and
photo.

| Outcome | What the card shows |
|---|---|
| Sent | "Request sent. An admin will see it when they next open Lakko." |
| The group's queue is full | "This group has too many requests right now. Try again later." |
| Out of requests | "You've asked to join a lot of groups today. Try again tomorrow." |
| No network | "Could not reach Lakko. Check your connection and try again." |

### 6.2 Waiting

A request cannot be withdrawn: it is already in front of the admins. There is no Cancel.

While one is open, a **Requests sent** row sits at the top of Chats, with a count. It
opens a list: avatar, name, `@name`, and one line.

| State | Line | Control |
|---|---|---|
| Sent, under a day | Waiting for an admin. | none |
| A day or more | No answer yet. | `Ask again` |
| Joined | The row leaves; the group appears in Chats. | none |

The person is never told a request was refused. It goes unanswered. "No answer yet" is
true either way, and the app claims no more than that. A device asks once per group per
day (amended 2026-11-12, maintainer: it was 7 days, which was too long to wait).

The page a group's invite link opens follows the same rules: after `Request to join` it
shows "Request sent. An admin will see it when they next open Lakko." and no other
control; opened again within the day, it shows the same line; after a day, "No answer
yet." and `Ask again`. There is no `Request again`. When the join lands, the page moves
straight to the group's chat, with no button in between.

**Admitted.** The group appears in Chats with an ordinary notification, "You joined
{name}". History before the join is not there.

**Who can find you there.** A public group counts as any group
([`who-can-find-me.md`](who-can-find-me.md) section 5; maintainer, 2026-10-31): the
person's own setting governs, and it reaches every member of a public group they join.

**Per platform.** `Requests sent` is a row above the list on all three.

## 7. Invitations

### 7.1 A private group

Two ways, both for the owner and admins:

- **Add members** ([`conversation-info.md`](conversation-info.md) section 2.4). Direct.
  Nobody approves.
- **Invite link**. As today ([`group-invite-page.md`](group-invite-page.md)): the person
  opens it and presses `Request to join`, and an admin approves. A link can leak, and the
  approval is what stands behind it.

Links work the same on a public group.

### 7.2 A channel's link page

How a link and an invitation are made is section 14. The page a link opens, on the `/g/`
page's card:

```
Anna invites you to
Berlin news
Private channel

[ Subscribe ]
```

| State | What it shows |
|---|---|
| Subscribing | Button reads "Subscribing...". |
| Waiting to be let in (private) | "Subscribed. Posts appear once the owner or an admin next opens Lakko." The channel is in Chats with an empty timeline. A public channel has no such state. |
| Admitted | Posts appear, and the line goes. |
| Revoked, expired, used up | "This invite link is invalid or has expired." |
| Damaged | As `group-invite-page.md`: "This invite link is damaged. Ask for a new link." |
| Too many requests, could not load | As `group-invite-page.md`. |

Revoking stops new people. Someone already in stays; to remove them, use the subscriber
list.

### 7.3 Removing a subscriber

In a private channel the subscriber list shows `Remove` per row, with a confirmation:
"Remove this subscriber? They stop receiving new posts." In a public channel there is no
`Remove`: anyone can subscribe again.

## 8. What an admin sees

The members panel's `Join requests ({n})`
([`proposal-based-membership.md`](proposal-based-membership.md) section 5). Owner and
admins only.

Row: avatar, the name the request carries, `@username` when it is verified, one line,
`Approve`, `Reject`.

- Line: `Asked 2 hours ago`. From a link: `Invited by Anna · 2 hours ago`. Both are
  recorded by the admin's device when it first reads the request; a request it holds
  no record of shows no line.
- A contact shows their contact name.
- A name in a request is what the person claims, unverified, with no mark, like a
  `Forwarded from` line.

When 64 are waiting the list stops growing and the count reads `64+`, with the line
"More requests are waiting. They appear as you answer these."

`Reject all` at the end of the section header, when there are two or more:

- Title: `Reject all requests?`
- Body: "This removes every request waiting, including ones you haven't seen."
- Confirm: `Reject all`.

A flood shows here as many strangers. `Reject all` clears it; making the group private
stops it. `Approve` is otherwise as today: the row reads `Waiting to be added` until the
change lands.

A request is not pushed: an admin sees it next time the group is read. In a group the
person is admin of, it shows as the count on the info screen; no notification.

## 9. When a person's membership ends

Removed or left, it ends the same way. This replaces sections 6.2.4 to 6.2.7 of
[`proposal-based-membership.md`](proposal-based-membership.md).

**Who it applies to:** it fires when no device of the person is left in the group. A
device briefly dropped while its account stays in the group keeps everything, and shows
the `waiting` state of [`conversation-backup.md`](conversation-backup.md).

### 9.1 Removed

The moment the device learns of it:

- The row leaves Chats, and leaves every folder.
- Everything from that group goes: messages, pins, drafts, saved messages from it,
  files, feed items, the name.
- It goes from the person's other devices too, each when it next runs.

Saying nothing would look like a fault, so the person is told once.

| Where they were | What they see |
|---|---|
| Anywhere else | A short notice at the bottom of the screen, "You are no longer in {name}.", that goes away by itself after 4 seconds. No close icon, no banner that stays. |
| In that conversation | A dialog over it. Title: "You are no longer in {name}." Body: "The group and its messages are gone from your devices." One button, `OK`, then Chats. If the composer held text, the text is shown selectable under the body, with `Copy`. |
| In a call in that group | The call ends: "You are no longer in this group." Then as above. |

Each device shows it once. It never says who.

Several removals (decided 2026-11-06, maintainer; replaces the banners of 2026-11-04, which piled up and stayed): one notice at a time. Removals noticed together show one notice, "You are no longer in {name} and {count} more."; a removal noticed while a notice is up replaces it with the combined text. A conversation whose dialog was already shown gets no notice. Nothing is kept for later: a notice the person did not see (the app was closed) is not shown at the next start.

The notice's `{name}` is held in memory only, for those 4 seconds.

### 9.2 Added again

The group appears as a new conversation: no messages, an ordinary row, "You joined
{name}" when a notification applies. Nothing from before returns.

### 9.3 Finding it afterwards

A private group is not findable, by them or anyone; a stale link they still hold is only
a request an admin can refuse. A public group can be found, and they can ask again; the
request shows like any other.

### 9.4 Leaving

The same, for one rule. The confirmation in `proposal-based-membership.md` section 6
becomes:

- Title: `Leave this group?`
- Body: "It leaves your list and its messages are removed from all your devices. You
  keep receiving messages until an owner or an admin applies your request."
- Buttons: `Leave` (destructive) / `Cancel`

While it waits the row stays, with `Waiting to leave.` When it lands the row goes with no
banner: the person asked for it. A public group can be found again; a private one needs a
new invitation.

### 9.5 A channel

Unsubscribing, or being removed from a private channel, removes the row and its posts on
every device of the person, in the same way. The notice reads "You are no longer in
{name}." on removal.

## 10. A group of one

Decision 4. A real state, not a transition.

- **Chats:** an ordinary row. Creation counts as activity, so it sits at the top. With
  nothing sent, the second line is empty.
- **The conversation:** a timeline row "You created this group", then:

  ```
  Only you are here.
  [ Add members ]  [ Copy invite link ]
  ```

  This goes when a second person lands. The composer works: what the person sends
  reaches their own other devices.
- **Call controls:** absent. There is nobody to call. They appear with a second person.
- **Info:** header `Private group · 1 member`. Add members, the invite link, rename,
  group type as usual.
- **Exit block:** `Delete group` (section 11) instead of `Transfer ownership` and
  `Leave group`.
- **The first person added** is an ordinary add: their row shows `Waiting to be added`
  only if it does not land at once, and it does, since the owner applies it.
- Other devices of the person count as the same person, so a group of one person on two
  phones is still a group of one.

A person who leaves can make the owner the only one left. The group then reads as a group
of one, with its messages intact.

## 11. Deleting a group

Only for an owner alone in the group. An owner with others transfers first.

- Control: `Delete group`, destructive styling, in the exit block.
- Title: `Delete this group?`
- Body: "This removes the group and its messages from all your devices and frees
  @{name} in 30 days. This cannot be undone."
- Buttons: `Delete` (destructive) / `Cancel`

The row goes with no banner. If the network is down: "Could not reach Lakko. Check your
connection and try again." and nothing changes.

## 12. A channel's screen

**A channel is not a group, and it does not need a screen shape of its own.** A channel
opens where a group opens, in the same view. The channel route, its own list pane, its own
header and the Manage page go. What differs is who may do what, and three things a group
does not have: comments under a post, no calls, and later broadcasts (section 13.4).

| | In a group | In a channel |
|---|---|---|
| Row in Chats, order, folders | the same | the same |
| Header: avatar, title, one line | `Private group · 12 members` | `Public channel · 10k+ subscribers` |
| Timeline, day separators, media viewer, search in the conversation | the same | the same |
| Composer: text, attach, voice, video note, GIF, emoji | everyone | the owner and publishers |
| Message menu | Reply, Forward, React, Pin, Save, Copy, and Edit and Delete on one's own | React, Forward, Save, Copy, and Edit and Delete for the owner and publishers |
| Call buttons | yes | none, for anyone |
| Info: tap the title | a pushed screen, [`conversation-info.md`](conversation-info.md) section 3 | the same screen |

**The few things only a channel has:**

- A post has no sender line. The channel is the sender: the bubble is an incoming bubble
  with no name, on every device, the owner's included.
- A subscriber has no composer at the bottom of the channel: a subscriber writes under a
  post (section 13.2). The timeline runs to the bottom of the pane; it is not a disabled
  field, because nothing is being withheld.
- A post is a card with the width of the pane, because comments hang under it.
- `Subscribe` and `Unsubscribe` where a group has joining and leaving.
- A subscriber count on the header line, not a member count. The number follows
  section 15.

**What goes, and where to:**

| On the Manage page today | Now |
|---|---|
| Channel: name, description | Rows on the info screen, as a group's rename |
| List in Discover | Part of Channel type (section 4.1): public means Lakko may suggest it |
| Subscribers | The `Subscribers` row (section 15) |
| Co-publishers | The `Publishers` row (section 15) |
| Protection, `Refresh now` | Gone. The channel looks after this by itself. Removing someone from a private channel does what it did |

Nothing that kept running because the Manage page was mounted depends on a screen any
more: a channel works whether or not its owner opens it.

### 12.1 The info screen of a channel

Same order as a group: header, actions, detail, exit block.

```
[avatar]
Berlin news
@berlin_news
Public channel · 10k+ subscribers

Daily headlines from Berlin.

Invite people                     >   owner, publishers
Invite link                       >   owner, publishers
Subscribers                 12,408 >  owner, publishers: exact; everyone else: the band
Publishers                         3 >  owner, publishers
Channel type                      >   owner
Members can find me          [on]     everyone, per who-can-find-me section 5

Unsubscribe                           a subscriber
```

The owner changes the photo from the header avatar (section 4.5.2).
Rename and the description are edited in place in the header by the owner, as a group's
name.

## 13. What a post can carry

A post is a message. The composer sends, and the timeline shows, the same things as in a
group: text, a photo, a file, a voice message, a video note, a GIF. The upload, the
progress, the failed-upload state and the viewer are the group's.

| State | What it shows |
|---|---|
| Publishing | As a message being sent. |
| Failed | The bubble's failed state, with Retry, as in a chat. |
| Not a publisher any more | "Only the owner and publishers can post here." The composer goes. |
| Older post, no key on this device | "This post isn't available." |
| File past its life, or deleted | The "no longer available" state of [`saved-messages-and-file-lifetime.md`](saved-messages-and-file-lifetime.md). A post lives 30 days and its file about 45. |
| Edited | The `edited` mark, as in a chat. |
| Deleted | The post leaves the timeline for everyone, as delete for everyone does. |

**What someone who subscribes later reads.**

- Public: the last 30 days, files included.
- Private: from about when they were let in.

The Channel info says neither; the timeline simply starts where it starts, with a line at
the top when older posts exist that this device cannot read: "Earlier posts aren't
available."

**No typing and no read marks** in a channel, for anyone.

### 13.1 Reactions

They look and behave as in a group: chips under the post with a count on each, the
person's own highlighted. Pressing a reaction adds it, pressing it again takes it off, and
the same goes for the chip. A reaction on a comment works the same way.

- Any emoji, as in a group, within what the owner allows (all, none, or a chosen list),
  and at most 3 per person on a post (decided 2026-11-02, maintainer;
  `docs/ux/reactions-and-emoji.md`). A reaction whose emoji this app does not know is not
  shown.
- The reaction shows at once on the person's device. It goes out when it can: if the
  device is out of tokens it waits and leaves with the next ones. Nothing is refused and no
  error shows. Taking a reaction off always goes out at once.
- Offline: as above.

### 13.2 Comments

**Under the post, in place** (maintainer, 2026-11-01). Telegram's move to another screen is
not used.

```
[ post ]
[ reaction chips ]
Anna  finally
Igor  works on Android
Show 16 more comments
[ Write a comment ]
```

- Under every post with comments, the first 2 and `Show {n} more comments`. With none, a
  quiet `Comment` link.
- `Show more` opens them in place, 20 at a time, oldest first, with `Show more` at the
  end of the page. A post can be collapsed again by the line at its end. The rest of the
  channel stays where it was: the scroll position holds on the post.
- The field appears when the thread is open. It takes text and emoji only in this version.
- A long press or right-click on a comment: React, Reply, Copy, and Delete for its author
  and for the owner and publishers. `Reply` quotes it, one level deep, as in a chat.
- An author is a name and an avatar. A name in a comment is what its author typed, and the
  screen does not claim it is a contact: no `@username`, no link to a profile.
- A comment that fails to send stays in the thread with Retry.
- Newly arrived comments do not move the thread while it is open: a line `{n} new comments`
  appears at its end.

| State | What it shows |
|---|---|
| Loading a thread | A thin line under the post, no spinner in the timeline. |
| Comments cannot load | "Comments didn't load." `Try again`. |
| Publisher removed a comment | It goes for everyone, with no trace. |
| Older post, no key on this device | No comments and no chips: the post itself is unavailable. |

### 13.3 Removing and banning

A person chooses `Ban from comments` on a comment (owner, publishers). The word on the
screen is the same in both kinds of channel; what happens is not, and the difference
cannot be hidden.

- **Private channel:** the person is removed, as `Remove` does (section 7.3). They stop
  reading and stop writing from the next key.
- **Public channel:** nobody can be kept from reading a public channel. Their comments
  disappear for everyone and new ones are not shown. A person's reactions cannot be banned:
  no one can tell whose they are.

`Banned` is a list in the info screen's `Subscribers`, for the owner and publishers, with
`Unban`. A banned person is told nothing.

### 13.4 Broadcasts

Not in this spec, and not designed. A channel has no calls. A broadcast is a kind of call
we do not have yet: the organiser speaks and shows video, and can let others speak for a
while. It needs its own design.

## 14. Inviting people to a channel

For the owner and publishers, public or private. The same row and the same screens as adding
to a group, with one difference, said here so it reads as a decision: a group's add puts
a person in, and a channel's invitation waits for them to press Subscribe. A read-only
stream a person never asked for is spam.

### 14.1 Invite people

The Add members screen ([`conversation-info.md`](conversation-info.md) section 2.4), titled
**Invite people**, trailing button **Invite** with the count. Contacts only: no `People`
search, because the invitation is a message in the one-to-one, and a stranger has none.
Listed contacts exclude people known to subscribe, by their card; an already-subscribed
person who shares no card can be invited and sees `Open`.

**Invite.** The screen closes back to the info. Each person gets a message from the
inviter in their one-to-one, started if there was none:

```
[avatar]  Berlin news
          Public channel
          [ Subscribe ]
```

It uses the channel's name and description as the inviter's device holds them. `Subscribe`
becomes `Open` once subscribed. On a private channel each card is a one-use invitation.

| State | What it shows |
|---|---|
| A send failed | In the info: "Could not invite {names}. Try again." The others go. |
| Card opened after the link was revoked | `Subscribe` gives "This invite link is invalid or has expired." |
| Used already by someone else | The same. |

### 14.2 Invite link

**Invite link** on the info screen: the group's screen, with `Copy link`, an optional
expiry and use limit, the list of links and `Revoke`. A public channel has a link too,
which opens the page of section 7.2 and is not needed to subscribe.

A publisher's links are their own to revoke; the owner may revoke any, as far as the links
sync between one account's devices. Revoking stops new people; section 7.2.

## 15. Subscribers and publishers

**The words.** On the screen a co-publisher is a **publisher**. A publisher posts as the
owner does, edits and deletes posts, deletes comments, bans, and invites and removes
subscribers. Only the owner renames the channel, changes its type and adds or removes
publishers.

### 15.1 Subscribers

What a person can be told is a number, and the names of people who share one.

**To the owner and publishers** the row reads `Subscribers  12,408`. **To everyone else** it
reads the band, `10k+`, and opens the same list as today.

Opening it shows the group's member screen, with the title `Subscribers (12,408)`:

- One row per person who shares a name: avatar, name, `@username` where they let the
  channel's members find them, and the role for the owner and publishers.
- At the end: "{n} others." They are subscribers who share no name. Nothing more can be
  known about them.
- **Private channels, for the owner and publishers:** those others are listed too, one row
  each, labelled by the link they came through: `From link "Friends"`, with `Remove`. A
  person who came by a contact invitation shows `From an invitation`.
- **Public channels:** no `Remove`.

What it never shows: "Subscriber 1", "Ready", "Setting up", a retry, a key, or anything
about how a device got its access. A person kept waiting sees it on their own device
(section 7.2).

| State | What it shows |
|---|---|
| Loading | The count, and the list loading in place. |
| Nobody yet | "No subscribers yet." and `Invite people`. |
| Failed | "Couldn't load subscribers. Try again." and `Try again`. |

**At a million subscribers** the row is the number. The list loads a page at a time as it
is scrolled, and search on it matches only what has loaded. It is not a roster, and it
does not try to be: nobody can read a million names, and no one is on it who did not
share theirs.

### 15.2 Publishers

The row `Publishers` opens a list: the owner, then each publisher, with a `Remove publisher` for the
owner. Everyone else sees no such row. A group's members and roles live in one list; a
channel's cannot, since a million people are not a roster, so publishers have their own row.

- **Add publisher** (owner): the Invite people screen, one pick. The person gets a card in
  their one-to-one: "{name} invites you to be a publisher of {channel}." with `Accept`. When
  they accept, it applies with no further step, because the owner already named them.
- **Remove publisher** (owner), confirmation: "Remove {name} as publisher? They can no longer post
  or invite."; they stay subscribed if they were.
- A copy-paste link for admins and the `Join as co-publisher` menu item go.
- A publisher's own row offers `Step down`, the same action on themselves.
- **Only the owner's device renews the channel's keys** (maintainer, 2026-11-08): the
  daily renewal and the one after a publisher is added or removed. A publisher's device
  never does it and shows nothing about it. A channel whose owner does not open Lakko is
  not renewed until the owner returns; nobody is told, and posting goes on as before.
- **A subscriber a publisher removes stops reading new posts once the owner's device next
  opens Lakko.** The removal itself is immediate on the service (the person is off the
  list at once), but the keys are renewed only by the owner's device, which the
  publisher's device asks to do so. Until then the removed person can still read what is
  posted. The publisher sees the person leave the list and nothing more; a removal by the
  owner renews the keys at once.

#### 15.2.1 A new publisher reads the earlier posts

Maintainer decision, 2026-11-08. A person who becomes a publisher reads the channel's
earlier posts, as far back as the owner's device can still read them. When the owner's
device approves the request, it hands the new publisher the keys it holds for the channel's
earlier posts, with the read key it already sends. The person sees the posts appear; there
is no step and no message about it.

- An empty new channel shows "No posts yet. Write the first one below.", never "Earlier
  posts aren't available.": the channel's own name and description, written when it was
  made, open on the new publisher's device too.
- "Earlier posts aren't available." shows at the top of the timeline only when a post
  older than the readable ones exists and this device cannot open it: posts older than
  the owner's device can read, or ones from before the owner's device holds keys (the
  newest 64 key periods are handed over, chosen; at the default of one change a day that is
  about two months, longer than posts are kept).
- If the keys arrive after the person opened the channel (the owner's device sends them on
  its next run when a send failed), the posts appear in place and the line goes.
- A publisher who stepped down or was removed and is added again gets the same hand-over.

What it never shows: a count of keys, a "syncing history" line, or a retry.

#### 15.2.2 Waiting for the owner

Maintainer decision, 2026-11-08: a publisher request the owner refused or never answered
follows a group request (section 6.2). The waiting lives on the invitation card in the
one-to-one, where it was accepted; it is not listed in `Requests sent`, which holds the
requests to join that a person sent from search.

| State | Card shows | Control |
|---|---|---|
| Sending | The button, disabled, reads "Waiting for the channel owner to approve..." | none |
| Sent, under a day | "Waiting for the channel owner to approve..." | none |
| Sent a day ago or more | "No answer yet." | `Ask again` |
| Asking again | The button, disabled, reads "Waiting for the channel owner to approve..." | none |
| The link no longer works | "This invite link is invalid or has expired." | none |
| Approved | `Open`; the channel is in Chats | `Open` |

- **The person is never told of a refusal.** An owner who rejects a request sends nothing;
  the request goes unanswered, and "No answer yet." is true either way.
- **`Ask again`** sends the same request again through the same link the card carries:
  the card fetches the link again, and when it still works the device asks the owner's
  device once more, as on the first `Accept`. The link counts the person once, so asking
  again does not use up a link meant for one person. The day starts again from the new
  request. A device asks again at most once per channel per day; `Ask again` is not
  shown before that (amended 2026-11-12: it was 7 days, as for a group).
- **When the link no longer works** (the owner revoked it, it expired, or the channel is
  gone), `Ask again` shows "This invite link is invalid or has expired." in place of the
  button, and the card keeps that line. The earlier request is not withdrawn: if the
  owner's device approves it later, the channel still arrives. To be asked again, the
  person needs a new invitation from the owner.
- No network on `Ask again`: "Could not reach Lakko. Check your connection and try again."
  under the card, and `Ask again` stays.
- A request is never withdrawn, and the card offers no Cancel.

## 16. Copy, the short list

| Key idea | Copy |
|---|---|
| Header, group | Private group · {n} members / Public group · {n} members |
| Header, channel | Private channel / Public channel |
| Search row | Public group · {band} members / Public channel · {band} subscribers |
| Held name | That @name isn't available. Try another. |
| Visibly taken | That @name is taken. |
| Bad form | 3 to 32 characters: letters a-z, digits and _. |
| Request sent | Request sent. An admin will see it when they next open Lakko. |
| Waiting | Waiting for an admin. |
| Past a day | No answer yet. |
| Publisher request, waiting (section 15.2.2) | Waiting for the channel owner to approve... / No answer yet. / Ask again |
| Removed | You are no longer in {name}. |
| Comments | Comment / Show {n} more comments / Write a comment / {n} new comments |
| Comments failed | Comments didn't load. |
| Moderation | Ban from comments / Banned / Unban |
| Photo | Change photo / Remove photo / Remove the group photo? / Remove the channel photo? |
| Photo failed | Couldn't upload the photo. Check your connection and try again. |
| Photo not allowed | Only the owner and admins can change the photo. / Only the owner can change the photo. |
| Photo gone | The photo is no longer available. Choose another. |

Never in copy: journal, knock, epoch, record, OPRF, token, key, commit, proposal. No
statement of what servers cannot see. A public channel carries no lock and no claim of
privacy.

## 17. Data the UI needs (input to the ADR)

- Per group and channel, synced among members: `@name`, the flag, a stamp, a way to
  verify the name.
- `suggest_handle` and the handle check in `lakko_ui_rules`.
- Per public group or channel from a lookup: kind, id, name, description, size band, a
  knock key or a read key.
- Claim outcomes: created, held, out of tokens, unreachable.
- Per device: the list of requests sent (group, name, `@name`, sent at), kept until joined
  or dismissed.
- Per admin: the retained requests with the carried profile, a count, whether more wait.
- Per device, per publisher request (section 15.2.2, 2026-11-08): the channel, the link the
  card carries, and when it was last sent; kept until the channel is joined or gone.
- Per owner device (section 15.2.1, 2026-11-08): which channel key periods it has handed to
  which publisher, so a failed hand-over is sent again on the next run; the hand-over
  itself is an envelope sealed to the publisher (ADR-0087 amendment of 2026-11-08).
- Per private channel: invite keys, their use limits, a list of links.
- A durable "ended" record per conversation, the left-or-removed reason, the title for the
  notice, and a synced register so other devices follow.
- `people` as distinct accounts, to tell a group of one.
- Per channel for its owner and admins: an exact subscriber count, the member cards a page
  at a time, the admins list, each private subscriber's link label.
- A post as the chat body, with a server cursor as its message id, and the edit and delete
  ops on it.
- An invitation card in the chat body.
- Per post a page of counts: reaction, count; a comment count; the first 2 comments. Per thread a page of comments with a cursor.
- Per account the reactions it gave, shared among its devices and kept on them only, so
  that a second device knows what the first gave.
- Per channel a ban list, sealed, folded by every reader.
- Per group and channel a photo reference (a still, optionally a video) with a stamp,
  synced among members and subscribers, set only by whoever may rename; in the public
  record of a public one; in the preview of a group link and a channel link.
- Per device a durable "photo owed" record per conversation (uploaded, not yet sent),
  and the existing avatar cache keyed by file.
- Per photo, whether its file is gone, as distinct from a failed download.

## 18. One model, three implementations

One form, one header line, one search section, one card, one requests row, one banner.
What differs is the idiom of a picker, a pushed screen and a banner, and those are the
design system's. **Which sections an info screen shows stays one copy in
`lakko_ui_rules::info`**, which gains `Handle`, `GroupType`, `DeleteGroup`, `InvitePeople`,
`Subscribers`, `Admins`, and a group's `DeleteChat` goes. `can_transfer` and `alone` are
inputs. A channel's blocks and a group's come from one function over one capability
record, so a channel cannot grow a screen of its own.

The avatar component is one per client and draws people, groups and channels alike
(section 4.5.5).

**Per platform.** The group type screen is a pushed screen on all three. The picker is
`LakkoPicker`. The removal banner is the design system's Banner.

## 19. Not in this spec

- A shareable address for a public name.
- A ban list. A removed person may ask again in a public group.
- "Let anyone join" with no approval on a public group.
- Telling a person their request was refused.
- Transferring a channel's ownership.
- Attachments in comments.
- Broadcasts (section 13.4).
- Pins in a channel.
