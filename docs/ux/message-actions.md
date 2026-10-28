<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Message actions

What a person can do to one message, what the menu that offers it looks like,
and what each action needs that does not exist yet.

This is the UX spec. The gesture that opens the menu is settled in
[`client-redesign.md`](client-redesign.md) and is not restated here: a
right-click on web, a long press on iOS and Android, never a hover, reachable
from anywhere on the bubble including over its own controls, and once the menu
is open the control underneath receives nothing more from that press.

## 1. Why this exists

The menu shipped with three actions - React, Edit, Delete - while
`client-redesign.md` had been promising four. Reply and Copy were named in the
spec and built by nobody. On iOS the one action available on someone else's
message, React, sits behind a disclosure row (`React >`) that opens a submenu,
so the commonest thing a person does to a message costs two taps and a
navigation. On web the action rows are stretched to the width of the reaction
row above them, because both live in one card.

So this document settles two different things that were being confused: what
the menu CONTAINS, and what it LOOKS LIKE.

## 2. The shape

**The reactions are not a menu item.** A row of emoji sits above the bubble and
each one is one tap away, with a trailing control that opens the full picker.
A person reacting is the commonest use of this menu by a wide margin and it must
cost the least. Putting them behind a row that opens a submenu is the defect
this rule exists to forbid.

**The action card is sized by its own content.** It is a separate surface from
the reaction row and must not inherit its width. On a phone it sits under the
bubble, aligned to the bubble's own side; on web it sits where the pointer
opened it.

**The rest of the screen recedes.** While the menu is open the conversation
behind it is dimmed or blurred, and the message being acted on stays fully
legible in place. Without this the card reads as a dropdown that has landed on
top of the next message - which is what it currently does on iOS - and the
person loses track of which message they are acting on.

**Two surfaces that read as two things.** The reaction row and the action card
are separate floating surfaces - each with its own background, border and
shadow, and a visible gap between them - on every client, web included. Sizing
the two children of one wrapper separately is not enough: if they share an
outline or touch, a person reads one panel with an odd top row. The maintainer
asked for this on web by name (2026-09-25), having seen the natives.

**The card's look is shared, and web's is the reference.** The maintainer chose
web's card by eye, so its values are the ones the natives adopt rather than each
platform's own dropdown chrome: corner radius 12, an inner padding of 4 around
the rows, a 1-point border in the surface-border colour on the elevated
surface, a soft shadow (0 8 24 at about 12 percent), rows padded 7 vertical by
8 horizontal with a 6 radius and the leading icon 8 from the label, and the
destructive group set off by a 1-point rule with 2 of padding above it. Where a
platform token already carries one of these values it is used by name; the
numbers are here so that "the same card" is checkable rather than felt.

**Order and grouping.** Actions that produce a new message first (Reply,
Forward), then the ones that only read (Copy, Select), then the ones that change
this message (Pin, Edit), then the destructive one, separated and in the
platform's destructive colour. Never a row whose only purpose is to open another
row of things.

## 3. The actions

| Action | Offered on | What it does |
|---|---|---|
| React | any message with an id | Toggles one emoji. Already built; what changes is that it is a visible row, not a submenu |
| Reply | any message | Composes a new message that quotes this one |
| Copy | any message with text | Puts the text on the clipboard. Nothing leaves the device |
| Translate | someone else's message with text, when this device can translate it | Shows a translation in place, made on this device ([`translation-and-transcription.md`](translation-and-transcription.md) section 3) |
| Forward | any message | Sends the same content to another conversation |
| Pin | any message, with a choice - see §4 | Keeps the message reachable at the top of the conversation |
| Edit | own, not deleted | Already built |
| Delete | own, not deleted | Already built |
| Select | any message | Enters a multi-select mode, from which Delete and Forward act on the set |
| Read by | own, in a group | Shows who has read it. The watermarks already exist (ADR-0024); nothing renders them |

`Select` is listed because it is what makes Delete and Forward bearable in bulk,
and because building either of those without it means building their
one-message form twice.

### 3.1 Reply: the strip and the quote

Reply is the first of the new-message actions to be built (ADR-0052 decision 1);
this is the flow the three clients implement.

**Choosing Reply** puts a strip above the composer and focuses the composer.
The strip is one line: a vertical rule in the accent colour, the quoted
author's name, then the excerpt, and a close control at the trailing end. The
name is whatever the timeline already calls that person (contact name, profile
name, the fingerprint fallback, "You" for an own message). The excerpt is the
message's text, trimmed to one line; a message with no text shows the kind of
its first attachment instead ("Voice message", "Video message", "Photo",
"Video", "GIF", "File"). The strip is what the recipient will see, and it is
taken now: both strings are snapshotted when Reply is chosen and sent as they
were, so a later edit or deletion of the original changes nothing.

