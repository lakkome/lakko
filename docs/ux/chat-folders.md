<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Chat folders

Date: 2026-09-28. Scope: web, iOS, Android. Status: accepted, not built.
Wire and sync: [ADR-0058](../adr/0058-chat-folders.md).

Answers the item [`navigation.md`](navigation.md) section 7 left open:
"Folders / custom filters. The merged list is the point; a filter mechanism on
top of it is a separate decision." This is that decision. The merged list stays
the home and stays exactly as specified there; a folder is a saved **view** of
it that the person made, never a second place a conversation has to live.

**How to read this.** As in `navigation.md`: every requirement is for all three
clients, and where a platform must differ it says so inline, marked **Per
platform**. What is built lives in [`PRODUCT-READINESS.md`](../PRODUCT-READINESS.md),
not here.

## 0. Why, and what it must not become

People with many conversations sort them by their own logic - work, family,
one hobby group and its three channels. The merged list sorts by *when*, which
is right for "what just happened" and wrong for "show me only work". A folder
answers the second without taking anything from the first.

Three things a folder is not:

- **Not a move.** A conversation in a folder is still in **All chats**. Nothing
  disappears from the home by being put in a folder. (Archive is the only
  thing that takes a conversation out of the way, and it keeps doing that.)
- **Not shared.** Folders are the person's own arrangement. Nobody else in a
  group learns that it sits in "Work", and Lakko's servers learn nothing at
  all - the folder set travels only between the person's own devices,
  end-to-end encrypted, and in their phrase-sealed backup.
- **Not per device.** A folder made on the phone is on the laptop. A person who
  has to rebuild folders on every device stops using them.

## 1. Entry points

1. **The folder bar** above the conversation list (section 2): tap a folder to
   see it; its menu edits it.
2. **A conversation row's menu** (right-click / long press, the same menu that
   holds Pin, Mute, Archive): **Add to folder** and, inside a folder,
   **Remove from folder**.
3. **Settings > Chat folders** (section 4): the full list, create, edit,
   reorder, delete, and the suggested folders.

The folder bar does not exist until the person has at least one folder. A
person who never makes one never sees any of this except the Settings row and
the "Add to folder" item.

## 2. The folder bar

### 2.1 Shape

A single horizontal strip of tabs between the search field and the list:

`All chats` · `Work 3` · `Family` · `Channels 12` · ...

- **All chats** is always first and cannot be edited, moved or deleted. It is
  the merged list of `navigation.md` section 2, unchanged.
- The person's folders follow in the person's order.
- Each tab shows the folder name and, when non-zero, a count: the number of
  **conversations** in that folder with anything unread, not counting muted
  ones. Conversations, not messages - "3" means three places to look, which is
  what a person uses a folder count for; a message count grows without bound
  in one busy group and says nothing. **All chats** carries no count (the
  list itself is the count).
- The selected tab is the accent label with an accent underline; the others
  are muted. No pill fills, no icons in v1.
- The strip scrolls horizontally when it overflows; the selected tab is
  scrolled into view.

**Per platform.**
- Web: the strip sits in the list column on every layout (narrow and wide).
  Click to select; the tab's menu is a right-click. `Ctrl+1`..`Ctrl+9`
  (`Cmd` on macOS) select the first nine tabs, `All chats` being `1`.
- iOS: the strip sits under the navigation title, above `.searchable`'s list.
  Tap to select; long press opens the tab menu. The list pages sideways
  between folders with a horizontal swipe, and the strip follows.
- Android: a `ScrollableTabRow` under the top app bar; the list is a
  `HorizontalPager` in step with it. Long press opens the tab menu.

Swiping between folders is a native idiom and web does not imitate it; the
tabs are the same tabs.

### 2.2 The tab menu

On a custom folder, in this order:

1. **Edit folder** - opens the editor (section 3).
2. **Mark all as read** - only when the tab's count is non-zero.
3. **Reorder folders** - opens Settings > Chat folders in its reorder state.
4. **Delete folder** - confirmation (section 3.5).

On **All chats**: **Mark all as read** (when anything is unread) and **Edit
folders** (opens Settings > Chat folders). Nothing else.

### 2.3 What the list shows inside a folder

