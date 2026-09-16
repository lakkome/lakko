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
 * even while it has two people, and that a conversation never moves between
 * the two lists on its own. The store records no such flag, so the kind is
 * PERSISTED per conversation (`conversation-kind-store.ts`): the flow that
 * creates a conversation writes it (new group -> `group`, contact accept /
 * New chat -> `chat`), and a conversation first seen through a Welcome or a
 * restore derives it once from the people count. The one sanctioned change
 * is chat -> group, when more than two people are observed; group -> chat
 * never happens.
 *
 * Pure: no wasm, no Solid, no storage. The decision lives here so it can be
 * tested; the store only reads and writes what it decides.
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

/**
 * The kind to show and to keep, given what is persisted (`null` at first
 * sight) and the people count observed now.
 *
 *   - nothing persisted: derive from the count (2 = chat, else group);
 *   - `group` persisted: stays a group whatever the count;
 *   - `chat` persisted: stays a chat until MORE than two people are observed,
 *     then it is a group for good. A count below two (a torn roster read, a
 *     member who left) never flips a chat back into the Groups list.
 */
export function decideConversationKind(
  persisted: ConversationKind | null,
  peopleCount: number,
): ConversationKind {
  if (persisted === null) return conversationKindOf(peopleCount);
  if (persisted === "group") return "group";
  return peopleCount > 2 ? "group" : "chat";
}

/**
 * What to write when a flow states a kind outright: a group stays a group, a
 * chat may become a group, a group never becomes a chat. Returns the kind to
 * store (which may be the current one).
 */
export function nextPersistedKind(
  current: ConversationKind | null,
  stated: ConversationKind,
): ConversationKind {
  if (current === "group") return "group";
  return stated;
}
