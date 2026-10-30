<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Conversation info

What opens when a person taps the name at the top of a conversation.

Everything below is a requirement for all three clients. Where a platform
genuinely has to differ, the difference is stated inline and marked
**Per platform**.

## 0. What was wrong

Checked on all three clients on 2026-09-23, signed in, against a deployed
stack.

**A one-to-one chat opened a group panel.** Tapping Anna Weber's name showed
`Members (2)`, a `Group name` field, `Create invite link`, `Make admin`,
`Transfer ownership`, and `Leave group` with the explanation *"Transfer
ownership first - a group cannot be left without an owner."* Every one of
those is meaningless for a conversation between two people: there is no
group to name, no owner to transfer, no admin to make, and nothing to leave
that is not simply the other person.

The cause is one panel doing three jobs. `MembersSheet` on iOS, the members
`Show` block on web and its Android twin are the same surface whatever the
conversation is, with the group-only parts hidden by a flag - so a one-to-one
chat gets the group panel minus some rows, which is not the same thing as a
screen about a person.

**On web it read as nothing happening.** The header is clickable and does
call `toggleMembers`, but what opens is a section that pushes the
conversation aside rather than a layer over it, and in a one-to-one chat
almost all of its content is behind `isGroupChat()`. A panel that shoves the
messages sideways to show two rows is indistinguishable from a glitch.

**The screens that should have opened already exist.** `ContactDetailScreen`
and `ChannelDetailScreen` are built on all three clients. The header simply
never pointed at them.

## 1. One way in, three things behind it

Tapping the conversation's name and avatar opens **info about that
conversation**, and which info is decided by what the conversation IS:

| Conversation | What opens |
|---|---|
| One-to-one | the person |
| Group | the group |
| Channel | the channel |

It is the ONLY way in. The `Members` item in the header's overflow menu goes:
two routes to one screen means two things to keep in step, and the one in the
menu is the one that drifts. Anything the menu offered that is not info -
search, mute, wallpaper - stays where it is.

## 2. What each one holds

Common shape, so the three read as one product: an identifying header
(avatar, name, and the one line that says what this is), then the actions,
then the detail. What differs is what belongs in each.

### 2.1 A person

Reuses the contact screen that already exists.

**A person you have a one-to-one conversation with is a contact, always.** The
conversation and the contact row used to be written by different events (the
conversation by the Welcome, the contact by Accept or by the acknowledgement of
your own request), so a device could hold a named conversation and an empty
Contacts tab - seen live on 2026-09-26 on a second device, which is admitted to
the account's conversations without any contact travelling with them. Every
client now derives the contact from the conversation: whenever the set of
conversations changes, each one-to-one peer without a row gets one (named by
the profile they sent, with their account id when a profile carried it), and a
row that never recorded its conversation is completed. The "not one of your
contacts" state is gone with it - the copy, the screens and the `is_contact`
flag the shared rule took: a person's card is the contact card. A one-to-one
whose roster cannot be read (no record can be resolved) simply has nothing to
open from its header; it is not a state with a screen.

- Name and `@username`.
- Message, Call, Video - always, wherever the card was opened from. From the
  conversation, Message simply returns to it. (Until 2026-09-26 Message was
  withheld when the card was reached from the chat's own header, which needed
  an "origin" to be threaded through the shared rule and gave the same person
  two cards; the origin is gone.)
- **Safety number** - the out-of-band check, which is the one security thing
  a person acts on and so is the one that belongs in front of them
  ([`design-system.md`](design-system.md)).
- Remove contact, with a confirmation. Local: it takes the person out of
  this device's list, tells them nothing, and leaves the conversation where
  it is - leaving a conversation is the conversation's own action. The
  removal stays (amended 2026-10-24): the rule that makes a one-to-one peer a
  contact skips a removed person, so the remaining conversation never brings
  them back; adding them again (Accept, an invite link, starting a chat from
  their profile) does.
- **Add to contacts**, on the card of a person the viewer removed (amended
  2026-10-24). The conversation's header still opens their card; it shows
  Message, Call or Delete this chat, and the safety number as before, and
  `Add to contacts` in place of Rename and Remove contact, which need a contact
  row to act on. Local and instant: it puts the person back on this device's
  list with the conversation they have, and sends nothing. The person is not
  listed in Contacts until then. The shared rule takes `is_contact` again
  for this one case (`false` only for a removed person whose one-to-one
  remains).

