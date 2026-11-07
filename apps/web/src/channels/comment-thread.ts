// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The page model of one post's comments (UX `group-and-channel-privacy.md`
 * 13.2), with no DOM and no network, so the rules can be tested:
 *
 * - collapsed: the first 2, then `Show {n} more comments` when the whole
 *   thread is known, `Show more comments` when a page is still unread, a
 *   quiet `Comment` link when there are none;
 * - open: 20 at a time, oldest first, `Show more` at the end of a page;
 * - comments that arrive while a thread is open do not move it: they wait
 *   behind a `{n} new comments` line until it is pressed;
 * - a banned author's comments are not shown.
 */

/** One opened comment. */
export interface CommentView {
  readonly cursor: number;
  readonly writerKeyHex: string;
  /** The per-channel author key (what a ban list names), hex. */
  readonly authorHex: string;
  readonly name: string;
  readonly text: string;
  readonly replyTo: number | null;
  readonly serverTsMs: number;
}

/** The service's page length. */
export const COMMENT_PAGE = 20;
/** Shown under a collapsed post. */
export const COLLAPSED_SHOWN = 2;

export interface ThreadState {
  /** Comments this device read, by cursor, oldest first. */
  readonly rows: readonly CommentView[];
  /** The cursor to read after. */
  readonly nextAfter: number;
  /** The last page read was shorter than a page: nothing more to read. */
  readonly done: boolean;
  readonly expanded: boolean;
  /** How many an open thread shows (grows by a page). */
  readonly shownCount: number;
  /** Arrived while the thread was open, not shown yet. */
  readonly arrived: readonly CommentView[];
}

export const EMPTY_THREAD: ThreadState = {
  rows: [],
  nextAfter: 0,
  done: false,
  expanded: false,
  shownCount: COMMENT_PAGE,
  arrived: [],
};

/** `existing` with `incoming` added, by cursor, no repeats. Pure. */
export function mergeComments(existing: readonly CommentView[], incoming: readonly CommentView[]): CommentView[] {
  const byCursor = new Map(existing.map((c) => [c.cursor, c] as const));
  for (const c of incoming) byCursor.set(c.cursor, c);
  return [...byCursor.values()].sort((a, b) => a.cursor - b.cursor);
}

/** A page was read: fold it in. While the thread is open, comments newer
 *  than what it already had wait in `arrived`. Pure. */
export function withPage(
  state: ThreadState,
  page: readonly CommentView[],
  nextAfter: number,
  mode: "page" | "poll",
  /** How many rows the service returned (rows that did not open are left out
   *  of `page` but still count toward a full page). */
  pageLen: number = page.length,
): ThreadState {
  const done = pageLen < COMMENT_PAGE;
  if (mode === "poll" && state.expanded) {
    const known = new Set(state.rows.map((c) => c.cursor));
    const fresh = page.filter((c) => !known.has(c.cursor));
    return {
      ...state,
      arrived: mergeComments(state.arrived, fresh),
      nextAfter: Math.max(state.nextAfter, nextAfter),
      done,
    };
  }
  return {
    ...state,
    rows: mergeComments(state.rows, page),
    nextAfter: Math.max(state.nextAfter, nextAfter),
    done,
  };
}

/** The `{n} new comments` line was pressed. Pure. */
export function showArrived(state: ThreadState): ThreadState {
  if (state.arrived.length === 0) return state;
  const rows = mergeComments(state.rows, state.arrived);
  return { ...state, rows, arrived: [], shownCount: Math.max(state.shownCount, rows.length) };
}

/** A comment was removed (its author, a publisher). Pure. */
export function withoutComment(state: ThreadState, cursor: number): ThreadState {
  return {
    ...state,
    rows: state.rows.filter((c) => c.cursor !== cursor),
    arrived: state.arrived.filter((c) => c.cursor !== cursor),
  };
}

/** What the collapsed thread shows under a post. Pure. */
export interface CollapsedView {
  readonly shown: readonly CommentView[];
  /** The control after them: a known count, an unknown rest, the quiet
   *  `Comment` link when there are none, or nothing. */
  readonly more: { readonly kind: "count"; readonly n: number } | { readonly kind: "unknown" } | { readonly kind: "comment" } | null;
}

export function visibleComments(rows: readonly CommentView[], isBanned: (authorHex: string) => boolean): CommentView[] {
  return rows.filter((c) => !isBanned(c.authorHex));
}

/**
 * What the collapsed thread shows. `serverCount` is the post's comment count
 * as the channels service maintains it (ADR-0087 phase B), `undefined` before
 * it answered: with it, `Show {n} more comments` is known before the rows are
 * read, and a post with none shows `Comment` without reading a page. Once
 * every row is read the rows decide (they apply the ban list and are newer
 * than a cached count). Pure.
 */
export function collapsedView(
  state: ThreadState,
  isBanned: (authorHex: string) => boolean,
  serverCount?: number,
): CollapsedView {
  const visible = visibleComments(state.rows, isBanned);
  const shown = visible.slice(0, COLLAPSED_SHOWN);
  let unread: number | null;
  if (state.done) unread = 0;
  else if (serverCount !== undefined) unread = Math.max(0, serverCount - state.rows.length);
  else unread = null;
  if (unread === null) return { shown, more: { kind: "unknown" } };
  const rest = visible.length - shown.length + unread;
  if (visible.length === 0 && rest === 0) return { shown, more: { kind: "comment" } };
  return { shown, more: rest > 0 ? { kind: "count", n: rest } : null };
}

/** Whether the first page needs reading at all: not when the service says
 *  the post has no comments. Pure. */
export function needsFirstRead(serverCount: number | undefined): boolean {
  return serverCount === undefined || serverCount > 0;
}

/** What an open thread shows, and whether `Show more` follows it. Pure. */
export function openView(
  state: ThreadState,
  isBanned: (authorHex: string) => boolean,
): { readonly shown: readonly CommentView[]; readonly showMore: boolean; readonly arrived: number } {
  const visible = visibleComments(state.rows, isBanned);
  const shown = visible.slice(0, state.shownCount);
  const arrived = visibleComments(state.arrived, isBanned).length;
  return { shown, showMore: shown.length < visible.length || !state.done, arrived };
}

/** `Show more` in an open thread: one more page is shown (the caller reads
 *  another page when what it holds runs out). Pure. */
export function showMorePage(state: ThreadState): ThreadState {
  return { ...state, shownCount: state.shownCount + COMMENT_PAGE };
}

/** The comment a reply quotes, one level deep (a reply to a reply quotes
 *  only its own target). Pure. */
export function quotedComment(rows: readonly CommentView[], replyTo: number | null): CommentView | null {
  if (replyTo === null) return null;
  return rows.find((c) => c.cursor === replyTo) ?? null;
}
