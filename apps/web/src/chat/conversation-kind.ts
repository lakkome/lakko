// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Chats and Groups are two lists over the same conversation store, split by
 * kind (docs/ux/client-redesign.md section 3).
 *
 * Since ADR-0062 the kind is not derived here. The creator writes it into the
 * group's GroupContext (`0xFF03`) and every member's device reads it with
 * `Group.groupKind()`, so it is the same answer on every device and never
 * changes: a two-person group is a group, and a one-to-one never becomes a
 * group by growing (it cannot grow). The people count is for display only.
 *
 * Pure: takes the value `groupKind()` returned, so it can be tested without
 * the wasm core. The numeric values are the wasm `GroupKind` enum (the
 * `0xFF03` wire bytes); `conversation-kind.test.ts` pins them against the
 * real enum through the boundary.
 */

export type ConversationKind = "chat" | "group";

/** `GroupKind.OneToOne` in the wasm core. */
export const GROUP_KIND_ONE_TO_ONE = 1;
/** `GroupKind.Group` in the wasm core. */
export const GROUP_KIND_GROUP = 2;

/**
 * The list a conversation belongs to, from what `Group.groupKind()` returned.
 *
 * `undefined` is a group carrying no `0xFF03` at all. A conversation cannot be
 * one (`join_from_welcome` refuses a role-table group without a kind, and
 * every creation site passes one), so the only such objects are the
 * non-conversation groups (`PublisherGroup`, `DeviceGroup`) and snapshots from
 * before the flag day. Those read as `group`: a one-to-one is something its
 * creator must have declared, and the group list never titles a conversation
 * after one person.
 */
export function conversationKindOfGroupKind(kind: number | undefined): ConversationKind {
  return kind === GROUP_KIND_ONE_TO_ONE ? "chat" : "group";
}

/** Anything with the wasm `Group.groupKind()` accessor. */
export interface HasGroupKind {
  groupKind(): number | undefined;
}

/**
 * The kind of a live group. A throwing read (a freed handle) answers `group`
 * for the reason `conversationKindOfGroupKind` gives for `undefined`.
 */
export function conversationKindOf(group: HasGroupKind): ConversationKind {
  let kind: number | undefined;
  try {
    kind = group.groupKind();
  } catch {
    kind = undefined;
  }
  return conversationKindOfGroupKind(kind);
}

/** Whether a group-name envelope (ADR-0051) may be applied: never in a
 *  one-to-one, which has no group name (ADR-0062 section 7). */
export function acceptsGroupName(kind: ConversationKind): boolean {
  return kind === "group";
}
