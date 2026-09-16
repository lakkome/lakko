// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Chats and Groups are two lists over the same conversation store, split by
 * kind (docs/ux/client-redesign.md section 3): a conversation with exactly
 * two PEOPLE is a chat, anything else is a group.
 *
 * People are counted by account, not by device leaf (`peopleCountOf` in
 * `person-key.ts`), so a two-person chat where one person has three devices
 * stays a chat.
 *
 * The spec also says a group created through the new-group flow is a group
 * even while it has two people. The store records no such flag (an
 * `ActiveGroup` carries only a label, and the label is re-synthesised on
 * every reload), so this helper decides on the people count alone. A
 * two-person group therefore lists under Chats until a third person joins;
 * that gap is recorded in the redesign report rather than papered over with
 * a flag nothing persists.
 *
 * Pure: no wasm, no Solid.
 */

export type ConversationKind = "chat" | "group";

/** The kind of a conversation with `peopleCount` distinct people in it. */
export function conversationKindOf(peopleCount: number): ConversationKind {
  return peopleCount === 2 ? "chat" : "group";
}

/** Which list a conversation of `peopleCount` people belongs to. */
export function belongsToList(peopleCount: number, list: ConversationKind): boolean {
  return conversationKindOf(peopleCount) === list;
}
