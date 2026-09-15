<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# New group

UX spec for creating a group chat. Before this, a group could only be made by
opening a 1x1 chat and adding people from its header, which nobody finds.

## 1. Goal and entry point

| Goal | Entry point |
|---|---|
| Start a conversation with several people | Chats > the new-chat button > **New group** |

The same button offers **New chat** (which opens Contacts) and **New group**.

## 2. Flow

1. **New group** screen: a name field and the contact list with a checkbox
   per contact. The name is local (never sent to the server) and optional;
   an empty name becomes "New group".
2. **Create group** is enabled once at least one contact is picked.
3. Tap: the group is created with the first picked contact (one Welcome, the
   creator is the owner per ADR-0022), the chat opens at once, and the
   remaining picks are added from the chat view through its normal add path,
   one after another, with its normal obligations (ADR-0040 durable commit
   record, ADR-0045 proposal + owner sweep, echo suppression, push-routing
   redistribution).
4. The chat header shows the growing member count as each add lands.

## 3. States

| State | What the user sees |
|---|---|
| No contacts yet | The members card says a group starts from people you already know, with a button to Contacts. |
| A contact without a captured account id (pre-ADR-0021 record) | Listed, dimmed, unselectable, with the line "Accept this contact again to add them to a group." |
| Creating | Button reads "Creating…", form disabled. |
| The first Welcome failed | The error line under the form; nothing was created; the form stays filled. |
| A later add failed | The chat opens anyway, with the members that did land; the chat shows "Could not add X, Y. Add them from the members panel." |
| Reload between create and the adds | The group exists with its first member; the parked list is gone (in memory only). The members panel adds the rest. |

## 4. Controls and data

- Name: local label of the `ActiveGroup`; max 64 characters.
- Members: `AcceptedContact` records with `accountIdHex` + `signaturePubkeyB64`,
  which is what `startGroupWith` / `addMemberToGroup` take, so no username
  lookup is needed.
- The hand-off: `chat/pending-group-setup.ts` (`setPendingGroupSetup` /
  `takePendingGroupSetup`), consumed once by the chat view on mount.

## 5. Cross-platform shape

Web builds this first. iOS and Android implement the same screen (name, a
checkable contact list, Create) from the new-chat entry point; the sequential
add through the chat's own path is the model on all three.
