// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** UX `group-and-channel-privacy.md` 13.2: the comment thread's page model. */
import { describe, expect, it } from "vitest";

import {
  collapsedView,
  COMMENT_PAGE,
  needsFirstRead,
  EMPTY_THREAD,
  openView,
  quotedComment,
  showArrived,
  showMorePage,
  withoutComment,
  withPage,
  type CommentView,
} from "./comment-thread";

function comment(cursor: number, author = "a"): CommentView {
  return {
    cursor,
    writerKeyHex: `w${cursor}`,
    authorHex: author,
    name: `n${cursor}`,
    text: `t${cursor}`,
    replyTo: null,
    serverTsMs: cursor,
  };
}

const none = (): boolean => false;

describe("collapsed", () => {
  it("offers the quiet Comment link when a known thread is empty", () => {
    const st = withPage(EMPTY_THREAD, [], 0, "page");
    expect(collapsedView(st, none)).toEqual({ shown: [], more: { kind: "comment" } });
  });

  it("shows the first 2 and a counted rest when the thread is known whole", () => {
    const st = withPage(EMPTY_THREAD, [comment(1), comment(2), comment(3), comment(4)], 4, "page");
    const v = collapsedView(st, none);
    expect(v.shown.map((c) => c.cursor)).toEqual([1, 2]);
    expect(v.more).toEqual({ kind: "count", n: 2 });
  });

  it("does not count when a page is still unread", () => {
    const page = Array.from({ length: COMMENT_PAGE }, (_, i) => comment(i + 1));
    const st = withPage(EMPTY_THREAD, page, COMMENT_PAGE, "page");
    expect(collapsedView(st, none).more).toEqual({ kind: "unknown" });
  });

  it("counts a full page as a page even when some rows did not open", () => {
    const st = withPage(EMPTY_THREAD, [comment(1)], 20, "page", COMMENT_PAGE);
    expect(st.done).toBe(false);
  });

  it("hides a banned author", () => {
    const st = withPage(EMPTY_THREAD, [comment(1, "x"), comment(2), comment(3)], 3, "page");
    const v = collapsedView(st, (a) => a === "x");
    expect(v.shown.map((c) => c.cursor)).toEqual([2, 3]);
    expect(v.more).toBeNull();
  });
});

describe("open", () => {
  it("holds new comments behind the line until it is pressed", () => {
    let st = withPage(EMPTY_THREAD, [comment(1)], 1, "page");
    st = { ...st, expanded: true };
    st = withPage(st, [comment(2), comment(3)], 3, "poll");
    expect(openView(st, none).shown.map((c) => c.cursor)).toEqual([1]);
    expect(openView(st, none).arrived).toBe(2);
    st = showArrived(st);
    expect(openView(st, none).shown.map((c) => c.cursor)).toEqual([1, 2, 3]);
    expect(st.arrived).toEqual([]);
  });

  it("shows 20 at a time and offers Show more while there is more", () => {
    const page = Array.from({ length: 25 }, (_, i) => comment(i + 1));
    let st = withPage({ ...EMPTY_THREAD, expanded: true }, page, 25, "page", 5);
    expect(openView(st, none).shown).toHaveLength(COMMENT_PAGE);
    expect(openView(st, none).showMore).toBe(true);
    st = showMorePage(st);
    expect(openView(st, none).shown).toHaveLength(25);
    expect(openView(st, none).showMore).toBe(false);
  });

  it("removes a deleted comment without a trace", () => {
    const st = withoutComment(withPage(EMPTY_THREAD, [comment(1), comment(2)], 2, "page"), 1);
    expect(st.rows.map((c) => c.cursor)).toEqual([2]);
  });

  it("quotes a reply's target when it is read", () => {
    const rows = [comment(1), { ...comment(2), replyTo: 1 }];
    expect(quotedComment(rows, 1)?.cursor).toBe(1);
    expect(quotedComment(rows, 9)).toBeNull();
    expect(quotedComment(rows, null)).toBeNull();
  });
});

describe("collapsed with the service's comment count (ADR-0087 phase B)", () => {
  it("shows Comment for a post with none, without reading a page", () => {
    expect(needsFirstRead(0)).toBe(false);
    expect(needsFirstRead(undefined)).toBe(true);
    expect(needsFirstRead(3)).toBe(true);
    expect(collapsedView(EMPTY_THREAD, none, 0)).toEqual({ shown: [], more: { kind: "comment" } });
  });

  it("counts the unread rest from the service, not from the rows read", () => {
    const st = withPage(EMPTY_THREAD, [comment(1), comment(2), comment(3)], 3, "page", COMMENT_PAGE);
    // A full page read, the service says 40.
    expect(st.done).toBe(false);
    const v = collapsedView(st, none, 40);
    expect(v.shown.map((c) => c.cursor)).toEqual([1, 2]);
    expect(v.more).toEqual({ kind: "count", n: 38 });
  });

  it("knows the count before any row is read", () => {
    expect(collapsedView(EMPTY_THREAD, none, 5).more).toEqual({ kind: "count", n: 5 });
  });

  it("leaves banned rows out of the count once they are read", () => {
    const st = withPage(EMPTY_THREAD, [comment(1, "x"), comment(2), comment(3)], 3, "page", COMMENT_PAGE);
    expect(collapsedView(st, (a) => a === "x", 10).more).toEqual({ kind: "count", n: 7 });
  });

  it("lets the rows decide once every row is read", () => {
    const st = withPage(EMPTY_THREAD, [comment(1), comment(2), comment(3)], 3, "page", 3);
    expect(st.done).toBe(true);
    // A stale count of 1 does not hide the third comment.
    expect(collapsedView(st, none, 1).more).toEqual({ kind: "count", n: 1 });
  });

  it("never goes below zero on a stale count", () => {
    const st = withPage(EMPTY_THREAD, [comment(1), comment(2)], 2, "page", COMMENT_PAGE);
    expect(collapsedView(st, none, 1).more).toBeNull();
  });
});