The same rows, the same row menu, the same ordering rule as `navigation.md`
section 2.2, applied to the conversations the folder matches (section 3.2),
with one difference: **pins inside a folder are the folder's own.** Pinning in
"Work" puts a chat at the top of "Work" and nowhere else; the All-chats pin is
unaffected and does not carry into a folder. (A person who pins the team chat
in Work and a family chat in All chats wants both, and one global pin cannot
express that.) The row menu's **Pin** / **Unpin** therefore acts on the pin set
of the tab it was opened from. A folder's pins are reordered by drag and by
Move up / Move down exactly as the pins of All chats are
([`navigation.md`](navigation.md) section 2.2.1, 2026-11-25), and the order
written is the folder's own pin list; the global pin order is untouched.

Archived conversations appear inside a folder only if the folder says so
(section 3.2, "Exclude archived" is on by default). When they appear, they sort
as in All chats: below a divider.

### 2.4 States inside a folder

- **Default** - the rows.
- **Empty because nothing matches right now** (for example "Unread" when all is
  read): one muted line, "Nothing here right now", and a secondary button
  **Edit folder**. Not a different sentence per filter - the person made the
  filter and knows what it is for.
- **Empty because the folder names conversations this device does not hold
  yet** (a folder that arrived from another device before the chats did):
  the same empty state. A conversation this device cannot open is never shown
  as a placeholder row; it appears when the chat itself arrives. The "Synchronizing..."
  label of [`second-device.md`](second-device.md) already covers "your chats are
  still coming".
- **Search**: typing in the search field hides the folder bar and searches
  **everything**, as today. A search is "where is it", and the answer must not
  depend on which tab happened to be open. Clearing the search restores the bar
  and the tab that was selected.

### 2.5 Which tab opens

The app opens on the tab that was last selected **on this device**. The
selected tab is a per-device fact, not synced: a phone left on "Family" should
not flip the laptop to "Family". If that folder has since been deleted (on any
device), the app opens on All chats.

## 3. The folder editor

One screen, reached from Edit folder, from Settings > Chat folders, and from
**New folder...** at the bottom of the "Add to folder" list.

### 3.1 Layout, top to bottom

1. **Name** - a text field, required. Placeholder "Folder name".
2. **Included** - a section titled "Included":
   - three kind toggles as rows with a checkmark: **One-to-one chats**,
     **Groups**, **Channels**;
   - then the conversations added by hand, each a normal compact row with a
     trailing remove control;
   - a row **Add chats** that opens the picker (3.3).
3. **Excluded** - a section titled "Excluded":
   - three toggles: **Muted**, **Read**, **Archived** (Archived on by default
     for a new folder, the other two off);
   - the conversations excluded by hand, same compact rows;
   - a row **Exclude chats** that opens the picker.
4. For an existing folder, at the bottom and visually separated: **Delete
   folder** (danger text).

The screen's save control is **Save** (web: primary button at the bottom;
iOS: toolbar `Save`; Android: top-bar action `Save`). **Save** is disabled
while the name is empty or while nothing is included. Leaving with unsaved
changes asks "Discard changes?" with **Discard** / **Keep editing**.

A single line of muted text under the Included header states the live match
count once the folder has a rule: "12 chats". Nothing else explains the rule
- the toggles are the explanation.

### 3.2 What a folder matches

A conversation is in a folder when:

1. it is **included** - its kind is toggled on, or it was added by hand; and
2. it is **not excluded** - it was not excluded by hand, and it is not muted
   while Muted is on, not fully read while Read is on, not archived while
   Archived is on.

Excluded by hand beats included by hand. That is the one precedence rule, and
it lets "All channels except these two" be built in two steps.

A conversation the person left, or a channel they unsubscribed from, matches
nothing: it is not in the list at all (`navigation.md` 2.1).

"Read" means the same thing the unread badge means: nothing unread in it and
no manual "mark as unread" on it.

### 3.3 The chat picker

A full-screen picker (sheet on iOS, full-screen dialog on Android, modal on
web) with the search field on top and the merged conversation list below, each
row with a checkbox. The rows already in the set are checked. **Done** returns
to the editor. The same picker serves Add chats and Exclude chats; its title
says which.

### 3.4 Limits, and what the person sees at them

