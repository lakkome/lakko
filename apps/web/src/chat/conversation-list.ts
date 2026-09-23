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

/**
 * Merge the sources and order them.
 *
 * Takes the already-mapped rows rather than the stores, so a caller that
 * gains a fourth kind adds it here and nothing else changes.
 */
export function mergeConversations(
  ...sources: readonly (readonly ConversationRow[])[]
): readonly ConversationRow[] {
  return sortConversations(sources.flat());
}

/**
 * Split the ordered list into the part that is shown and the part behind the
 * "Archived" row.
 *
 * Archived conversations stay in the list on a wide layout, below a divider.
 * On a narrow one, more than a handful of them push everything live off the
 * screen, so past [`ARCHIVED_INLINE_LIMIT`] they collapse behind one row.
 * Below it they are simply the tail, which is what `sortConversations`
 * already produced.
 */
export const ARCHIVED_INLINE_LIMIT = 5;

export interface ConversationSections {
  /** Everything not archived, in order. */
  readonly live: readonly ConversationRow[];
  /** The archived tail, in order. */
  readonly archived: readonly ConversationRow[];
  /** Whether the archived tail should collapse behind one row. */
  readonly collapseArchived: boolean;
}

export function sectionConversations(
  ordered: readonly ConversationRow[],
  narrow: boolean,
  limit: number = ARCHIVED_INLINE_LIMIT,
): ConversationSections {
  const live = ordered.filter((r) => !r.archived);
  const archived = ordered.filter((r) => r.archived);
  return { live, archived, collapseArchived: narrow && archived.length > limit };
}

/**
 * Whether a row matches what was typed, over the data this device already
 * holds.
 *
 * Local and loose: case-folded and diacritic-folded substring over the title
 * and the preview. This is NOT the network lookup, which can only ever be
 * exact (`docs/ux/navigation.md` section 4.1) and which that section leaves
 * open - nothing here reaches the network, and nothing here should grow to.
 */
export function conversationMatches(row: ConversationRow, query: string): boolean {
  const q = fold(query);
  if (q.length === 0) return true;
  return fold(row.title).includes(q) || fold(row.preview).includes(q);
}

function fold(text: string): string {
  return text
    .normalize("NFD")
    .replace(/\p{Diacritic}/gu, "")
    .toLocaleLowerCase()
    .trim();
}
