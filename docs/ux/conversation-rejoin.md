<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: a device that missed an update to a chat comes back

Owner: [ADR-0091](../adr/0091-conversation-rejoin.md) (Proposed). Status: with
the ADR, not built. Applies to web (and the desktop app, which runs it), iOS
and Android identically.

## 0. Why this exists

Seen live on 2026-11-18. The desktop app missed one change to a group's
membership on 2026-11-11 and from then on could read nothing in that group.
It still showed the group as live: the last message was a week old, nothing
new ever arrived, and every message the person typed failed to send. Nothing
on screen said why. Each client today skips what it cannot read and carries
on, so the device can never notice.

The maintainer's decision: the device notices by itself and comes back, with
no action from the person, on every client.

## 0.1 First, the device should not drop out (phase 1)

The maintainer's order (2026-11-18): dropping out is itself the defect, so
phase 1 of ADR-0091 keeps the device from skipping the change it failed to
apply: it tries again for a while instead of moving on. Phase 1 has no
screen. A retry that succeeds is invisible; one that gives up leaves the
device behind, and from there everything below applies (phase 2).

## 1. User goal and entry points

The person wants the chat to keep working on this device. They did nothing
to cause the problem and should not have to do anything to fix it. There is
no entry point: the device notices on its own while the app is open (it reads
the chat in the background of the app, as it does today) and asks to be
brought back.

The person meets this in three places:

- the chat itself (the note where the composer is, section 2.2);
- the chat list (nothing new, section 2.1);
- after the device is back, a line in the chat where messages are missing
  (section 2.4).

## 2. What the person sees

### 2.1 The chat list

No mark, no badge, no counter. The row keeps its last readable message,
time and unread count. A chat that cannot be read on this device for a while
is not news; the person learns it when they open the chat. (A badge on a row
would be a second state for the same thing, and would light up for chats the
person never opens.)

### 2.2 Inside the chat: the waiting note

While the device is out of date in a chat, the chat is **waiting**, the same
state [`conversation-backup.md`](conversation-backup.md) section 2.2 defines
for a device that has not joined yet. One state, two notes. Where the composer
would be:

> This device missed an update to this chat. It is coming back up to date by
> itself; you can read earlier messages meanwhile.

As in conversation-backup 2.2, what a waiting chat cannot do is absent rather
than disabled: no composer, no call buttons, no "Pin for everyone", no member
changes, no rename; the message menu offers Copy, Forward, Save and Pin for me.
Conversation info opens with what this device last knew (name, people) and
hides every control that would change the chat. Earlier messages stay readable
and searchable.

The note has a second line when the device knows who it is waiting for:

| Who can bring the device back | Second line |
|---|---|
| another of the person's devices that is in this chat | "Open Lakko on one of your other devices if this takes long." |
| a one-to-one: the other person | "This finishes when {name} next opens Lakko." |
| a group: its owner or an admin | "This finishes when the group's owner or an admin next opens Lakko." |

No second line in the first 30 seconds, since most of the time the device is
back before anyone reads it. No spinner, no progress bar, no countdown: the
device cannot know how long, and showing a time would be invented.

### 2.3 No time limit

Decided by the maintainer on 2026-11-18: the device waits to be brought back
for as long as it takes. The note never changes and there is no "Remove from
this device" offer; the person can still delete the chat on this device the
ordinary way.

If the account left the chat, or was removed, on another of its devices,
this device learns it from the account's own devices and shows "You left" /
"You were removed" exactly as today; it never waits.

### 2.4 Back up to date

The waiting note disappears and the composer comes back, with no message,
toast or sound. Where messages were sent while the device was out, one line
in the chat, styled as the date separators are:

> Messages sent between 11 Nov, 13:10 and 18 Nov, 14:02 can't be shown on
> this device.

- The two times are the first message the device could not read and the
  moment it came back. On the same day the date is shown once.
- The device that brings this one back sends the missed messages it holds
  (maintainer, 2026-11-18): the person's other device in the chat, or else
  the other person in a one-to-one or the owner's or admin's device in a
  group. They are shown in place, and the line covers only what none of them
  held (older messages the server no longer keeps, or messages not on that
  device). If nothing is missing, there is no line.
- The line is local to this device. It is not sent, it marks nothing as
  read, and it is not an unread message.

Messages the device received while waiting but could not read never appear
as "unreadable message" bubbles; they are covered by the line.

### 2.5 Notifications while waiting

A message arriving in a waiting chat cannot be read on this device. The
background notification, where one is shown today, says only "New message in
{chat}" (the existing generic wording), never a placeholder text. Waiting
does not add notifications of its own: the device does not tell the person it
fell behind until they open the chat.

### 2.6 Calls

A call offer in a waiting chat cannot be read on this device, so a call does
not ring here and a group call is not offered here; the person's other
devices ring as usual. Nothing is shown about it. Calls work again once the
device is back.

## 3. Controls and what each drives

| Control | Effect |
|---|---|
| Opening a waiting chat | reads earlier messages from this device; marks nothing as read for others |

Everything else is automatic: noticing, asking to be brought back, and coming
back.

## 4. Data the UI needs

Per chat, from the core and the client's own records:

- whether the chat is waiting on this device (a durable flag, survives a
  restart), and since when (for the 30-second rule);
- who can bring it back: another device of the account in the chat / the
  other person of a one-to-one (their display name) / the group's owner or an
  admin;
- the first message time this device could not read, and the time it came
  back, for the line of 2.4;
- whether missed messages were supplied by another device of the account, and
  the time range they cover.

The flag is local; nothing about it is synced to other devices or sent to a
server beyond the request to come back itself (ADR-0091).

## 5. Cross-platform shape

One model: the waiting state of conversation-backup 2.2 with a second note
text, and a date-separator-style line. Web and the desktop app use the
existing composer-replacing note element; iOS and Android the same note view
they use for a waiting chat. No platform-specific divergence beyond the
usual message-menu gesture.

## 6. Copy checklist

No protocol or component names ("update" stands for the missed change; no
"epoch", "commit", "group state", "MLS", "Welcome", "knock"). "Chat" in
user-facing text. Sentence case, no exclamation marks, no "by design". Every
string is a catalog key (ADR-0061); the note of 2.2 and the line of 2.4 are
marked for native review.

## 7. What the person never sees

- a "Rejoin" or "Fix" button: the device does it by itself;
- the device being removed and re-added as two events in the member list or
  in anyone's chat: the person stays in the chat throughout, and other
  members see nothing more than an ordinary device admission shows them
  today;
- a notice in other people's chats that someone's device fell behind.