Chosen, not measured - they bound the size of what travels between devices and
can be raised later without a wire change:

| limit | value | at the limit |
|---|---|---|
| folders per account | 20 | Settings "New folder" and the row menu's "New folder..." are disabled with the helper line "You can have up to 20 folders." |
| name | 1-24 characters | the field stops accepting input; no error text |
| chats added by hand | 200 per folder | picker checkboxes of unchecked rows disable; one line at the top: "A folder can hold up to 200 chats added one by one. Use the kind toggles for more." |
| chats excluded by hand | 200 per folder | the same, with "excluded" |
| pins inside a folder | 10 | Pin in the row menu is disabled inside that folder with "You can pin up to 10 chats in a folder." |

The name follows the display-name rule already used for profiles and group
names (letters, digits, punctuation, spaces and emoji; no line breaks, no
invisible formatting characters).

### 3.5 Deleting

"Delete folder "Work"?" / "The chats in it stay in All chats." / **Cancel**
(ghost) / **Delete** (danger). The folder disappears from every device. Nothing
happens to any conversation.

## 4. Settings > Chat folders

A new Settings section, between Notifications and Appearance.

- **Your folders**: the folders in bar order, each a row with the name and a
  muted "12 chats" match count; tapping opens the editor. **Per platform**:
  drag handles on iOS (`.onMove` in edit mode, the `Edit` toolbar button) and
  Android (long-press drag in the list), and on web a drag handle plus
  **Move up** / **Move down** in each row's menu for keyboard users. All
  chats is shown as the first, fixed row with no handle.
- **New folder** - opens an empty editor.
- **Suggested**: shown only for suggestions the person does not already have
  (compared by rule, not by name). Each is a row with the name, a one-line
  description and an **Add** button that creates it as is:
  - **Unread** - "Chats with new messages" (all three kinds, exclude Read);
  - **Personal** - "One-to-one chats" (one-to-one only);
  - **Groups** - "Group chats" (groups only);
  - **Channels** - "Channels you follow" (channels only).
  A suggestion that the person adds disappears from Suggested; the created
  folder is an ordinary folder they can edit.
- **Empty state** (no folders): the Suggested list is the content, above it one
  line: "Group your chats the way you think about them. Folders appear on all
  your devices."

This is the one place the sync is mentioned. There is no per-folder "synced"
indicator: a folder is always on every device, and a badge that is always
true says nothing.

## 5. Row menu: Add to folder

On every conversation row, after Archive:

- **Add to folder** opens a small list of the person's folders, each with a
  checkmark if this conversation is currently **in** it (by any rule), then
  **New folder...**.
  - Tapping an unchecked folder adds the conversation by hand, replacing a
    hand exclusion if there was one. (Only removing the exclusion would leave
    a chat no kind toggle covers still outside the folder.)
  - Tapping a checked folder where the conversation is in **by hand** removes
    it.
  - Tapping a checked folder where the conversation is in **by a kind
    toggle** excludes it by hand. (The person asked to take it out; how the
    folder was built is not their problem.)
  - **New folder...** opens the editor with this conversation already added.
- Inside a folder, the row menu also carries **Remove from folder**, which is
  the same as tapping that folder's checkmark.

With no folders yet, **Add to folder** goes straight to the editor, as
**New folder...** would.

A new pin inside a folder goes to the top of that folder's pins.

**Per platform.** The folder list is a submenu on the web right-click menu
and on iOS's context menu (`Menu` inside the context menu), and a second
dropdown step on Android's long-press menu. Same items, same order.

## 6. Across devices

What the person experiences, and nothing more:

- Any change - create, rename, reorder, add or remove a chat, delete - appears
  on the person's other devices the next time each of them syncs with the
  others. On an open app that is seconds; on a phone that was off, when it
  opens.
- A new device linked to the account gets the folders when it finishes
  linking, together with the chats.
- An account restored with no other device alive gets the folders from its
  backup.
- Edits made on two devices at nearly the same moment are combined, not
  overwritten: a chat added to "Work" on the phone and another added on the
  laptop are both in "Work". Only a change to the **same thing** - the same
  chat's place in the same folder, the folder's name, its toggles, its pin
  order, the folder order - is decided by whichever edit was later. Deleting a
  folder is final: an edit made to it on another device at the same moment is
  dropped, and the folder does not come back. There is no conflict prompt.
