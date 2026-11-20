<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: a device that missed an update to a chat comes back

Owner: [ADR-0091](../adr/0091-conversation-rejoin.md) (Proposed). Status:
built on web (and the desktop app, which runs it), iOS and Android; sections
2.2 and 2.4 rewritten 2026-11-20 (maintainer, seen on an iPhone: an ordinary
person does not care WHY a message was not sent, only THAT it was not sent).
Applies to every client identically.

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

The person meets this in two places only, and neither explains anything:

- a message sent while the device is behind is shown not sent, as any other
  failed send is (section 2.2);
- after the device is back, a line in the chat where messages are missing
  (section 2.4).

Nothing anywhere names the machinery: no "missed update", no "coming back up
to date", no "other devices", no "sync", no "re-entry".

## 2. What the person sees

### 2.1 The chat list

No mark, no badge, no counter. The row keeps its last readable message,
time and unread count. A chat that cannot be read on this device for a while
is not news; the person learns it when they open the chat. (A badge on a row
would be a second state for the same thing, and would light up for chats the
person never opens.)

### 2.2 Inside the chat: nothing changes

Decided by the maintainer on 2026-11-20, replacing the waiting note this
section first specified. While the device is behind in a chat the chat looks
and works as always: the composer stays, the message menu, reactions,
replies, pins and conversation info are offered as usual. There is no note,
no banner, no badge, no second line and no explanation.

- **A message the person sends** is not published (the device holds it, C-93)
  and lands in the timeline in the client's ordinary failed-send state: the
  same "Not sent" marker and Retry as any other send failure, and the same
  ordinary send-failure line. Retry works as usual: back in step it sends,
  still behind it fails again the same way.
- **Any other action** (a reaction, an edit, a deletion, a shared pin, a
  rename, a role change) fails with the ordinary failure text that action
  already has.
- **Automatic sends** (typing, read receipts, repeat envelopes) are held
  quietly, as before.
- **Calls**: the call buttons stay. Pressing one fails the way a call that
  could not start always fails on that client (its ordinary could-not-start
  notice); nothing is captured or published (section 2.6).
- A refused read of the chat raises no banner (it is expected while behind).

### 2.3 No time limit

Decided by the maintainer on 2026-11-18: the device waits to be brought back
for as long as it takes. There is no "Remove from this device" offer; the
person can still delete the chat on this device the ordinary way.

If the account left the chat, or was removed, on another of its devices,
this device learns it from the account's own devices and shows "You left" /
"You were removed" exactly as today; it never waits.

### 2.4 Back up to date

Nothing is announced: no message, toast or sound; sends simply work again.
Where messages are missing, one line in the chat, styled as the date
separators are, placed where the missing messages would be:

> Some messages are not available.

- The line names no times, no device and no cause (maintainer, 2026-11-20;
  until then it read "Messages sent between {from} and {to} can't be shown
  on this device.").
- The device that brings this one back sends the missed messages it holds
  (maintainer, 2026-11-18): the person's other device in the chat, or else
  the other person in a one-to-one or the owner's or admin's device in a
  group. They are shown in place, and the line covers only what none of them
  held (older messages the server no longer keeps, or messages not on that
  device). If nothing is missing, there is no line.
- The line is local to this device. It is not sent, it marks nothing as
  read, and it is not an unread message.

Messages the device received while behind but could not read never appear
as "unreadable message" bubbles; they are covered by the line.

### 2.5 Notifications while behind

A message arriving in a chat this device is behind in cannot be read here. The
background notification, where one is shown today, says only "New message in
{chat}" (the existing generic wording), never a placeholder text. Being
behind adds no notifications of its own, and the person is never told it
fell behind.

### 2.6 Calls

A call offer in a chat this device is behind in cannot be read here, so a call does
not ring here and a group call is not offered here; the person's other
devices ring as usual. Nothing is shown about it. The call buttons stay
(maintainer, 2026-11-20: hiding a control without a word is the same
pattern as the waiting note). A call placed or joined from here fails before
the microphone or camera opens and before anything is sent, with the
client's ordinary could-not-start text ("Could not start the call. Try
again.", "Could not start the group call. Try again.", "Could not join the
group call. Try again."; iOS answers a 1x1 with its existing "Calls are not
available right now. Try again later."). Calls work again once the device is
back.

## 3. Controls and what each drives

| Control | Effect |
|---|---|
| Opening a chat this device is behind in | as always; a message sent shows "Not sent" with Retry |

Everything else is automatic: noticing, asking to be brought back, and coming
back.

## 4. Data the UI needs

Per chat, from the core and the client's own records:

- whether the device is behind in the chat (a durable flag, survives a
  restart), for holding sends and call starts;
- where the missing messages were, to place the line of 2.4;
- whether missed messages were supplied by another device of the account, and
  the time range they cover.

The flag is local; nothing about it is synced to other devices or sent to a
server beyond the request to come back itself (ADR-0091).

## 5. Cross-platform shape

One model: no state of its own on screen. Each client reuses its ordinary
failed-send row and its ordinary action failure texts, and draws the line of
2.4 as a date separator. No platform-specific divergence.

## 6. Copy checklist

One string only, the line of 2.4 (`chat.reentry.gap`, marked for native
review). Nothing narrates the machinery: no "update", "up to date", "this
device", "other devices", "sync", "epoch", "commit", "Welcome", "knock",
"re-entry". Sentence case, no exclamation marks.

## 7. What the person never sees

- a "Rejoin" or "Fix" button: the device does it by itself;
- the device being removed and re-added as two events in the member list or
  in anyone's chat: the person stays in the chat throughout, and other
  members see nothing more than an ordinary device admission shows them
  today;
- a notice in other people's chats that someone's device fell behind.
