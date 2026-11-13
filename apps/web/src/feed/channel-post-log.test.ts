// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  CHANNEL_POST_CAP,
  CHANNEL_POST_LOG_VERSION,
  CHANNEL_POST_PAGE,
  CHANNEL_POST_RETENTION_MS,
  decodeChannelPostLog,
  mergeChannelPosts,
  retainChannels,
  syncChannelSlot,
  type ChannelPostPage,
  type ChannelSyncDeps,
  type PostOutcome,
} from "./channel-post-log";

const NOW = Date.parse("2026-09-29T12:00:00Z");
const ID = "ab".repeat(16);

describe("decodeChannelPostLog", () => {
  it("refuses an unknown version and drops malformed entries alone", () => {
    expect(decodeChannelPostLog({ version: 99, channels: {} })).toBeNull();
    const log = decodeChannelPostLog({
      version: CHANNEL_POST_LOG_VERSION,
      channels: {
        [ID]: {
          cursor: 3,
          posts: [
            { cursor: 3, serverTsMs: NOW, text: "b" },
            { cursor: 0, serverTsMs: NOW, text: "zero cursor" },
            { cursor: 2, serverTsMs: NOW, text: 5 },
            { cursor: 1, serverTsMs: NOW, text: "a" },
          ],
        },
        "not-an-id": { cursor: 1, posts: [] },
        ["cd".repeat(16)]: { cursor: -1, posts: [] },
      },
    });
    expect(Object.keys(log?.channels ?? {})).toEqual([ID]);
    expect(log?.channels[ID]?.posts.map((p) => p.text)).toEqual(["a", "b"]);
  });
});

describe("mergeChannelPosts", () => {
  it("dedupes, orders, drops the expired, caps and never moves the cursor back", () => {
    const old = { cursor: 1, serverTsMs: NOW - CHANNEL_POST_RETENTION_MS - 1, text: "old" };
    const slot = mergeChannelPosts(
      { cursor: 10, posts: [old, { cursor: 5, serverTsMs: NOW, text: "x" }] },
      [{ cursor: 5, serverTsMs: NOW, text: "x2" }, { cursor: 4, serverTsMs: NOW, text: "y" }],
      8,
      NOW,
    );
    expect(slot.cursor).toBe(10);
    expect(slot.posts.map((p) => p.text)).toEqual(["y", "x2"]);

    const many = Array.from({ length: CHANNEL_POST_CAP + 5 }, (_, i) => ({
      cursor: i + 1,
      serverTsMs: NOW,
      text: String(i),
    }));
    const capped = mergeChannelPosts(undefined, many, many.length, NOW);
    expect(capped.posts).toHaveLength(CHANNEL_POST_CAP);
    expect(capped.posts[0]?.cursor).toBe(6);
  });
});

describe("retainChannels", () => {
  it("keeps only the listed channels", () => {
    const log = { version: 1, channels: { [ID]: { cursor: 0, posts: [] }, x: { cursor: 0, posts: [] } } };
    expect(Object.keys(retainChannels(log, new Set([ID])).channels)).toEqual([ID]);
  });
});

function deps(
  pages: ChannelPostPage[],
  outcome: (payload: string) => PostOutcome,
): ChannelSyncDeps & { afters: number[] } {
  const afters: number[] = [];
  let i = 0;
  return {
    afters,
    fetchPage: async (_id, after) => {
      afters.push(after);
      return pages[i++] ?? { posts: [], nextAfter: after };
    },
    decode: async (_id, payload) => outcome(payload),
  };
}

const post = (cursor: number, payloadB64: string) => ({ cursor, serverTsMs: NOW, payloadB64 });

describe("syncChannelSlot", () => {
  it("keeps text, passes over skips, and advances past both", async () => {
    const d = deps([{ posts: [post(1, "t:a"), post(2, "skip"), post(3, "t:b")], nextAfter: 3 }], (p) =>
      p.startsWith("t:") ? { kind: "text", text: p.slice(2) } : { kind: "skip" },
    );
    const slot = await syncChannelSlot(ID, undefined, d, NOW);
    expect(slot.cursor).toBe(3);
    expect(slot.posts.map((p) => p.text)).toEqual(["a", "b"]);
    expect(d.afters).toEqual([0]);
  });

  it("stops before a post waiting for its seed and resumes there next time", async () => {
    const first = deps([{ posts: [post(1, "t:a"), post(2, "wait"), post(3, "t:c")], nextAfter: 3 }], (p) =>
      p === "wait" ? { kind: "wait" } : { kind: "text", text: p.slice(2) },
    );
    const slot = await syncChannelSlot(ID, undefined, first, NOW);
    expect(slot.cursor).toBe(1);
    expect(slot.posts.map((p) => p.text)).toEqual(["a"]);

    const second = deps([{ posts: [post(2, "t:b"), post(3, "t:c")], nextAfter: 3 }], (p) => ({
      kind: "text",
      text: p.slice(2),
    }));
    const next = await syncChannelSlot(ID, slot, second, NOW);
    expect(second.afters).toEqual([1]);
    expect(next.posts.map((p) => p.text)).toEqual(["a", "b", "c"]);
  });

  it("pages on while a page is full", async () => {
    const full = Array.from({ length: CHANNEL_POST_PAGE }, (_, i) => post(i + 1, "t:x"));
    const d = deps(
      [
        { posts: full, nextAfter: CHANNEL_POST_PAGE },
        { posts: [post(CHANNEL_POST_PAGE + 1, "t:y")], nextAfter: CHANNEL_POST_PAGE + 1 },
      ],
      (p) => ({ kind: "text", text: p.slice(2) }),
    );
    const slot = await syncChannelSlot(ID, undefined, d, NOW);
    expect(d.afters).toEqual([0, CHANNEL_POST_PAGE]);
    expect(slot.cursor).toBe(CHANNEL_POST_PAGE + 1);
  });
});

describe("attachment kinds in the log", () => {
  it("keeps a caption-less photo, file or GIF post with its attachments' kinds", async () => {
    const d = deps([{ posts: [post(1, "t:hello"), post(2, "gif")], nextAfter: 2 }], (p) =>
      p === "gif"
        ? { kind: "text", text: "", media: [{ mime: "image/gif", kind: "gif" }] }
        : { kind: "text", text: p.slice(2) },
    );
    const slot = await syncChannelSlot(ID, undefined, d, NOW);
    expect(slot.posts.at(-1)).toEqual({
      cursor: 2,
      serverTsMs: NOW,
      text: "",
      media: [{ mime: "image/gif", kind: "gif" }],
    });
    expect(slot.posts[0]).not.toHaveProperty("media");
  });

  it("round-trips the kinds through the stored form and drops malformed ones", () => {
    const log = decodeChannelPostLog({
      version: CHANNEL_POST_LOG_VERSION,
      channels: {
        [ID]: {
          cursor: 2,
          posts: [
            { cursor: 1, serverTsMs: NOW, text: "", media: [{ mime: "image/jpeg", kind: 7 }, "bad"] },
            { cursor: 2, serverTsMs: NOW, text: "", media: "not a list" },
          ],
        },
      },
    });
    expect(log?.channels[ID]?.posts).toEqual([
      { cursor: 1, serverTsMs: NOW, text: "", media: [{ mime: "image/jpeg", kind: null }] },
      { cursor: 2, serverTsMs: NOW, text: "" },
    ]);
  });
});
