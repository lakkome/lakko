// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { channelTimeline } from "./channel-timeline";
import type { PostDecode } from "./post-decode";

const text = (s: string): PostDecode => ({ kind: "text", text: s, body: { kind: "chat", text: s, attachments: [] } });
const page = (...cursors: number[]): { cursor: number; serverTsMs: number }[] =>
  cursors.map((c) => ({ cursor: c, serverTsMs: c * 1000 }));

describe("channelTimeline", () => {
  it("orders posts by cursor and hides records", () => {
    const decoded = new Map<number, PostDecode>([
      [3, text("c")],
      [1, text("a")],
      [2, { kind: "meta", meta: { name: "n", description: "", updatedAtMs: 1n } }],
      [4, { kind: "ban-list", plaintext: new Uint8Array() }],
    ]);
    const t = channelTimeline(page(3, 1, 2, 4), decoded);
    expect(t.posts.map((p) => p.text)).toEqual(["a", "c"]);
    expect(t.earlierUnavailable).toBe(false);
  });

  it("applies the newest edit and a delete, never a forward reference", () => {
    const decoded = new Map<number, PostDecode>([
      [1, text("first")],
      [2, text("second")],
      [3, { kind: "op", body: { kind: "edit", target: 1, text: "first, fixed" } }],
      [4, { kind: "op", body: { kind: "edit", target: 1, text: "first, fixed again" } }],
      [5, { kind: "op", body: { kind: "delete", target: 2 } }],
      [6, { kind: "op", body: { kind: "delete", target: 9 } }],
    ]);
    const t = channelTimeline(page(1, 2, 3, 4, 5, 6), decoded);
    expect(t.posts).toHaveLength(1);
    expect(t.posts[0]?.text).toBe("first, fixed again");
    expect(t.posts[0]?.edited).toBe(true);
  });

  it("keeps a deleted post in its hold as a deleted card, and only then", () => {
    // message-actions.md section 9.
    const decoded = new Map<number, PostDecode>([
      [1, text("first")],
      [2, text("second")],
      [3, { kind: "op", body: { kind: "delete", target: 2 } }],
    ]);
    const held = channelTimeline(page(1, 2, 3), decoded, new Set([2]));
    expect(held.posts.map((p) => [p.cursor, p.deleted === true, p.text])).toEqual([
      [1, false, "first"],
      [2, true, ""],
    ]);
    expect(channelTimeline(page(1, 2, 3), decoded).posts.map((p) => p.cursor)).toEqual([1]);
  });

  it("folds unreadable posts at the start into one line", () => {
    const decoded = new Map<number, PostDecode>([
      [1, { kind: "older-key" }],
      [2, { kind: "older-key" }],
      [3, text("readable")],
      [4, { kind: "unreadable" }],
    ]);
    const t = channelTimeline(page(1, 2, 3, 4), decoded);
    expect(t.earlierUnavailable).toBe(true);
    expect(t.posts.map((p) => p.state)).toEqual(["ok", "unavailable"]);
  });

  it("shows a post still decoding as loading", () => {
    const t = channelTimeline(page(1), new Map());
    expect(t.posts[0]?.state).toBe("loading");
  });
});

describe("channelDayLabels", () => {
  const day = (iso: string): string => iso.slice(0, 10);
  const same = (a: string, b: string): boolean => day(a) === day(b);
  it("puts the separator before the first post of each day, never after it", async () => {
    const { channelDayLabels } = await import("./channel-timeline");
    const posts = [
      { serverTsMs: Date.UTC(2026, 10, 1, 12) },
      { serverTsMs: Date.UTC(2026, 10, 1, 13) },
      { serverTsMs: Date.UTC(2026, 10, 2, 9) },
    ];
    expect(channelDayLabels(posts, day, same)).toEqual(["2026-11-01", null, "2026-11-02"]);
    expect(channelDayLabels([], day, same)).toEqual([]);
  });
});