Never: a group name, an invite link, roles, ownership, or `Leave group`.

**And never a way to add a third person.** A one-to-one does not become a
group by growing; adding somebody starts a new group. Decided on 2026-09-23,
after the first implementation pass left web able to do it and both natives
not — which is the divergence this document exists to remove, and the
question of which behaviour to keep is the maintainer's rather than whichever
client happened to keep it.

Since [ADR-0062](../adr/0062-conversation-kind.md) this is a rule of the
conversation, not only of the screen. Whether a conversation is a one-to-one
or a group is set when it is created and never changes, and it is read from
the conversation itself, never from how many people are in it; a two-person
group is a group on every member's device. A one-to-one is always the same
two people: every member's device refuses a change that would add or remove a
person, and nobody can leave it (`Delete this chat` removes it from this
device only). Each person's own devices still join and leave as usual.

What a person sees when a peer's device refuses: nothing, because no screen
offers the action - there is no add, remove, invite link, role or `Leave` on
a one-to-one. An invite-link request to join a one-to-one never appears on
either side.

### 2.2 A group

- Name, and the members' names when it has none.
- How many people.
- Members, each with their role shown where they have one, and add or remove
  for whoever may ([ADR-0022](../adr/0022-group-admin-roles.md)). Adding is
  **Add members** at the top of the members list (section 2.4).
- Rename, for the owner and admins; read-only for everyone else
  ([ADR-0051](../adr/0051-group-and-channel-names.md)).
- Invite link, for whoever may mint one
  ([ADR-0028](../adr/0028-group-join-ticket.md)).
- Transfer ownership, for the owner.
- Leave group.

This is what exists today, minus its habit of appearing where there is no
group.

Tapping a member who is a contact opens their contact screen, which returns
to the group's info by its back control (section 3.1).

### 2.3 A channel

- Name, `@handle`, description.
- Subscriber count where the client has it.
- Unsubscribe, for a subscriber and not for a publisher: a publisher reads
  its own channel from its own keys, so there is no subscription to end and
  a control offering to end one would end nothing.
- For a publisher: the owner's screen it already has.

**What `Unsubscribe` actually does, said plainly because the copy has to be
true.** It takes the channel off this device's list and drops the subscriber
keys, so the device stops reading it and can subscribe again later. It does
not delete the registration on the server - no channels route retires a
subscriber - so the copy says what happens to *this device* and never claims
the channel has been told.

### 2.4 Adding members (decided 2026-10-29)

Seen live 2026-10-29: on the web the only way to add someone to a group was
an `Add member` item in the chat's overflow menu, which opened the group info
with a field for a full `@username`; the info itself offered only `Create
invite link`. A person had to know the exact username even of a contact.

**Entry.** The first row of the members list, for whoever may add (the owner
and admins; anyone in a group with no role table): a person-plus icon and
**Add members**. It is the only way in: the overflow menu item and the typed
`@username` form go.

**The Add members screen.** Pushed over the group's info, like any screen in
this spec (section 3), with the back control returning to the info.

- Top bar: back icon, the title **Add members**, and at the trailing end
  **Add** (with the count once more than one is picked: **Add 3**), disabled
  until somebody is picked.
- A search field under the bar, placeholder "Search contacts or people".
- Picked people as removable chips between the field and the list, so a pick
  made from a search result stays visible after the search text changes.
- The list: the person's contacts, each with avatar, name, `@username` where
  known, and a checkbox. Contacts already in the group, or already asked for
  (an add waiting for the owner's or an admin's sweep), are not listed. A
  contact that cannot be added (no captured account id, `new-group.md`
  section 3) is listed dimmed with its line.
- Typing filters the contacts from the first letter, loosely, on the device.
  From 3 characters it also searches the network exactly as the search field
  of [`who-can-find-me.md`](who-can-find-me.md) section 6.1 does (500 ms after
  the last keystroke, by username and by name), and shows the results under
  the heading **People**, each with a checkbox instead of Add contact. A
  result that is already a member, or a contact already listed above, is not
  shown twice. The People states (searching, not found, network failure, out
  of searches) are that section's, word for word.

Adding a person who is not a contact is allowed exactly when the network
search found them: finding someone is the permission to reach them
(`who-can-find-me.md` section 12). Being added to a group does not make two
people contacts.

