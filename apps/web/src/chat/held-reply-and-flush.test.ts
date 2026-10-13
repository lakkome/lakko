// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.7 case 1 - a held reply flushed into the successor keeps its
 * quote and drops its jump target; and a view shows what a background
 * flush sent while it was mounting.
 */
import { describe, expect, it, vi } from "vitest";

vi.mock("../auth/key-store", () => ({ getDeviceKey: async () => null }));

import { REPLY_NO_JUMP_TARGET, flushedRowsToShow, replyForSend } from "./held-send";
import {
  heldFromPersisted,
  heldToPersisted,
  notifyHeldFlushed,
  subscribeHeldFlushed,
  type HeldMessage,
} from "./held-send-queue";
import type { PersistedMessage } from "./message-log-store";

describe("replyForSend", () => {
  const reply = { target: 7, author: "Anna", excerpt: "see you" };
  it("no reply stays no reply", () => {
    expect(replyForSend(undefined, "old", "new")).toBeUndefined();
  });
  it("a reply sent into the group it was composed in is unchanged", () => {
    expect(replyForSend(reply, "old", "old")).toBe(reply);
  });
  it("a reply flushed into the successor keeps its quote and drops its jump target", () => {
    expect(replyForSend(reply, "old", "new")).toEqual({
      target: REPLY_NO_JUMP_TARGET,
      author: "Anna",
      excerpt: "see you",
    });
  });
  it("a reply whose compose group was not recorded is never sent with a target", () => {
    expect(replyForSend(reply, undefined, "new")?.target).toBe(REPLY_NO_JUMP_TARGET);
  });
  it("the no-target value names no message", () => {
    // DS ids start at 1 and history rows are negative.
    expect(REPLY_NO_JUMP_TARGET).toBe(0);
  });
});

describe("the compose group of a held reply survives a reload", () => {
  it("round-trips through the stored form", () => {
    const payload: HeldMessage = {
      text: "hi",
      attached: [],
      reply: { target: 7, author: "Anna", excerpt: "see you" },
      replyGroupIdB64: "b2xk",
    };
    const read = heldFromPersisted(
      JSON.parse(JSON.stringify(heldToPersisted("ab", { heldAtMs: 1, payload }))),
    );
    expect(read?.held.payload.replyGroupIdB64).toBe("b2xk");
    expect(read?.held.payload.reply?.target).toBe(7);
  });
  it("a message with no reply stores no group", () => {
    const payload: HeldMessage = { text: "hi", attached: [], reply: undefined, replyGroupIdB64: "b2xk" };
    expect(heldToPersisted("ab", { heldAtMs: 1, payload })).not.toHaveProperty("replyGroupIdB64");
  });
});

describe("flushedRowsToShow", () => {
  const m = (id: string): { messageId: string } => ({ messageId: id });
  it("shows what the view does not hold, in send order", () => {
    expect(flushedRowsToShow(new Set([5]), [m("6"), m("7")])).toEqual([m("6"), m("7")]);
  });
  it("skips what the log read already brought in", () => {
    expect(flushedRowsToShow(new Set([6]), [m("6"), m("7")])).toEqual([m("7")]);
  });
  it("shows a repeated report once and drops an unreadable id", () => {
    expect(flushedRowsToShow(new Set(), [m("6"), m("6"), m("x")])).toEqual([m("6")]);
  });
});

describe("the flush report", () => {
  const sent: PersistedMessage = {
    messageId: "6",
    direction: "outgoing",
    text: "held",
    attachments: [],
    createdAt: "2026-10-11T00:00:00.000Z",
  };
  it("reaches a subscribed view, and not after it unsubscribed", () => {
    const seen: string[] = [];
    const off = subscribeHeldFlushed((r) => seen.push(r.groupIdB64));
    notifyHeldFlushed({ groupIdB64: "G", messages: [sent] });
    off();
    notifyHeldFlushed({ groupIdB64: "H", messages: [sent] });
    expect(seen).toEqual(["G"]);
  });
  it("an empty flush reports nothing, and one view's failure does not stop the next", () => {
    const seen: string[] = [];
    const offA = subscribeHeldFlushed(() => {
      throw new Error("view");
    });
    const offB = subscribeHeldFlushed((r) => seen.push(r.groupIdB64));
    notifyHeldFlushed({ groupIdB64: "G", messages: [] });
    notifyHeldFlushed({ groupIdB64: "G", messages: [sent] });
    offA();
    offB();
    expect(seen).toEqual(["G"]);
  });
});
