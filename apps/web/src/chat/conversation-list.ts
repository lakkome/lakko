// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One list for every conversation: one-to-one chats, groups and channels.
 *
 * `docs/ux/navigation.md` section 2. Chats, Groups and Channels used to be
 * three destinations for one activity - a person does not think "I will go
 * to the Groups place", they think of a conversation and look for it - and a
 * list sorted by WHEN cannot be three lists.
 *
 * **The order itself is not here.** It is `order_conversations` in
 * `crates/lakko-ui-rules`, shared with iOS and Android; what this module owns
 * is the mapping between the rich row the web draws and the six fields that
 * order reads. Chats and groups were already one store split by a filter, so
 * they merge by dropping it; channels are their own store and have to be
 * mapped in, which is the real work and the reason this module exists.
 *
 * **The row says nothing about which kind it is.** `kind` is here to route a
 * tap and for nothing else: no badge, no label, no differing avatar. A
 * person who subscribed to a channel knows they subscribed to a channel.
 */
import type { UiRules } from "./ui-rules";

/** Which screen a row opens. Not rendered. */
export type ConversationRowKind = "chat" | "group" | "channel";

/** One row of the merged list, as the web draws it. */
export interface ConversationRow {
  /** Stable within a kind; the pair `(kind, id)` is unique in the list. */
  readonly id: string;
  readonly kind: ConversationRowKind;
  readonly title: string;
  /** The last message or post, already flattened to one line. */
  readonly preview: string;
  /**
   * When the last thing a person can SEE happened, in unix millis, or `null`
   * for a conversation nothing has happened in yet. A row that draws nothing
   * does not move a conversation to the top.
   */
  readonly lastActivityMs: number | null;
  readonly unread: number;
  /**
   * Something unread, or marked unread by hand - what the unread badge means
   * and what a folder's "Read" toggle reads (`docs/ux/chat-folders.md` 3.2).
   * Absent means `unread > 0`.
   */
  readonly hasUnread?: boolean;
  readonly pinned: boolean;
  readonly muted: boolean;
  readonly archived: boolean;
}

/**
 * Pinned first, archived last, newest between - decided by the shared rule.
 *
 * The rule hands back the input INDICES rather than rows, so the avatar, the
 * preview and the unread count never cross the wasm boundary at all: this
 * reorders the array it was already given.
 */
export function sortConversations(
  rows: readonly ConversationRow[],
  rules: UiRules,
): readonly ConversationRow[] {
  const order = rules.orderConversations(
    rows.map(
      (r) =>
        new rules.ConversationRow(
          r.id,
          kindFor(rules, r.kind),
          r.title,
          r.lastActivityMs === null ? undefined : BigInt(r.lastActivityMs),
          r.pinned,
          r.archived,
        ),
    ),
  );
  return Array.from(order, (i) => rows[i]).filter(
    (r): r is ConversationRow => r !== undefined,
  );
}

type WasmConversationKind = UiRules["ConversationKind"][keyof UiRules["ConversationKind"]];

/** Whether a row counts as unread (the badge's meaning). */
export function rowHasUnread(row: ConversationRow): boolean {
  return row.hasUnread ?? row.unread > 0;
}

export function kindFor(rules: UiRules, kind: ConversationRowKind): WasmConversationKind {
  switch (kind) {
    case "group":
      return rules.ConversationKind.Group;
    case "channel":
      return rules.ConversationKind.Channel;
    default:
      return rules.ConversationKind.Chat;
  }
}