**One thing at a time.** Replying and editing are exclusive: choosing Reply
while an edit is open ends the edit (the draft is discarded, as Cancel would),
and choosing Edit while a reply is armed drops the reply. Choosing Reply on
another message replaces the strip. The close control, Escape on web, and a
sent message all clear it. Recording a voice message with a reply armed sends
the take as the reply; the strip clears the same way. Switching conversation
clears the strip; a reply is not a draft that follows the person.

**The quote in the bubble.** A reply renders a quote block above its own text,
inside the bubble: the same rule, the name on the first line, the excerpt on the
second, both single-line and truncated, in the bubble's own colours. Own and
incoming replies both show it. The block is the snapshot the envelope carries
and never re-reads the original, so it is drawn exactly the same when the
original is edited, deleted, or not on this device at all. A reply that arrives
with no snapshot at all (only the target id) draws the block with the name line
empty and the excerpt "Message".

**Tap to jump.** Tapping or clicking the quote block scrolls the timeline to
the quoted message, which is highlighted briefly (about a second and a half,
the surface tint used for the pressed state) and then returns to normal. The
target is found by its message id in the timeline this device holds; when it is
not there (older than what is loaded, or never received here), the tap does
nothing and nothing is said - a quote is not a promise that the original
exists. The block is a button for accessibility purposes on every platform,
named "Reply to <name>".

**Reply to a reply.** The strip and the block quote the message that was chosen,
one level deep: its own text, never the text it quoted. Nothing about the chain
is carried or shown.

## 4. Pin: the person pinning chooses who it is pinned for

A pin is offered as a choice between **pin for me** and **pin for everyone**,
made at the moment of pinning. They are different features that happen to share
a word:

- **For me** is a flag on this device. Nothing is sent, nobody else sees it, and
  it survives exactly as long as this device's storage does.
- **For everyone** is group state: it travels to every member, everyone sees the
  same pinned message, and it therefore needs an authority rule (who may pin for
  everyone), an ordering rule (two people pinning at once), and an answer to who
  may unpin what somebody else pinned.

The choice is the maintainer's decision and it is the more expensive of the
three possible answers, deliberately: offering only the shared pin makes a
private bookmark impossible, and offering only the private one makes "look at
this" impossible. The UI must make which one happened obvious afterwards - a
message pinned only for me must not look like one the whole group can see.

**The pinned bar** (the maintainer's shape, 2026-09-26: several pins, one shown
at a time, with a way to move between them). When a conversation has at least
one pin, a bar sits under the header showing ONE pinned message - author and
excerpt - the newest first, with a thin stacked indicator beside it that says
how many there are and which one this is, the way Telegram's does. Tapping the
bar scrolls the conversation to the message it shows and advances the bar to
the next older pin; past the oldest it wraps to the newest. That single gesture
is the navigation: every pin is reachable by tapping the bar repeatedly, and
nothing else needs to exist for the common case. A second affordance on the bar
opens all pins as a list, each row jumping to its message, for the person who
wants to scan rather than cycle. Shared pins come before personal ones in the
order, since a shared pin outranks a personal one (above). A pinned message
that is later deleted leaves the bar; it is not shown as a tombstone.

The authority and ordering rules for the shared pin are **not settled here**.
They belong in the ADR, and the lesson from ADR-0042 through ADR-0044 applies
directly: *who may write* and *which write survives* are two different questions,
and answering only the first leaves the second to arrival order.

### 4.1 Pin: the flow

The authority and ordering rules the section above left to the ADR are settled
there (ADR-0052 decision 3, closed 2026-09-26); this section is what a person
sees, in the order they see it.

**The row.** `Pin` sits in the action card in section 2's slot - after the
reading actions Copy and Select, before Edit (it used to read "after Forward"
here, from before Select was built; section 2's order is the one the clients
follow) - on any message whose send is complete and that is not deleted. On a message that is already
pinned in one scope the row reads `Unpin`; on one pinned in both it still reads
`Unpin` and the sheet below asks which.

**The sheet.** Tapping `Pin` opens a two-row choice:

