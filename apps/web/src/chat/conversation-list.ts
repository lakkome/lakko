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
 * The ordering rule is not new: the chat pane already sorted pinned first,
 * archived last, newest between. What changes is the set it sorts. Chats and
 * groups were already one store split by a filter, so they merge by dropping
 * it; channels are their own store and have to be mapped in, which is the
 * only real work and the reason this module exists.
 *
 * Pure and source-agnostic on purpose. The caller maps each store into a
 * [`ConversationRow`] and this decides nothing except order - so it can be
 * run by a test (vitest has no jsdom and no Solid transform here, so nothing
 * in the components is reachable) and so neither store's internals leak into
 * the rule.
 *
 * **The row says nothing about which kind it is.** `kind` is here to route a
 * tap and for nothing else: no badge, no label, no differing avatar. A
 * person who subscribed to a channel knows they subscribed to a channel.
 */

/** Which screen a row opens. Not rendered. */
export type ConversationRowKind = "chat" | "group" | "channel";

/** One row of the merged list. */
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
  readonly pinned: boolean;
  readonly muted: boolean;
  readonly archived: boolean;
}

/**
 * Pinned first, archived last, newest between.
 *
 * Ties break on the title and then on `(kind, id)`, so the order is total:
 * two conversations with no activity yet - both `null` - would otherwise sit
 * in whatever order the stores happened to yield, and a list that reshuffles
 * on an unrelated refresh reads as a bug.
 */
export function sortConversations(
  rows: readonly ConversationRow[],
): readonly ConversationRow[] {
  return [...rows].sort(compareRows);
}

function compareRows(a: ConversationRow, b: ConversationRow): number {
  if (a.pinned !== b.pinned) return a.pinned ? -1 : 1;
  if (a.archived !== b.archived) return a.archived ? 1 : -1;
  const byActivity = (b.lastActivityMs ?? 0) - (a.lastActivityMs ?? 0);
  if (byActivity !== 0) return byActivity;
  const byTitle = a.title.localeCompare(b.title);
  if (byTitle !== 0) return byTitle;
  if (a.kind !== b.kind) return a.kind < b.kind ? -1 : 1;
  return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
}

