// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  appendToGroupSlot,
  ClosedViewBaseline,
  eventToPersistedMessage,
  fileHistoryRows,
  mergeClosedViewSlot,
  mergeForegroundSlot,
  summarizeGroupSlot,
} from "./background-log";
import { markSeenInSlot } from "../feed/feed-seen";
import { withoutGroup, type PersistedGroup, type PersistedMessage } from "./message-log-store";

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

  it("keeps a stored row the view does not hold, below its newest row too", () => {
    // 2026-11-20: a one-to-one lost the other person's "hello" after they
    // deleted their account. A view that has not loaded a stored row and then
    // writes a newer one of its own must not erase it: the view never removes
    // a row, so a stored row it lacks is one it did not load.
    const stored: PersistedGroup = { cursor: "3", messages: [msg(1), msg(2), msg(3)] };
    const next = mergeForegroundSlot(stored, { messages: [msg(1), msg(3), msg(4, "outgoing")], cursor: 4 });
    expect(next.messages.map((m) => m.messageId)).toEqual(["1", "2", "3", "4"]);
  });

  it("writes the view's copy of a row it holds over the stored one", () => {
    const stored: PersistedGroup = { cursor: "2", messages: [msg(1, "incoming", "old"), msg(2)] };
    const next = mergeForegroundSlot(stored, { messages: [msg(1, "incoming", "edited")], cursor: 2 });
    expect(next.messages.map((m) => [m.messageId, m.text])).toEqual([
      ["1", "edited"],
      ["2", "m2"],
    ]);
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

  it("treats a row this account sent from another device as its own", () => {
    // Base scenario 33: the list read "Ada Two: ..." and counted the line as
    // unread, while the timeline drew it as the person's own.
    const own = "b3duLWFjY291bnQta2V5";
    const fromOtherDevice: PersistedMessage = { ...msg(2, "incoming", "from my laptop"), authorKeyB64: own };
    const slot: PersistedGroup = { cursor: "2", messages: [msg(1), fromOtherDevice] };
    const s = summarizeGroupSlot(slot, 1n, own);
    expect(s.unread).toBe(0);
    expect(s.previewIsOwn).toBe(true);
    expect(s.lastAuthorKeyB64).toBeNull();
    // Without the key the old reading stands: someone else's, unread.
    const before = summarizeGroupSlot(slot, 1n);
    expect(before.unread).toBe(1);
    expect(before.previewIsOwn).toBe(false);
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

  it("never previews a deleted message: falls back to the newest one left", () => {
    // docs/ux/message-actions.md section 9 — the preview never says "Message
    // deleted"; it shows the newest message that is not deleted, with that
    // message's time.
    const slot: PersistedGroup = {
      cursor: "3",
      messages: [
        { ...msg(1, "incoming", "still here"), createdAt: "t1" },
        { ...msg(2, "incoming", "the secret"), createdAt: "t2", deleted: true },
        { ...msg(3, "incoming", ""), createdAt: "t3", deleted: true },
      ],
    };
    const s = summarizeGroupSlot(slot, null);
    expect(s.preview).toBe("still here");
    expect(s.lastAt).toBe("t1");
  });

  it("previews nothing when every message was deleted", () => {
    const slot: PersistedGroup = {
      cursor: "2",
      messages: [{ ...msg(2, "incoming", "the secret"), deleted: true }],
    };
    const s = summarizeGroupSlot(slot, null);
    expect(s.preview).toBeNull();
    expect(s.lastAt).toBeNull();
  });

  it("never counts a deleted message as unread", () => {
    const slot: PersistedGroup = {
      cursor: "3",
      messages: [msg(1), { ...msg(2), deleted: true }, { ...msg(3), deleted: true }],
      lastViewedMessageId: "1",
    };
    expect(summarizeGroupSlot(slot, null).unread).toBe(0);
    const { lastViewedMessageId: _viewed, ...some } = slot;
    expect(summarizeGroupSlot(some, null).unread).toBe(1);
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
      // docs/ux/media-messages.md section 11 — the row's thumbnail.
      previewMedia: { thumb: null, video: false, count: 1 },
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

  it("persists the ADR-0052 reply reference verbatim, and omits it when absent", () => {
    const base = {
      kind: "application" as const,
      messageId: 43,
      text: "yes",
      plaintext: "yes",
      attachments: [],
      senderLeaf: undefined,
      senderKey: undefined,
      createdAt: "t",
    };
    const withReply = eventToPersistedMessage({
      ...base,
      reply: { target: 42, author: "Anna", excerpt: "at five?" },
    });
    expect(withReply.reply).toEqual({ target: "42", author: "Anna", excerpt: "at five?" });
    const bare = eventToPersistedMessage({ ...base, reply: { target: 42 } });
    expect(bare.reply).toEqual({ target: "42" });
    expect(Object.keys(bare.reply ?? {})).toEqual(["target"]);
    const plain = eventToPersistedMessage(base);
    expect("reply" in plain).toBe(false);
  });

  it("persists the ADR-0052 forward attribution verbatim, and omits it when absent", () => {
    const base = {
      kind: "application" as const,
      messageId: 44,
      text: "fwd",
      plaintext: "fwd",
      attachments: [],
      senderLeaf: undefined,
      senderKey: undefined,
      createdAt: "t",
    };
    const full = eventToPersistedMessage({
      ...base,
      forward: { author: "Anna", origin: { groupIdB64: "Zw==", messageId: 41 } },
    });
    expect(full.forward).toEqual({
      author: "Anna",
      origin: { groupIdB64: "Zw==", messageId: "41" },
    });
    const nameOnly = eventToPersistedMessage({ ...base, forward: { author: "Anna" } });
    expect(nameOnly.forward).toEqual({ author: "Anna" });
    const plain = eventToPersistedMessage(base);
    expect("forward" in plain).toBe(false);
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

describe("mergeClosedViewSlot (a closed view's late write)", () => {
  it("adds an own send that completed after the view closed", () => {
    const slot: PersistedGroup = { cursor: "3", messages: [msg(1), msg(3)], lastViewedMessageId: "3" };
    const next = mergeClosedViewSlot(slot, [msg(4, "outgoing")], 3);
    expect(next.messages.map((m) => m.messageId)).toEqual(["1", "3", "4"]);
  });

  it("keeps a row the background receive filed below the late send", () => {
    // The view closed at 3; the background receive filed 5; the send that was
    // in flight came back as 6. The view never held 5 and must not erase it
    // (since 2026-11-20 its ordinary merge keeps it too).
    const slot: PersistedGroup = { cursor: "5", messages: [msg(3), msg(5)] };
    const next = mergeClosedViewSlot(slot, [msg(6, "outgoing")], 3);
    expect(next.messages.map((m) => m.messageId)).toEqual(["3", "5", "6"]);
    expect(mergeForegroundSlot(slot, { messages: [msg(3), msg(6, "outgoing")], cursor: 3 })
      .messages.map((m) => m.messageId)).toEqual(["3", "5", "6"]);
  });

  it("replaces the stored row an edit changed, and only that row", () => {
    const slot: PersistedGroup = { cursor: "4", messages: [msg(2), msg(4, "outgoing", "before")] };
    const next = mergeClosedViewSlot(slot, [{ ...msg(4, "outgoing", "after"), edited: true }], 4);
    expect(next.messages.map((m) => m.text)).toEqual(["m2", "after"]);
    expect(next.messages[0]).toBe(slot.messages[0]);
  });

  it("does not mark anything as viewed: nobody is looking", () => {
    const slot: PersistedGroup = { cursor: "3", messages: [msg(3)], lastViewedMessageId: "3" };
    const next = mergeClosedViewSlot(slot, [msg(7)], 7);
    expect(next.lastViewedMessageId).toBe("3");
    expect(summarizeGroupSlot(next, null).unread).toBe(1);
    expect(mergeClosedViewSlot(undefined, [msg(7)], 7).lastViewedMessageId).toBeUndefined();
  });

  it("never moves the cursor backward, and keeps the slot's other fields", () => {
    const slot: PersistedGroup = { cursor: "9", messages: [], feedSeen: ["8"] };
    const next = mergeClosedViewSlot(slot, [], 2);
    expect(next.cursor).toBe("9");
    expect(next.feedSeen).toEqual(["8"]);
    expect(mergeClosedViewSlot(slot, [], 12).cursor).toBe("12");
  });

  it("writes a row named twice once, in its last form", () => {
    const next = mergeClosedViewSlot(undefined, [msg(2, "outgoing", "a"), msg(2, "outgoing", "b")], 2);
    expect(next.messages.map((m) => m.text)).toEqual(["b"]);
  });
});

describe("ClosedViewBaseline", () => {
  it("reports nothing for the rows the view held when it closed", () => {
    const b = new ClosedViewBaseline([msg(1), msg(2)], 2);
    // Re-derived objects, same content.
    expect(b.changed([msg(1), msg(2)])).toEqual([]);
    expect(b.cursorAdvanced(2)).toBe(false);
  });

  it("reports a new row and a changed row, and not the untouched ones", () => {
    const b = new ClosedViewBaseline([msg(1), msg(2)], 2);
    const changed = b.changed([msg(1), msg(2, "incoming", "edited"), msg(3, "outgoing")]);
    expect(changed.map((m) => m.messageId)).toEqual(["2", "3"]);
  });

  it("stops reporting a row once its write was accepted, and reports it again on failure", () => {
    const b = new ClosedViewBaseline([], 0);
    const rows = [msg(5, "outgoing")];
    // Not accepted (the write failed): still owed.
    expect(b.changed(rows)).toHaveLength(1);
    expect(b.changed(rows)).toHaveLength(1);
    b.accept(rows, 5);
    expect(b.changed(rows)).toEqual([]);
    expect(b.cursorAdvanced(5)).toBe(false);
    expect(b.cursorAdvanced(6)).toBe(true);
  });
});

describe("the history floor (docs/ux/second-device.md section 2)", () => {
  it("files the rows the slot lacks, in order, and raises the floor", () => {
    const stored: PersistedGroup = { cursor: "9", messages: [msg(4)] };
    const { slot, added } = fileHistoryRows(stored, [msg(3), msg(4, "incoming", "theirs"), msg(1)], 4n);
    expect(added).toBe(2);
    expect(slot.messages.map((m) => m.messageId)).toEqual(["1", "3", "4"]);
    expect(slot.messages[2]?.text).toBe("m4");
    expect(slot.cursor).toBe("9");
    expect(slot.syncedThroughMessageId).toBe("4");
  });

  it("creates the slot of a group this device has no log for", () => {
    const { slot } = fileHistoryRows(undefined, [msg(2)], 2n);
    expect(slot).toEqual({ cursor: "0", messages: [msg(2)], syncedThroughMessageId: "2" });
  });

  it("keeps the higher floor and what the slot says about this device", () => {
    const stored: PersistedGroup = {
      cursor: "9",
      messages: [msg(9)],
      lastViewedMessageId: "7",
      feedSeen: ["9"],
      syncedThroughMessageId: "8",
    };
    const { slot } = fileHistoryRows(stored, [msg(2)], 3n);
    expect(slot.syncedThroughMessageId).toBe("8");
    expect(slot.lastViewedMessageId).toBe("7");
    expect(slot.feedSeen).toEqual(["9"]);
  });

  it("an empty bundle sets no floor", () => {
    expect(fileHistoryRows(undefined, [], null).slot.syncedThroughMessageId).toBeUndefined();
  });

  const history = (): PersistedGroup =>
    fileHistoryRows(undefined, [msg(1), msg(2), msg(3)], 3n).slot;

  it("a history row is not unread, with no read position or a lower one", () => {
    expect(summarizeGroupSlot(history(), null).unread).toBe(0);
    expect(summarizeGroupSlot(history(), 1n).unread).toBe(0);
    expect(summarizeGroupSlot({ ...history(), lastViewedMessageId: "1" }, null).unread).toBe(0);
  });

  it("what arrives after it counts, and a read position still covers it", () => {
    const next = appendToGroupSlot(history(), [msg(4), msg(5)], 5);
    expect(summarizeGroupSlot(next, null).unread).toBe(2);
    expect(summarizeGroupSlot(next, 4n).unread).toBe(1);
    expect(summarizeGroupSlot(next, 5n).unread).toBe(0);
  });

  it("covers a row that came over the journal before the bundle did", () => {
    const live = appendToGroupSlot(undefined, [msg(3), msg(4)], 4);
    const { slot } = fileHistoryRows(live, [msg(1), msg(3)], 3n);
    expect(summarizeGroupSlot(slot, null).unread).toBe(1);
  });

  it("moves no read position: the slot has no lastViewedMessageId", () => {
    expect(history().lastViewedMessageId).toBeUndefined();
  });

  it("an unreadable floor counts for nothing", () => {
    const slot: PersistedGroup = { cursor: "1", messages: [msg(1)], syncedThroughMessageId: "x" };
    expect(summarizeGroupSlot(slot, null).unread).toBe(1);
  });

  it("every writer of the slot keeps it, and so does a restart", () => {
    const floor = "3";
    const slot = history();
    expect(appendToGroupSlot(slot, [msg(4)], 4).syncedThroughMessageId).toBe(floor);
    expect(mergeForegroundSlot(slot, { messages: [msg(3)], cursor: 3 }).syncedThroughMessageId).toBe(floor);
    expect(mergeClosedViewSlot(slot, [msg(4)], 4).syncedThroughMessageId).toBe(floor);
    expect(markSeenInSlot(slot, ["3"], () => undefined).slot.syncedThroughMessageId).toBe(floor);
    const restarted = JSON.parse(JSON.stringify(slot)) as PersistedGroup;
    expect(restarted.syncedThroughMessageId).toBe(floor);
    expect(summarizeGroupSlot(restarted, null).unread).toBe(0);
  });

  it("a slot that never had one does not grow the key", () => {
    const merged = mergeForegroundSlot(undefined, { messages: [msg(1)], cursor: 1 });
    expect("syncedThroughMessageId" in merged).toBe(false);
  });

  it("goes with the group's transcript", () => {
    const payload = { version: 5, groups: { aa: history() } };
    expect(withoutGroup(payload, "aa")?.groups["aa"]).toBeUndefined();
  });
});
