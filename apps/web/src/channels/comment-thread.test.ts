// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** UX `group-and-channel-privacy.md` 13.2: the comment thread's page model. */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";

import {
  collapsedView,
  commentRefresh,
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
import type { CollapsedCommentsRule } from "./comment-thread";

// The rule is the shared one (`lakko_ui_rules::comments`), driven here across
// the real wasm boundary.
let rules: CollapsedCommentsRule;

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
  rules = wasm;
});


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
    expect(collapsedView(st, none, undefined, rules)).toEqual({ shown: [], more: { kind: "comment" } });
  });

  it("shows the first 2 and a counted rest when the thread is known whole", () => {
    const st = withPage(EMPTY_THREAD, [comment(1), comment(2), comment(3), comment(4)], 4, "page");
    const v = collapsedView(st, none, undefined, rules);
    expect(v.shown.map((c) => c.cursor)).toEqual([1, 2]);
    expect(v.more).toEqual({ kind: "count", n: 2 });
  });

  it("does not count when a page is still unread", () => {
    const page = Array.from({ length: COMMENT_PAGE }, (_, i) => comment(i + 1));
    const st = withPage(EMPTY_THREAD, page, COMMENT_PAGE, "page");
    expect(collapsedView(st, none, undefined, rules).more).toEqual({ kind: "unknown" });
  });

  it("counts a full page as a page even when some rows did not open", () => {
    const st = withPage(EMPTY_THREAD, [comment(1)], 20, "page", COMMENT_PAGE);
    expect(st.done).toBe(false);
  });

  it("hides a banned author", () => {
    const st = withPage(EMPTY_THREAD, [comment(1, "x"), comment(2), comment(3)], 3, "page");
    const v = collapsedView(st, (a) => a === "x", undefined, rules);
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
    expect(collapsedView(EMPTY_THREAD, none, 0, rules)).toEqual({ shown: [], more: { kind: "comment" } });
  });

  it("takes only the rows not read yet from the service", () => {
    // A full page of 20 rows read, 3 of them opened; the service says 40.
    const st = withPage(EMPTY_THREAD, [comment(1), comment(2), comment(3)], 3, "page", COMMENT_PAGE);
    expect(st.done).toBe(false);
    expect(st.rowsRead).toBe(COMMENT_PAGE);
    const v = collapsedView(st, none, 40, rules);
    expect(v.shown.map((c) => c.cursor)).toEqual([1, 2]);
    // 1 opened and not shown, plus the 20 rows still unread.
    expect(v.more).toEqual({ kind: "count", n: 21 });
  });

  it("says nothing before the shared rule has loaded", () => {
    const st = withPage(EMPTY_THREAD, [], 0, "page");
    expect(collapsedView(st, none, 3, null).more).toBeNull();
  });

  it("says no count before the first page is read", () => {
    // The service's count includes comments this reader may hide.
    expect(collapsedView(EMPTY_THREAD, none, 5, rules).more).toBeNull();
  });

  it("leaves banned rows out of the count once they are read", () => {
    const st = withPage(EMPTY_THREAD, [comment(1, "x"), comment(2), comment(3)], 3, "page", COMMENT_PAGE);
    expect(collapsedView(st, (a) => a === "x", 30, rules).more).toEqual({ kind: "count", n: 10 });
  });

  it("never counts a banned author's comments the service still holds", () => {
    // Base scenario 28: two stored comments, both by a banned author. The
    // service says 2; the reader shows none and offers `Comment`.
    const st = withPage(EMPTY_THREAD, [comment(1, "x"), comment(2, "x")], 2, "page");
    expect(st.done).toBe(true);
    expect(collapsedView(st, (a) => a === "x", 2, rules)).toEqual({ shown: [], more: { kind: "comment" } });
  });

  it("never counts a comment that did not open", () => {
    // One row read, it did not open; the service says 1.
    const st = withPage(EMPTY_THREAD, [], 1, "page", 1);
    expect(collapsedView(st, none, 1, rules)).toEqual({ shown: [], more: { kind: "comment" } });
  });

  it("lets the rows decide once every row is read", () => {
    const st = withPage(EMPTY_THREAD, [comment(1), comment(2), comment(3)], 3, "page", 3);
    expect(st.done).toBe(true);
    // A stale count of 1 does not hide the third comment.
    expect(collapsedView(st, none, 1, rules).more).toEqual({ kind: "count", n: 1 });
  });

  it("never goes below zero on a stale count", () => {
    const st = withPage(EMPTY_THREAD, [comment(1), comment(2)], 2, "page", COMMENT_PAGE);
    expect(collapsedView(st, none, 1, rules).more).toBeNull();
  });
});

// The open channel's tick (UX 13.2): the same table as iOS
// `ChannelCommentRefreshTests` and Android `CommentRefreshTest`.
describe("commentRefresh", () => {
  const cases: ReadonlyArray<[string, Parameters<typeof commentRefresh>, string]> = [
    ["nothing before the first read", [false, false, false, 0, null, 3], "none"],
    ["no count, open, read to the end", [true, true, false, 2, null, undefined], "newer"],
    ["no count, collapsed", [true, false, false, 2, null, undefined], "none"],
    ["no count, open, pages unread", [true, true, true, 20, null, undefined], "none"],
    ["count as read", [true, false, false, 2, 2, 2], "none"],
    ["count up, collapsed", [true, false, false, 1, 1, 2], "newer"],
    ["count up, open", [true, true, false, 1, 1, 2], "newer"],
    ["count up, pages unread", [true, true, true, 20, 25, 26], "none"],
    ["count down, collapsed", [true, false, false, 3, 3, 2], "reread"],
    ["count down, open", [true, true, false, 3, 3, 2], "rebase"],
    ["no count at the read: the rows read stand in", [true, false, false, 2, null, 2], "none"],
    ["no count at the read, one more now", [true, false, false, 2, null, 3], "newer"],
  ];
  for (const [name, args, want] of cases) {
    it(name, () => expect(commentRefresh(...args)).toBe(want));
  }

  it("a comment read into the thread no longer waits behind the new line", () => {
    let st = withPage(EMPTY_THREAD, [comment(1)], 1, "page");
    st = { ...st, expanded: true };
    st = withPage(st, [comment(2)], 2, "poll");
    expect(openView(st, none).arrived).toBe(1);
    st = withPage(st, [comment(2)], 2, "page");
    expect(openView(st, none).arrived).toBe(0);
    expect(st.rows.map((c) => c.cursor)).toEqual([1, 2]);
  });
});
