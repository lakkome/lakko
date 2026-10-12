// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { mergeForegroundSlot, summarizeGroupSlot } from "../chat/background-log";
import type { PersistedGroup, PersistedMessage } from "../chat/message-log-store";
import { isSeenInSlot, markSeenInSlot } from "./feed-seen";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const contiguous = (ids: BigInt64Array, seen: BigInt64Array): bigint | undefined =>
  wasm.contiguousSeen(ids, seen);

function m(id: number, direction: PersistedMessage["direction"] = "incoming"): PersistedMessage {
  return { messageId: String(id), direction, text: "x", attachments: [], createdAt: "2026-09-29T10:00:00Z" };
}

function slot(extra: Partial<PersistedGroup> = {}): PersistedGroup {
  return { cursor: "5", messages: [m(1), m(2, "outgoing"), m(3), m(4), m(5)], ...extra };
}

describe("markSeenInSlot", () => {
  it("keeps a gap as ids and never moves the watermark past an unseen message", () => {
    // 2 is the person's own, so the watermark moves to it and stops at 3.
    const res = markSeenInSlot(slot({ lastViewedMessageId: "1" }), ["4"], contiguous);
    expect(res.advancedTo).toBe(2n);
    expect(res.slot.lastViewedMessageId).toBe("2");
    expect(res.slot.feedSeen).toEqual(["4"]);
    const again = markSeenInSlot(res.slot, ["4"], contiguous);
    expect(again.advancedTo).toBeNull();
  });

  it("closing the gap folds the ids into the watermark (own rows count as seen)", () => {
    const first = markSeenInSlot(slot({ lastViewedMessageId: "1" }), ["4"], contiguous).slot;
    const res = markSeenInSlot(first, ["3"], contiguous);
    expect(res.advancedTo).toBe(4n);
    expect(res.slot.lastViewedMessageId).toBe("4");
    expect(res.slot.feedSeen).toBeUndefined();
  });

  it("ignores ids that name no message and never moves the watermark back", () => {
    const res = markSeenInSlot(slot({ lastViewedMessageId: "5" }), ["99", "3"], contiguous);
    expect(res.advancedTo).toBeNull();
    expect(res.slot.lastViewedMessageId).toBe("5");
    expect(res.slot.feedSeen).toBeUndefined();
  });

  it("a never-opened chat advances from its first message", () => {
    const res = markSeenInSlot(slot(), ["1"], contiguous);
    expect(res.advancedTo).toBe(2n);
  });
});

describe("unread and the chat's own write", () => {
  it("a message the feed showed is not unread, and the view keeps it", () => {
    const s = slot({ lastViewedMessageId: "1", feedSeen: ["4"] });
    expect(summarizeGroupSlot(s, null).unread).toBe(2);
    expect(isSeenInSlot(s, m(4))).toBe(true);
    expect(isSeenInSlot(s, m(3))).toBe(false);

    const merged = mergeForegroundSlot(s, { messages: s.messages.slice(0, 2), cursor: 2 });
    expect(merged.feedSeen).toEqual(["4"]);
    const covered = mergeForegroundSlot(s, { messages: s.messages, cursor: 5 });
    expect(covered.feedSeen).toBeUndefined();
  });
});