- `Pin for me` - always offered.
- `Pin for everyone` - offered only when this device may set a shared pin:
  the owner or an admin of a group's role table, any member of a group with no
  table, either participant of a one-to-one. In a one-to-one the row reads
  `Pin for both`. A plain group member sees a one-row sheet and no explanation;
  the missing row is the explanation, as it is for `Add members`.

Under each row one line says what it does: "Only on this device" and "Every
member sees it". `Unpin` on a message pinned in both scopes offers `Unpin for
me` and `Unpin for everyone` (`Unpin for both`); in one scope it acts at once.
Unpinning a shared pin needs the same standing as setting one, so a plain
member holding a message pinned by an admin sees `Unpin for me` only, and only
when they also pinned it for themselves; otherwise the row is `Pin` and the
sheet offers `Pin for me` alone.

**The bar.** Section 4 describes it: under the header, one pin at a time,
author and excerpt on one line, newest first, shared before personal; tap =
scroll to that message with the short highlight the reply jump uses, then
advance to the next older pin, wrapping past the oldest; a stacked indicator
beside the excerpt shows how many and which; a `list` control at the far end
opens every pin as rows, each row jumping to its message and closing the list.
A personal pin carries the label `For you` in place of the author, in the bar
and in the list, so a message pinned only for me never looks like one the room
can see. The excerpt is the message's text, or the same one-word stand-in the
reply quote uses for a message without text (`Photo`, `Voice message`, `GIF`,
`File`). A pinned message this device does not hold (not yet downloaded,
purged) is not shown in the bar until it is; the pin itself is kept. A pinned
message that is deleted leaves the bar, shared or personal, and its shared
pin record is dropped with the message.

**When it is empty.** No pins, no bar. The header does not move.

**What travels** (shared only). One envelope per action - pin or unpin of ONE
message, stamped with the sender's clock - sent as an ordinary message on the
conversation's journal and never shown as a bubble. The sender applies it
locally the same way a receiver would, since it never receives its own message
back. A re-send (the growth republish below) carries the ORIGINAL stamp.

**What a receiver does**, in this order and under the conversation's own
serializing lock, because the comparison is only meaningful against current
state:

1. Resolve the sender to the account key at receipt (C-68) and check standing:
   owner or admin of the live role table; any member when there is no table;
   either participant in a one-to-one. Refuse otherwise - drop, no error, no
   bubble.
2. Compare the stamp with the stored stamp FOR THAT MESSAGE (`pinIsNewer`,
   strictly greater). Equal or older is dropped: a re-send is not an update,
   and a late-arriving pin can never undo a later unpin.
3. Store `(messageId -> pinned?, stamp)`. An unpin is stored, not deleted:
   the stamp is what keeps a stale pin of the same message from coming back.

**Growth.** When the conversation grows, whoever may set shared pins re-sends
the current shared pins verbatim - stamp and all - through the same throttled
path the group name uses, so a newcomer sees the room's pins. Unpins are not
re-sent; a newcomer holds nothing to undo.

**Background.** A pin envelope is a service envelope: the web service worker,
the Android background decrypt and the iOS extension recognise it and show no
notification for it, as they do for a rename.

**Not in this build, stated rather than hidden:** the channel author's pin (a
channel post is a different carrier), and a shared pin in the search or the
conversation list preview. Both are unbuilt; neither is decided against.

**Data each client keeps.** Per device: a set of personal pins keyed on
`(conversation, messageId)`. Per conversation: the shared pin map
`messageId -> {pinned, updatedAtMs}`, under the conversation's lock, wiped
with the conversation. The bar is derived from the two and the held messages,
never stored.

## 5. Forward: the original author is a name and nothing more

A forwarded message carries the original author's **display name as
metadata**, and nothing else. No username, no account id, no key, no tap target,
no route to reach them. The name is a label on the message, not a reference to a
person.

Three consequences, all of which must be stated rather than discovered:

- **A recipient cannot reach the original author through a forward.** That is
  the point: forwarding a message must not hand the receiving conversation a
  way to contact somebody who never agreed to be contacted by them. Lakko's
  whole contact model is capability-based (ADR-0005), and a forward that carried
  a reachable identity would be a way around it.
- **The name is therefore not verifiable.** What cannot be reached cannot be
  checked. The forwarding client writes a string, and any client could write any
  string. It is a courtesy label, not a proof of authorship, and the UI must not
  dress it as one - no verification tick, no safety-number affordance, nothing
  that implies the name was confirmed.
- **It is a snapshot.** The name is captured when the message is forwarded. If
  the author later changes their profile name (ADR-0048), the forward keeps the
  old one, because there is nothing to look it up against.

A message forwarded onward keeps the ORIGINAL author's name rather than the name
of whoever forwarded it, so a chain does not rewrite its own history.

**Tapping the attribution goes to the original - when the device can.** The
maintainer asked for Telegram's behaviour with one condition, that the server
learn nothing about the link. So the forward carries, inside its encrypted
payload, a reference to the original message, and a recipient who is also in
that conversation is taken to it on tap. A recipient who is not sees the name
only; tapping does nothing and does not explain, because explaining would
confirm that the conversation exists. The reference points at a message, never
at a person - the author stays unreachable as above.

### 5.1 Forward: the flow

**Choosing Forward** opens a conversation picker: the same merged list the
Chats tab shows (one-to-one chats and groups this device is a live member of,
pinned first, newest next, with the search field), one row per conversation,
no multi-select in this wave. Picking a row sends at once - no comment field,
no confirmation - and takes the person to that conversation, where the
forwarded message is the newest bubble. Cancel (Back, Escape, the backdrop)
sends nothing.

**What is sent** is a new message from the forwarder: the original's text
verbatim (the edited text if it was edited), its attachments re-encrypted
under fresh keys and uploaded again (decision 2's OQ1: the two conversations
never share a blob), the original author's display name as the forwarder's
timeline shows it, and - when the forwarder is in the origin conversation,
which a forwarder always is, and the origin is not a one-to-one (its only other
holder is the other participant; ADR-0052 amendment of 2026-09-27) - the origin
reference. A forwarded message's
quote (if it was a reply) is NOT carried; a forwarded forward carries the
ORIGINAL author's name and origin, never the forwarder's. Reactions are not
carried. A message that cannot be forwarded yet (its attachments still
uploading, or a failed send) does not offer Forward.

**The bubble.** A forwarded message draws one line above its content, inside
the bubble, in the muted text colour with a small forward arrow:
"Forwarded from <name>", or "Forwarded" when no name was carried. The name is
plain text - no avatar, no username, no verification mark, no link styling
(section 5: it is a label, not a person). The forwarder is still the message's
author for everything else: the sender name on the bubble, edit, delete, the
read state.

**Tap to jump.** Tapping the line takes the person to the original when this
device holds the origin conversation and the message in it: switch to that
conversation and highlight the message the way a reply's quote does. When
the device does not hold the conversation, or holds it but not the message,
the tap does nothing and says nothing (section 5: an explanation would confirm
the conversation exists). The line is a button for accessibility purposes on
every platform, named "Forwarded from <name>".

**Copy of the day.** "Forwarded from" is the only new string; the picker's
title is "Forward to".

## 6. Data the UI needs (input to the ADR)

What each unbuilt action requires. This section is the handover to the
architecture step; it is not a design.

- **Copy** - nothing. Clipboard only.
- **Reply** - an optional reference to the target message in the chat envelope
  (`lakko-attachments`), alongside the text rather than instead of it. Note the
  existing `edit_of` / `delete_of` / `react_to` fields are mutually exclusive
  operations and a reply is **not** one of them: a reply is an ordinary message
  that also points somewhere, so putting it in that set would make replying and
  writing text exclusive. The bubble needs the quoted author and an excerpt,
  which means either carrying them in the envelope or resolving the target
  locally and showing a placeholder when it is not held.
- **Forward** - an optional display-name string per §5, and a rule for
  re-sending content. Attachments are content-addressed with their key inside
  the reference, so forwarding the reference shares the same blob with the new
  conversation rather than re-uploading it; whether that is acceptable, or
  whether a forward should re-encrypt, is an ADR question with a real privacy
  answer either way.
- **Pin (for me)** - per-device storage keyed on conversation and message.
- **Pin (for everyone)** - a group envelope, an authority rule, an ordering rule
  and a conflict answer, per §4. Built: `lakko-contacts::pin` (one
  message per envelope, `pinIsNewer` on both FFI surfaces), §4.1.
- **Select** - no wire surface; a UI mode plus bulk forms of Delete and Forward.
- **Read by** - nothing new. `ADR-0024` watermarks are already received and
  already keyed on the sender's signature key; they are simply not rendered.

## 7. Cross-platform shape

One mental model, three implementations. The reaction row, the action card and
the receding backdrop are the same idea everywhere; the mechanics are not.

- **iOS** cannot express this with SwiftUI's own `.contextMenu`, which offers no
  way to put a horizontal reaction row above the lifted bubble. That is why the
  current menu hides reactions in a submenu. A Telegram-class menu here is a
  custom presentation: a long press, a blurred backdrop, the bubble drawn at its
  own position, the reaction row above and the card below.
- **Android** has the same shape available through a custom composable over a
  scrim; the platform's own dropdown has the same limitation as iOS's.
- **Web** opens at the pointer and OVER the message, the way Telegram's does
  (the maintainer put the two side by side, 2026-09-26): the card's near corner
  is where the pointer was - its left edge ordinarily, its right edge when the
  block would run off the right of the viewport - the reaction row floats above
  the card with the gap between them, or below it when there is no room above,
  and both surfaces share the edge that meets the pointer. Nothing moves the
  block except the viewport's edge; whether it covers the bubble is not a
  question the placement asks. An earlier rule kept the menu outside the
  bubble; it was the cure for a hover strip that stole the play button, and it
  put the menu somewhere off to the side on a wide screen. Reopening on the
  same message lands in the same place - the anchor is the pointer, not the
  previous menu.

Whatever differs, the ACTION SET must not: a message offers the same actions on
every client, or the menu becomes a thing people learn per device.

## 8. Select: Delete and Forward in bulk

Select is the row in the card that makes the two multiplied actions bearable
(section 3). It adds no wire surface: a selection is a UI mode, and what it
does at the end is the one-message Delete or Forward of sections 3 and 5.1
repeated over a set. Building it is what stops those two from being built a
second time.

**Entering.** `Select` in the action card enters selection mode with the
message it was opened on already selected. In this mode the conversation
header is replaced by a selection bar: a leading `Cancel`, the count in the
middle ("1 selected", "12 selected"), nothing trailing. The composer is
replaced by an action bar with two buttons, `Forward` and `Delete`, each with
its icon and label. The reaction row and the action card do not open while
selecting; a long press or right-click on a row toggles it like a tap.

**Selecting.** Every message row shows a selection mark at its leading edge -
an empty circle, filled with a check when the row is selected - and the whole
row is the tap target. Rows that cannot be acted on are not selectable and
show no mark: a date separator or a system row, a deleted message, a message
whose send has not completed or has failed (the Forward gate of section 5.1).
At most 100 messages can be selected; at the limit further taps do nothing and
the count reads "100 selected, the limit". The mark is a toggle for
accessibility purposes, named by the message's text or kind and stating
"Selected" or "Not selected".

**Delete.** Lakko has no "delete for me": a delete is the section 3 delete,
which is sent to everyone and which peers accept only from the message's
author (ADR-0023). So `Delete` is enabled when the selection is not empty and
every selected message is the person's own; when it holds someone else's
message the button is disabled and a helper line under the bar reads "Only
your own messages can be deleted." A single delete today asks nothing; a bulk
one is irreversible and multiplied, so it confirms: "Delete 12 messages?" with
a destructive `Delete` and `Cancel` ("Delete 1 message?" for one). On
confirmation the client sends one delete per message, oldest first, applying
each locally as the single delete does (the pin records go with it, section
4.1). On a failure it stops, keeps the unsent messages selected, and says
"Deleted 5 of 12." Full success leaves selection mode.

**Forward.** `Forward` is enabled when the selection is not empty. It opens the
picker of section 5.1, titled "Forward to", one target. Picking a row sends one
forwarded message per selected message, oldest first, each composed exactly as
section 5.1 composes a single forward (the edited text, the attachments
re-encrypted under fresh keys, the original author's name, the origin
reference; no quote, no reactions). On full success the client leaves selection
mode and takes the person to the target conversation, where the forwarded
messages are the newest bubbles in their original order. On a failure it stays
in the source conversation with the unsent messages still selected and says
"Forwarded 5 of 12." Cancel in the picker sends nothing and returns to the
selection as it was.

**Leaving.** `Cancel`, Back on Android, Escape on web, a completed action, or
navigating to another conversation all leave selection mode and clear the
selection. Nothing about a selection is persisted.

**Copy of the day.** "Select", "N selected", "100 selected, the limit", "Only
your own messages can be deleted.", "Delete N messages?" / "Delete 1 message?",
"Deleted K of N.", "Forwarded K of N.".

**Cross-platform shape.** The same mode everywhere: the selection bar in place
of the header, the action bar in place of the composer, the mark at the leading
edge of each row. Web adds Escape and the right-click toggle; iOS and Android
add the long-press toggle. The gates are the ones the single actions already
compute (own, deleted, send complete), read once for the whole selection.
