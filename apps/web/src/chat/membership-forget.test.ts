// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 section 9 — the per-group half of the stores that keep one row per
 * account: a group goes, the others stay, and the row on disk follows.
 */
import { describe, expect, it } from "vitest";

import { emptyFeedMarks, withoutConversation } from "../feed/feed-marks";
import { MEMORY_GROUP_NAME_BACKEND, GroupNameStore } from "./group-name-store";
import { allowGroupInMessageLog, blockGroupInMessageLog, withoutBlockedSlots, type MessageLogPayload } from "./message-log-store";
import { ReadWatermarkStore, MEMORY_READ_WATERMARK_BACKEND } from "./read-watermark-store";

describe("the message log, in a page where a group ended", () => {
  const payload = (): MessageLogPayload =>
    ({ version: 5, groups: { aa: { cursor: 1, messages: [] }, bb: { cursor: 2, messages: [] } } }) as never;

  it("leaves the group out of every write, and takes it again after a join", () => {
    expect(Object.keys(withoutBlockedSlots("acct", payload()).groups)).toEqual(["aa", "bb"]);
    blockGroupInMessageLog("acct", "aa");
    expect(Object.keys(withoutBlockedSlots("acct", payload()).groups)).toEqual(["bb"]);
    // Another account on this browser is its own.
    expect(Object.keys(withoutBlockedSlots("other", payload()).groups)).toEqual(["aa", "bb"]);
    allowGroupInMessageLog("acct", "aa");
    expect(Object.keys(withoutBlockedSlots("acct", payload()).groups)).toEqual(["aa", "bb"]);
  });

  it("keeps the held sends on a write it trims", () => {
    blockGroupInMessageLog("acct", "aa");
    const held = [{ conversationHex: "bb" }] as never;
    expect(withoutBlockedSlots("acct", { ...payload(), held }).held).toBe(held);
    allowGroupInMessageLog("acct", "aa");
  });
});

describe("the group name", () => {
  it("forgets one group", async () => {
    const store = new GroupNameStore(MEMORY_GROUP_NAME_BACKEND);
    await store.open("acct");
    await store.set("acct", "gone", { name: "Book club", updatedAtMs: 1n } as never);
    await store.set("acct", "stays", { name: "Other", updatedAtMs: 1n } as never);
    await store.forget("acct", "gone");
    expect(store.nameOf("gone")).toBeNull();
    expect(store.nameOf("stays")).toBe("Other");
  });
});

describe("the read watermarks", () => {
  it("forgets one group", async () => {
    const store = new ReadWatermarkStore(MEMORY_READ_WATERMARK_BACKEND);
    await store.open("acct");
    store.ingest("acct", "gone", "sender", 5);
    store.ingest("acct", "stays", "sender", 5);
    store.forget("acct", "gone");
    await store.flush();
    expect(store.of("gone").size).toBe(0);
    expect(store.of("stays").size).toBe(1);
  });
});

describe("the feed marks", () => {
  it("drop a conversation's hidden mark and a channel's read position", () => {
    const marks = { ...emptyFeedMarks(), hidden: ["aa", "bb"], channelRead: { aa: 3, cc: 4 } };
    const next = withoutConversation(marks, "aa");
    expect(next.hidden).toEqual(["bb"]);
    expect(next.channelRead).toEqual({ cc: 4 });
  });

  it("are the same object when nothing was held", () => {
    const marks = emptyFeedMarks();
    expect(withoutConversation(marks, "aa")).toBe(marks);
  });
});
