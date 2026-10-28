<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: a second device is a full client

Owner: [ADR-0050](../adr/0050-per-device-conversation-leaves.md). Status: with
the ADR. Applies to web, iOS and Android identically.

## 1. User goal and entry points

"I signed in on my other phone / laptop and I want my chats there." Entry
points are the existing ones: QR pairing, the ADR-0049 "Is this you?"
confirmation, the full phrase, a passkey. Nothing new to find. (Three words
no longer bring keys to a new device: since ADR-0072 they are checked on a
device that already holds them.)

## 2. What the user sees

One state, no counters, no per-chat rows. While the device is still being
admitted to any of the account's chats, the chat list header shows
**"Synchronizing..."** where the connection status would be. Chats appear in
the list as they arrive, fully usable. When nothing is pending the label
disappears. That is the whole surface.

The label is for a device added while another device of the account is
alive: the pairing bundle names the account's chats, so the list is known and
finite, and the label ends when the last one arrives.

After a restore with no other live device there is no label (decided
2026-10-02). The device does not know when, or whether, each chat will come
back, so one label over the whole list would either never end or end without
meaning anything. Chats appear as members admit the device. Saved history
and the one per-chat state (waiting) that replaces this label on every route
are [`conversation-backup.md`](conversation-backup.md) (ADR-0064, proposed).
The ADR-0049 "Your identity is restored" block stays removed.

The old device shows nothing extra during pairing beyond the existing
"Device added". The fan-out runs on the journal and needs no screen.

## 3. Devices screen

Rewritten 2026-10-16. Until then every row read "Device 1", "Device 2" and so
on, and a person could not tell which was which. The screen now follows the
shape people know from other messengers' "active sessions".

**Layout.**

- **This device**, a section of its own at the top, one row, marked "online".
- **Other devices**, a section below, newest activity first.
- At the end of the first section, when other devices exist: **Remove all
  other devices** (destructive colour), with the line under it "Signs out
  every device except this one."

**A row.**

- Icon by platform (phone, tablet, computer, browser).
- First line: the device's model or name, e.g. "iPhone 16 Pro", "MacBook Pro",
  "Pixel 7a", or for a browser "Chrome on macOS".
- Second line: app and version, then the system and its version, e.g.
  "Lakko 1.4 · iOS 27.0.1", "Lakko web · macOS 26".
- Third line, when known: the city and country, e.g. "Almere, Netherlands".
- Right side: last activity, "online" for this device, otherwise a time for
  today ("14:05"), a weekday within the last week ("Sun"), else a date.
- Other devices only: **Remove**.

A device whose card has not arrived yet (it was added before this change, or
its card is still on the way) shows its platform icon, "Device" and the date
it was added, until the card arrives.

**Remove one.** Confirmation dialog: title **Remove "iPhone 16 Pro"?**, body
"It will be signed out and lose access to your messages.", buttons **Remove**
and **Cancel**. Then the same removal as before (ADR-0074: the password or
passkey may be asked for once).

**Remove all others.** Confirmation dialog: title **Remove all other
devices?**, body "They will be signed out and lose access to your messages.
This device stays signed in.", buttons **Remove all** and **Cancel**. One
password or passkey prompt covers all of them. While it runs, each row shows
its own progress; a device that could not be removed keeps its row with
"Could not remove. Try again." and its own **Remove** button.

**Where the city comes from.** Each device asks a Lakko server, without
signing in and without naming its account, which city its own internet address
belongs to, and puts the answer in its card. The server keeps nothing. When
the device uses the anti-censorship connection, the address is Lakko's own
relay, so no city is asked for and the third line is left out. A person who
does not want the city shown can turn it off in Settings > Privacy ("Show my
city to my other devices", on by default); the row then has no third line.

**Data the UI needs (feeds the ADR).**

1. A **device card** each device publishes about itself, end-to-end encrypted,
   to the account's own devices only (the DeviceGroup): platform, model or
   browser name, app version, system name and version, city and country when
   known, last active time. The server never sees it.
2. "Last active" is the last time the person did something on that device
   (opened a chat, wrote, pressed a control), not when the app happened to
   run. A device sends a fresh card after such an action when its last card
   is older than 30 minutes, and when a field other than the time changes
   (app or system version, city). A device nobody touches sends nothing, so
   its time stays where it was. Decided by the maintainer 2026-10-16.
3. An anonymous city lookup route on an existing service: no session, no
   token, no account; answers city and country (or nothing); keeps no record
   and writes no log line; limited per address in memory. The city database
   lives on the server.
4. The card is keyed by the device's leaf, so removing a device removes its
   card.

### 3.1 This device was removed

When another device of the account removes this one, this device learns it
from the DeviceGroup journal (its own removal commit leaves its DeviceGroup
inactive). From then on nothing new reaches it, so it must not pretend to be
synchronizing. It shows one full-screen state instead of the app:

- Title: **This device was removed**
- Body: "It was removed from your account on another device. New messages no
  longer reach it. Sign out, then sign in again if you want to use it."
- One button: **Sign out**. It does what "Forget this device" does: this
  device's keys and local history go, the other devices keep theirs.

No dismiss, no retry, no chat list behind it. Shown on web, iOS and Android
alike, at launch too if the removal landed while the app was closed.

A device cut off by a DeviceGroup reset (another device repaired a forked
DeviceGroup, ADR-0057 2026-09-30) is not covered here: it reads `401` rather
than its own removal, and a `401` is not proof of removal (a wrong clock
gives the same answer). Unbuilt.

## 4. Chats with several devices per person

- A 1x1 chat is a chat with exactly two people, whatever the device count.
- Members panel lists people, one row each. Admin actions act on the person.
- Read receipts, typing, reactions, edits and deletes are the person's, not
  the device's.
- System rows say "Alice added a device" / "Alice removed a device" only if the
  chat shows membership rows at all; never a commit number.

## 5. Failures

Anything that keeps a chat from admitting the device (peer offline, cap
reached, a bad binding) is not surfaced per chat. After a pairing
"Synchronizing..." stays while a chat the bundle named is still missing; the
request stays on the journal, and the re-admission rule finishes it when a
member is online. The only user-facing error is the device cap, shown on the
**adding** device at pairing time: "Up to 8 devices per account. Remove one
first." A device whose binding is rejected by peers is a client bug, logged,
not a screen.

## 6. Data the UI needs

- Whether any chat the pairing bundle named has not been joined yet (one
  boolean drives "Synchronizing..."). A restore from the phrase sets nothing
  here.
- Per member: the resolved account key (people, not leaves, in every list).
- Per device in the DeviceGroup roster: binding `issued_at`, self flag.
- Whether this device's own DeviceGroup is still active (`DeviceGroup.isActive`),
  which drives section 3.1.
