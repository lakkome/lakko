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
 * - a banned author's comments are not shown, and never counted.
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
  /** How many rows the service returned so far, opened or not (a row that
   *  did not open, and a banned author's row, are still rows read). */
  readonly rowsRead: number;
  /** The first page was read (or there was nothing to read). */
  readonly firstRead: boolean;
  /** The service's comment count when this device last read the thread
   *  (`null` before a read that knew it): what a new count is compared
   *  with on the open channel's tick. */
  readonly countAtRead: number | null;
}

export const EMPTY_THREAD: ThreadState = {
  rows: [],
  nextAfter: 0,
  done: false,
  expanded: false,
  shownCount: COMMENT_PAGE,
  arrived: [],
  rowsRead: 0,
  firstRead: false,
  countAtRead: null,
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
  const rowsRead = state.rowsRead + pageLen;
  if (mode === "poll" && state.expanded) {
    const known = new Set(state.rows.map((c) => c.cursor));
    const fresh = page.filter((c) => !known.has(c.cursor));
    return {
      ...state,
      arrived: mergeComments(state.arrived, fresh),
      nextAfter: Math.max(state.nextAfter, nextAfter),
      done,
      rowsRead,
      firstRead: true,
    };
  }
  // A comment read into the thread no longer waits behind the line (the own
  // comment read after a send, raced by the tick's read).
  const read = new Set(page.map((c) => c.cursor));
  return {
    ...state,
    rows: mergeComments(state.rows, page),
    arrived: state.arrived.filter((c) => !read.has(c.cursor)),
    nextAfter: Math.max(state.nextAfter, nextAfter),
    done,
    rowsRead,
    firstRead: true,
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

/**
 * The comments a re-read of the first page shows to be gone
 * (`message-actions.md` section 9): held before, absent from the page, and
 * inside what the page covers (below its newest comment, or anywhere when the
 * page was the last). A comment beyond the page is not gone, only unread.
 * Pure.
 */
export function commentsGoneOnReread(
  before: readonly CommentView[],
  page: readonly CommentView[],
  pageLen: number = page.length,
): CommentView[] {
  const now = new Set(page.map((c) => c.cursor));
  const done = pageLen < COMMENT_PAGE;
  const newest = page.reduce((m, c) => Math.max(m, c.cursor), 0);
  return before.filter((c) => !now.has(c.cursor) && (done || c.cursor < newest));
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

/** The shared rule `crates/lakko-ui-rules` `comments` (over wasm): what
 *  follows the first comments of a collapsed thread. */
export interface CollapsedCommentsRule {
  collapsedComments(
    visible: number,
    rowsRead: number,
    firstPageRead: boolean,
    moreRows: boolean,
    serverCount?: number,
  ): { readonly kind: string; readonly count: number | undefined };
}

/**
 * What the collapsed thread shows. `serverCount` is the post's comment count
 * as the channels service maintains it (ADR-0087 phase B), `undefined` before
 * it answered. The service counts every stored comment, including those this
 * reader hides (a banned author's, one that does not open), so the count
 * never stands alone; the rule that says what follows is the shared one
 * (`lakko_ui_rules::comments`), so the three clients cannot disagree. `rules`
 * is `null` until wasm has loaded, and then nothing is said. Pure.
 */
export function collapsedView(
  state: ThreadState,
  isBanned: (authorHex: string) => boolean,
  serverCount: number | undefined,
  rules: CollapsedCommentsRule | null,
): CollapsedView {
  const visible = visibleComments(state.rows, isBanned);
  const shown = visible.slice(0, COLLAPSED_SHOWN);
  if (rules === null) return { shown, more: null };
  // A post the service says has none is marked done without a read.
  const answer = rules.collapsedComments(
    visible.length,
    state.rowsRead,
    state.firstRead || state.done,
    !state.done,
    serverCount,
  );
  switch (answer.kind) {
    case "count":
      return { shown, more: { kind: "count", n: answer.count ?? 1 } };
    case "unknown":
      return { shown, more: { kind: "unknown" } };
    case "comment":
      return { shown, more: { kind: "comment" } };
    default:
      return { shown, more: null };
  }
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

/**
 * What an open channel does for one post's thread on its tick (UX
 * `group-and-channel-privacy.md` 13.2: new comments reach a channel on
 * screen, collapsed or open). The written-three-times rule: web here, iOS
 * `ChannelConversationModel.commentRefresh`, Android `commentRefresh` in
 * `CommentCount.kt`, the same table in each client's tests.
 *
 * - `none`: nothing to read;
 * - `newer`: read after the last row read (collapsed: into the thread, so the
 *   first two and the count follow; open: behind `{n} new comments`);
 * - `reread`: a collapsed thread lost comments (a removal): read its first
 *   page again in place of what it held;
 * - `rebase`: an open thread lost comments: nothing is read (the open thread
 *   does not move), the count read is taken as the new base.
 *
 * Driven by the service's comment count, which the tick reads for the posts
 * in view with the reaction counts; with no count (an older service) an
 * open thread reads after its last row on every tick. Pure.
 */
export type CommentRefresh = "none" | "newer" | "reread" | "rebase";

export function commentRefresh(
  firstRead: boolean,
  expanded: boolean,
  morePages: boolean,
  rowsRead: number,
  countAtRead: number | null,
  serverCount: number | undefined,
): CommentRefresh {
  if (!firstRead) return "none";
  if (serverCount === undefined) return expanded && !morePages ? "newer" : "none";
  const seen = countAtRead ?? rowsRead;
  if (serverCount === seen) return "none";
  if (serverCount > seen) return morePages ? "none" : "newer";
  return expanded ? "rebase" : "reread";
}