**Add.** The screen closes back to the group's info at once. Each pick is
added through the conversation's normal add path, one after another. Each
new person shows in the members list's **Waiting to be added** section until
the change lands, then as a member
([`proposal-based-membership.md`](proposal-based-membership.md) section 5.2),
and no notice is shown, in effect or not (section 2 of that document). A
pick that could not be added at all is reported once, in place, in the
group's info: "Could not add {names}. Try again." The others still land.

## 3. It is a screen with a way back

**Amended 2026-10-29 (maintainer):** a group's info is a screen, not a layer.
Until then it opened as a centred modal on the web and as a sheet or a dialog
on the natives, which made it the one info that did not look like the
others: a person's info is the contact screen, a channel's info is the
channel screen, and both are screens.

A group's info opens the way the contact screen does: it takes the place of
the conversation, with a **back icon** at the leading end of its top bar that
returns to the conversation as it was left (scroll position, the draft in the
composer). The conversation is not closed while its info is shown; the info
is a step forward from it, not a different place.

**Per platform** - the idiom of a pushed screen: web the info takes the
conversation's column, with a back icon button in its header and a history
entry so the browser's Back does the same thing, and `Esc` too; iOS a push on
the navigation stack with the standard back button; Android a screen with a
top app bar whose navigation icon is the back arrow, and the system back
does the same.

### 3.1 The back control, everywhere this spec opens something

Every screen this spec opens - a group's info, a channel's info, a contact
screen, the Add members screen - shows the back icon whenever it was opened
from another screen it now covers, and the icon returns there: to the
conversation, to the group's info a member was tapped in, to the search a
result was opened from. Seen live 2026-10-29: the contact screen opened from
a chat had no way back on the web except the sidebar.

The one place it has no back icon is where nothing is covered: a contact
opened from the Contacts list on a wide web layout, where the list stays
beside it. On a narrow web layout the same screen covers the list and shows
the icon.

## 4. States

| State | What it shows |
|---|---|
| Default | The header, the actions, the detail. |
| A group still loading its roster | The header and what is known; never an empty list presented as "no members". |
| Membership ended | The info opens and is read-only: the conversation is over, and the record of who was in it is not ([ADR-0045](../adr/0045-remote-commit-epoch-fork.md) UX 6.2). `Call` goes, because the per-call key comes from a group this device is no longer in. `Delete this chat` arrives in its place - **on a one-to-one exactly as on a group**, because it is the one thing the reader still can do, and because the control used to live in the group panel that a one-to-one has just stopped opening. Everything about the PERSON is unaffected: the conversation ended, they did not. |
| An action refused | One sentence on what happened and one on what to do, in place, and the screen stays open. |
| Some picks could not be added | "Could not add {names}. Try again." in the group's info; the others are added (section 2.4). |

## 5. What the data has to provide

Nothing new. Every client already knows a conversation's kind - it is what
routes a tap in the merged list ([`navigation.md`](navigation.md) section
2.1) - and already has all three screens. What changes is which one the
header opens and how it is presented.

Adding members (section 2.4) needs, for every pick, what the add path takes:
the person's account id and signature key. A contact record carries both; a
network search result has to carry them too, so a person found there is
added without a second lookup.

## 6. One model, three implementations

One entry point, three destinations chosen by the conversation's kind, one
common shape, and a screen pushed from the conversation with a way back. The
**Per platform** notes are the only differences.

**And the model is one copy, in Rust.** `lakko_ui_rules::info` decides the
destination and the blocks; every client asks it over its FFI and renders
what it is handed ([`navigation.md`](navigation.md) section 8 says where the
shared rules live and why).

That is not tidiness. The first implementation pass wrote this section three
times - Android as enum cases, iOS as a struct of booleans, web as an array
of strings - and the three disagreed on three separate points, every one of
them a sentence of this document rather than a platform difference:

- `Remove contact` existed on web and Android, and iOS carried a comment
  asserting no client had one.
- `Unsubscribe` existed on web and on neither native, whose comments
  asserted the operation did not exist - while both already held the two
  functions it is made of.
- `Delete this chat` reached an ended one-to-one only on Android, so on the
  other two a one-to-one that ended could never be closed.

All three were found by a person opening two phones side by side, which is
the cost this spec exists to stop paying.
