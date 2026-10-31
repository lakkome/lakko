<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# New group

UX spec for creating a group chat. Before this, a group could only be made by
opening a 1x1 chat and adding people from its header, which nobody finds.

**Amended 2026-10-31 (maintainer's decisions,
[`group-and-channel-privacy.md`](group-and-channel-privacy.md),
[ADR-0087](../adr/0087-group-and-channel-handles-and-privacy.md)):** every group has
an `@name`, is Private or Public, and may be created with nobody but its creator.
Sections 2 to 4 below are rewritten for it; the form's fields, its states and the
proposed `@name` are in that spec, sections 2 and 3, and this document keeps the
order of events.

## 1. Goal and entry point

| Goal | Entry point |
|---|---|
| Start a conversation with several people | Chats > the new-chat button > **New group** |

The same button offers **New chat** (which opens Contacts) and **New group**.

## 2. Flow

1. **New group** screen: a name field, the `@name` field (filled in), the `Who
   can join` picker (Private by default) and the contact list with a checkbox per
   contact, under `Add people (optional)`. The name is optional when Private,
   required when Public. A name reaches every member (ADR-0051); a public group's
   name is also published. With no name, the title follows
   [`group-and-channel-privacy.md`](group-and-channel-privacy.md) section 4.3.
2. **Create group** is enabled once the `@name` is well formed and, if Public,
   there is a name. Nobody has to be picked.
3. Tap: the group is made on this device, the `@name` is claimed, the journal is
   claimed, and the person's other devices join in the founding commit. If the name
   is held or the claim fails, nothing exists and the form stays filled. The chat
   opens at once, and picks are added from the chat view through its normal add path,
   one after another, with its normal obligations (ADR-0040 durable commit record,
   ADR-0045 proposal + owner sweep, echo suppression, push-routing redistribution).
4. With no picks the chat opens as a group of one
   ([`group-and-channel-privacy.md`](group-and-channel-privacy.md) section 10). With
   picks the chat header shows the growing member count as each add lands.

## 3. States

| State | What the user sees |
|---|---|
| No contacts yet | `Add people` reads "No contacts yet. You can invite people after creating the group." Create stays available. |
| A contact without a captured account id (pre-ADR-0021 record) | Listed, dimmed, unselectable, with the line "Accept this contact again to add them to a group." |
| Creating | Button reads "Creating…", form disabled. |
| The `@name` is held, the claim failed, or secure setup failed | The error line under the field or the form; nothing was created; the form stays filled ([`group-and-channel-privacy.md`](group-and-channel-privacy.md) section 3.3). |
| A later add failed | The chat opens anyway, with the members that did land; the chat shows "Could not add X, Y. Add them from the members panel." |
| Reload between create and the adds | The group exists with its first member; the parked list is gone (in memory only). The members panel adds the rest. |

## 4. Controls and data

- Name: 1 to 64 characters, sent into the group (ADR-0051).
- `@name`, `Who can join`: [`group-and-channel-privacy.md`](group-and-channel-privacy.md)
  sections 2 and 3.
- Members: `AcceptedContact` records with `accountIdHex` + `signaturePubkeyB64`,
  which is what `startGroupWith` / `addMemberToGroup` take, so no username
  lookup is needed.
- The hand-off: `chat/pending-group-setup.ts` (`setPendingGroupSetup` /
  `takePendingGroupSetup`), consumed once by the chat view on mount.

## 5. Cross-platform shape

Web builds this first. iOS and Android implement the same screen (name, a
checkable contact list, Create) from the new-chat entry point; the sequential
add through the chat's own path is the model on all three.
