<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: a second device is a full client

Owner: [ADR-0050](../adr/0050-per-device-conversation-leaves.md). Status: with
the ADR. Applies to web, iOS and Android identically.

## 1. User goal and entry points

"I signed in on my other phone / laptop and I want my chats there." Entry
points are the existing ones: QR pairing, the ADR-0049 "Is this you?"
confirmation, three words, the full phrase, a passkey. Nothing new to find.

## 2. What the user sees

One state, no counters, no per-chat rows. While the device is still being
admitted to any of the account's chats, the chat list header shows
**"Synchronizing..."** where the connection status would be. Chats appear in
the list as they arrive, fully usable. When nothing is pending the label
disappears. That is the whole surface.

After a restore with no other live device, the same "Synchronizing..." shows
while contacts re-admit this device; there is nothing else to say to the user.
The ADR-0049 "Your identity is restored" block is removed.

The old device shows nothing extra during pairing beyond the existing
"Device added". The fan-out runs on the journal and needs no screen.

## 3. Devices screen

Lists the account's connected devices: this device / other, platform when
known, added-on date (from the binding). Remove asks for confirmation and
removes the device everywhere. Nothing about chats appears here.

## 4. Chats with several devices per person

- A 1x1 chat is a chat with exactly two people, whatever the device count.
- Members panel lists people, one row each. Admin actions act on the person.
- Read receipts, typing, reactions, edits and deletes are the person's, not
  the device's.
- System rows say "Alice added a device" / "Alice removed a device" only if the
  chat shows membership rows at all; never a commit number.

## 5. Failures

Anything that keeps a chat from admitting the device (peer offline, cap
reached, a bad binding) is not surfaced per chat. "Synchronizing..." stays,
the request stays on the journal, and the re-admission rule finishes it when
a member is online. The only user-facing error is the device cap, shown on the
**adding** device at pairing time: "Up to 8 devices per account. Remove one
first." A device whose binding is rejected by peers is a client bug, logged,
not a screen.

## 6. Data the UI needs

- Whether any self-service Add for this device is still pending in any of
  the account's known groups (one boolean drives "Synchronizing...").
- Per member: the resolved account key (people, not leaves, in every list).
- Per device in the DeviceGroup roster: binding `issued_at`, self flag.
