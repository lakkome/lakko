<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: conversations on a restored or added device

Owner: [ADR-0064](../adr/0064-conversation-backup-and-device-re-entry.md).
Status: with the ADR (Proposed). Applies to web, iOS and Android identically.

## 1. User goal and entry points

"I signed in on another device and I want my conversations there: what was
said, and to keep writing."

Entry points are the existing ones and nothing new: the recovery phrase, three
words, a passkey, QR pairing, the "Is this you?" confirmation. The person does
nothing extra, and there is nothing to switch on: history is always saved.

Signing in with the recovery phrase **adds a device**. It never removes or
disconnects the person's other devices: the person may simply be away from
them. A device the person no longer has is removed on the Devices screen, as
today.

Two things come back separately, and the UI never pretends they are one:

- **What was said.** Earlier messages, from the encrypted history the
  account's devices saved.
- **Being in the conversation.** Receiving and sending new messages, once this
  device has a place in the conversation.

## 2. What the person sees

### 2.1 The conversation list

The list fills with the account's conversations and channels, most recently
active first, as their saved history arrives. The first screenful comes before
the rest; scrolling loads more. Rows are ordinary rows: title, last message
preview, time, unread count.

There is no general label, no counter, no progress bar and no mark on a row.
The "Synchronizing..." label of [`second-device.md`](second-device.md) is
removed on every route.

### 2.2 Waiting: the one new state

A conversation is **waiting** while the person can read it on this device but
cannot write yet: its earlier messages are shown, and this device has no place
in it yet. It is shown in one place only, inside the conversation, where the
composer would be:

> This device is still joining this chat. You can read earlier messages, and
> write once it has joined.

Everything a waiting conversation cannot do is absent rather than disabled:
no composer, no call buttons, and the message menu offers Copy and Forward
only. Conversation info opens read-only from the saved snapshot (name,
people).

Waiting ends by itself, normally within seconds of the sign-in and with no
message to the person. The device asks the conversation's members to let it in
and, at the same time, enters by itself; whichever happens first counts. It
needs no approval and no member online, as long as nobody changed who is in
the conversation after the account's last device saved it. If somebody did,
the first member that comes online lets the device in.

There is no timer and no failure state. A conversation nobody ever returns to
stays readable and waiting.

A conversation the person had left, or was removed from, is shown as it is
today ("You left" / "You were removed"), readable, and never waits.

### 2.3 Earlier messages

Shown exactly as in a live conversation: authors, replies, forwards,
reactions, edits, attachments, pins. Scrolling up loads earlier messages in
steps; a quiet "Loading earlier messages" row shows while a step is on its
way. An attachment whose file is no longer available shows the "no longer
available" state of
[`saved-messages-and-file-lifetime.md`](saved-messages-and-file-lifetime.md).

### 2.4 Channels

A channel the person subscribes to appears in the list with its earlier posts
and is readable at once. A channel the person publishes in is waiting (2.2)
until this device has joined its publishers, by the same rule.

### 2.5 Failures

- **Saved history cannot be fetched** (network): the list shows what it has;
  a conversation whose earlier messages are missing shows, at the top of the
  conversation, "Earlier messages could not be loaded." with **Try again**.
  It is retried on the next launch without being asked.
- **No saved history exists** for a conversation: it appears when this device
  has joined it, with no earlier messages.
- **The account already has 8 devices**: this device cannot join a
  conversation until one is removed. The conversation stays waiting, and the
  note reads: "You have 8 devices on this account. Remove one on the Devices
  screen so this device can join." with a button **Devices**.
- **A wrong or damaged saved record** is skipped and logged; it is never a
  screen.

## 3. Controls and what each drives

| Control | Effect |
|---|---|
| Opening a waiting conversation | reads saved history only; marks nothing as read for others |
| Scrolling up | fetches the next step of earlier messages |
| **Try again** (2.5) | re-fetches the saved history of that conversation |
| **Devices** (2.5) | opens the Devices screen |

## 4. Data the UI needs

Per conversation or channel, available before the device is in it:

- title (group name, the other person for a one-to-one, the channel name),
  kind, last activity time, last message preview, unread boundary (the
  account's read cursor is already carried between devices);
- the people in it with display names (people, not devices), and the person's
  own role;
- membership at save time: in it / left / removed;
- the earlier messages in steps, newest first, each with author (a person),
  time, text, attachments, reply, forward, reactions, edited / deleted flags,
  call outcome, shared and personal pins; for a channel, its posts;
- whether the conversation is waiting, and whether the reason is the device
  limit.

## 5. Cross-platform shape

One model on the three clients: a list row that does not require a live
conversation handle, and a composer-replacing note for a waiting
conversation. Platform differences are only the usual ones (long-press on the
natives, right-click / hover on web for the message menu).

## 6. Copy checklist

No protocol or component names; "chat" in user-facing text; sentence case; no
exclamation marks. Every string is a catalog key (ADR-0061). The waiting note
of 2.2 and the device-limit note of 2.5 are marked for native review.
