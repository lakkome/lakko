// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  appendToGroupSlot,
  eventToPersistedMessage,
  mergeForegroundSlot,
  summarizeGroupSlot,
} from "./background-log";
import type { PersistedGroup, PersistedMessage } from "./message-log-store";

function msg(
  id: number,
  direction: PersistedMessage["direction"] = "incoming",
  text = `m${id}`,
): PersistedMessage {
  return { messageId: String(id), direction, text, attachments: [], createdAt: "t" };
}

describe("appendToGroupSlot (the background receive's write)", () => {
  it("adds new rows, keeps existing ones, and sorts by DS id", () => {
    const slot: PersistedGroup = { cursor: "3", messages: [msg(1, "outgoing"), msg(3)] };
    const next = appendToGroupSlot(slot, [msg(5), msg(4)], 5);
    expect(next.messages.map((m) => m.messageId)).toEqual(["1", "3", "4", "5"]);
    expect(next.cursor).toBe("5");
  });

  it("does not duplicate a row it already holds", () => {
    const slot: PersistedGroup = { cursor: "4", messages: [msg(4)] };
    const next = appendToGroupSlot(slot, [msg(4), msg(4)], 4);
    expect(next.messages).toHaveLength(1);
  });

  it("never moves the cursor backward", () => {
    const slot: PersistedGroup = { cursor: "9", messages: [] };
    expect(appendToGroupSlot(slot, [], 2).cursor).toBe("9");
  });

  it("preserves lastViewedMessageId", () => {
    const slot: PersistedGroup = { cursor: "1", messages: [], lastViewedMessageId: "1" };
    expect(appendToGroupSlot(slot, [msg(2)], 2).lastViewedMessageId).toBe("1");
  });

  it("creates a slot for a chat never persisted before", () => {
    const next = appendToGroupSlot(undefined, [msg(7)], 7);
    expect(next).toEqual({ cursor: "7", messages: [msg(7)] });
  });
});

describe("mergeForegroundSlot (the chat view's write)", () => {
  it("keeps rows the background wrote after the view's newest row", () => {
    // The race this exists for: the view's last debounced write lands AFTER
    // the background receive filed new rows for the same chat.
    const stored: PersistedGroup = {
      cursor: "12",
      messages: [msg(10), msg(11), msg(12)],
    };
    const next = mergeForegroundSlot(stored, { messages: [msg(10), msg(11)], cursor: 11 });
    expect(next.messages.map((m) => m.messageId)).toEqual(["10", "11", "12"]);
    expect(next.cursor).toBe("12");
  });

  it("stays authoritative for what it holds (a row it dropped is not resurrected)", () => {
    const stored: PersistedGroup = { cursor: "3", messages: [msg(1), msg(2), msg(3)] };
    const next = mergeForegroundSlot(stored, { messages: [msg(1), msg(3)], cursor: 3 });
    expect(next.messages.map((m) => m.messageId)).toEqual(["1", "3"]);
  });

  it("records the newest row the view held as viewed, never lowering it", () => {
    const first = mergeForegroundSlot(undefined, { messages: [msg(4), msg(6)], cursor: 6 });
    expect(first.lastViewedMessageId).toBe("6");
    const again = mergeForegroundSlot(
      { ...first, lastViewedMessageId: "9" },
      { messages: [msg(4)], cursor: 6 },
    );
    expect(again.lastViewedMessageId).toBe("9");
  });
});

describe("summarizeGroupSlot (the chat list row)", () => {
  it("counts every incoming row of a chat this device has never displayed", () => {
    // The old badge returned 0 with no synced read cursor, so a chat filled by
    // the background receive showed nothing.
    const slot: PersistedGroup = { cursor: "3", messages: [msg(1), msg(2), msg(3)] };
    expect(summarizeGroupSlot(slot, null).unread).toBe(3);
  });

  it("counts only rows above the later of the synced read cursor and the last viewed row", () => {
    const slot: PersistedGroup = {
      cursor: "6",
      messages: [msg(1), msg(2), msg(3), msg(4), msg(5), msg(6)],
      lastViewedMessageId: "4",
    };
    expect(summarizeGroupSlot(slot, 2n).unread).toBe(2);
    expect(summarizeGroupSlot(slot, 5n).unread).toBe(1);
  });

  it("never counts this device's own messages", () => {
    const slot: PersistedGroup = { cursor: "2", messages: [msg(1, "outgoing"), msg(2, "outgoing")] };
    expect(summarizeGroupSlot(slot, null).unread).toBe(0);
  });

  it("previews the newest non-system message on one line", () => {
    const slot: PersistedGroup = {
      cursor: "3",
      messages: [msg(1), msg(2, "incoming", "hello\n  there"), msg(3, "system", "[mls commit]")],
    };
    const s = summarizeGroupSlot(slot, null);
    expect(s.preview).toBe("hello there");
    expect(s.previewIsOwn).toBe(false);
  });

  it("labels an attachment-only message and truncates long text", () => {
    const photo: PersistedMessage = {
      ...msg(1, "outgoing", ""),
      attachments: [
        { blobIdB64: "x", keyB64: "y", nonceB64: "z", mime: "image/png", sizeStr: "1", name: null },
      ],
    };
    expect(summarizeGroupSlot({ cursor: "1", messages: [photo] }, null)).toEqual({
      unread: 0,
      preview: "Photo",
      previewIsOwn: true,
      lastAt: photo.createdAt,
      lastAuthorKeyB64: null,
    });
    const long = msg(2, "incoming", "x".repeat(200));
    const p = summarizeGroupSlot({ cursor: "2", messages: [long] }, null).preview ?? "";
    expect(p.length).toBe(80);
  });

  it("has no preview and no unread for an empty or missing slot", () => {
    expect(summarizeGroupSlot(undefined, null)).toEqual({
      unread: 0,
      preview: null,
      previewIsOwn: false,
      lastAt: null,
      lastAuthorKeyB64: null,
    });
  });
});

describe("eventToPersistedMessage", () => {
  it("persists the author as the signature key resolved at receipt, never the leaf", () => {
    const row = eventToPersistedMessage({
      kind: "application",
      messageId: 42,
      text: "hi",
      plaintext: "hi",
      attachments: [],
      senderLeaf: 3,
      senderKey: "c2lnbmF0dXJlLWtleQ==" as never,
      createdAt: "2026-09-13T12:00:00Z",
    });
    expect(row).toEqual({
      messageId: "42",
      direction: "incoming",
      text: "hi",
      attachments: [],
      createdAt: "2026-09-13T12:00:00Z",
      authorKeyB64: "c2lnbmF0dXJlLWtleQ==",
    });
    expect(JSON.stringify(row)).not.toContain("senderLeaf");
  });

  it("persists a system row the way the chat view does", () => {
    const row = eventToPersistedMessage({
      kind: "system",
      messageId: 7,
      systemKind: "commit",
      createdAt: "t",
    });
    expect(row).toEqual({
      messageId: "7",
      direction: "system",
      text: "[mls commit]",
      attachments: [],
      createdAt: "t",
    });
  });
});