- Pins, mutes, archive, drafts and what has been read reach a newly linked or
  restored device the same way folders do.
- If an edit cannot be sent because this device is offline, it applies here
  at once and travels later. Settings > Chat folders shows one quiet line
  while anything is waiting: "Changes will reach your other devices when this
  device is back online." Nothing is shown on the folder bar.

There is no failure state that loses a folder. A device that cannot open a
sync message from another device (it was offline across a security change)
keeps its own folders, and the next edit from any device brings all devices to
the same folder set again.

## 7. What the data has to provide

This is the list the ADR and the shared rules must satisfy.

1. **A conversation reference that covers channels.** Pins, mutes and archive
   are keyed on a group id a channel does not carry (`navigation.md` 6.1). A
   folder that cannot name a channel cannot hold "the three channels of my
   hobby", so every hand-added or hand-excluded entry is a (kind, id) pair:
   one-to-one and group by group id, channel by channel id.
2. **A stable folder id**, minted once by the device that creates the folder,
   so a rename is not a delete plus a create and two devices can refer to the
   same folder.
3. **Per folder:** name, the three kind toggles, the three exclude toggles,
   the hand-added set, the hand-excluded set and the folder's own ordered pin
   list - each part stamped separately, so edits to different parts on two
   devices combine instead of overwriting each other.
4. **The folder order**, separately from the folders, so reordering does not
   rewrite every folder.
5. **A deletion that is remembered**, so a device that was offline and still
   holds the folder does not bring it back by re-sending it.
6. **Every row input the rule reads**: kind, archived, muted, has-unread
   (unread count or manual unread), and the pinned state of the current view.
   Muted and unread are not in today's `ConversationRow`.
7. **The matching rule and the per-folder ordering written once**, in
   `lakko-ui-rules` beside `order_conversations`, so three clients cannot
   disagree about which chats are in "Work" - the exact drift that shared rule
   crate exists to prevent.
8. **Per device, not synced:** the selected tab.
9. **A pending-sync flag** per device for the one line in section 6.
10. **References this device cannot resolve are kept, never dropped.** A
    device that does not yet hold one of a folder's chats keeps the reference
    and never writes it away; otherwise a half-synced device would silently
    delete it from the folder on every other device.

## 8. Copy

| where | text |
|---|---|
| fixed first tab | All chats |
| Settings section | Chat folders |
| row menu | Add to folder · Remove from folder · New folder... |
| tab menu | Edit folder · Mark all as read · Reorder folders · Delete folder · Edit folders |
| editor | Folder name · Included · One-to-one chats · Groups · Channels · Add chats · Excluded · Muted · Read · Archived · Exclude chats · Save |
| editor, count | 12 chats |
| empty folder | Nothing here right now · Edit folder |
| delete dialog | Delete folder "Work"? · The chats in it stay in All chats. · Cancel · Delete |
| unsaved | Discard changes? · Discard · Keep editing |
| offline | Changes will reach your other devices when this device is back online. |
| Settings empty | Group your chats the way you think about them. Folders appear on all your devices. |

No word about encryption or sync mechanics appears anywhere in
this feature beyond "on all your devices".

## 9. Accessibility

- The folder bar is a tab list (`role="tablist"` / `.accessibilityAddTraits(.isTabBar)`
  equivalent / Compose `Tab` semantics); each tab's label reads "Work, 3
  unread chats".
- Every drag-to-reorder has a non-drag equivalent (Move up / Move down on web,
  the system reorder actions of VoiceOver and TalkBack on the natives).
  This covers the folder order here and the order of pinned conversations in
  All chats and inside a folder ([`navigation.md`](navigation.md) section
  2.2.1).
- The kind and exclude toggles are real checkable controls, not tinted text.

## 10. Not in this spec

- **Folder icons.** Tabs carry names only. Icons matter in a vertical folder
  sidebar, which Lakko does not have; the wire leaves room for one later.
- **Sharing a folder** as a link (a bundle of chats someone else can join).
  It would reveal a set of conversations to another person and is a separate
  decision.
- **Folders that match message content or people** ("chats with Anna").
- **Moving All chats**, or hiding it. It is the home.
