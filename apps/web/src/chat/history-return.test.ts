// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0101 section 9 - who answers a history request, and how the
 *  requester pages, newest first. */
import { describe, expect, it } from "vitest";

import {
  FIRST_HISTORY_REQUEST,
  HISTORY_MAX_ASKS,
  HISTORY_MAX_PAGES,
  historyAsked,
  HISTORY_RESEND_MS,
  historyAnswerAllowed,
  historyRequestDue,
  nextHistoryStep,
  noteReturnedConversation,
  readHistoryStates,
  writeHistoryState,
} from "./history-return";

function memory(): { getItem(k: string): string | null; setItem(k: string, v: string): void; removeItem(k: string): void } {
  const m = new Map<string, string>();
  return { getItem: (k) => m.get(k) ?? null, setItem: (k, v) => void m.set(k, v), removeItem: (k) => void m.delete(k) };
}

describe("who answers", () => {
  const ok = {
    fromSelf: false,
    requesterResetAccepted: true,
    requesterWasInEarlierGroup: true,
    earlierGroupHeld: true,
    kind: "chat" as const,
    ownIsOwnerOrAdmin: false,
  };
  it("the other person of a one-to-one", () => {
    expect(historyAnswerAllowed(ok)).toBe(true);
  });
  it("an owner or admin of a group, not a plain member", () => {
    expect(historyAnswerAllowed({ ...ok, kind: "group", ownIsOwnerOrAdmin: true })).toBe(true);
    expect(historyAnswerAllowed({ ...ok, kind: "group", ownIsOwnerOrAdmin: false })).toBe(false);
  });
  it("never to itself, a person whose reset was not accepted, or someone who was not in the earlier group", () => {
    expect(historyAnswerAllowed({ ...ok, fromSelf: true })).toBe(false);
    expect(historyAnswerAllowed({ ...ok, requesterResetAccepted: false })).toBe(false);
    expect(historyAnswerAllowed({ ...ok, requesterWasInEarlierGroup: false })).toBe(false);
    expect(historyAnswerAllowed({ ...ok, earlierGroupHeld: false })).toBe(false);
  });
});

describe("paging", () => {
  it("moves `before` to the oldest id of each page and stops at an empty page", () => {
    const one = nextHistoryStep(FIRST_HISTORY_REQUEST, ["900", "500", "700"]);
    expect(one).toEqual({ before: "500", pages: 1, sentAtMs: 0, asks: 0, done: false });
    const two = nextHistoryStep(one, ["499", "300"]);
    expect(two.before).toBe("300");
    expect(nextHistoryStep(two, []).done).toBe(true);
  });
  it("stops on a page that does not go older, and at the page cap", () => {
    const s = { before: "300", pages: 3, sentAtMs: 1, done: false };
    expect(nextHistoryStep(s, ["300", "400"]).done).toBe(true);
    expect(nextHistoryStep({ ...s, pages: HISTORY_MAX_PAGES - 1 }, ["10"]).done).toBe(true);
  });
  it("posts a request once, again after the resend time, never when done", () => {
    expect(historyRequestDue(FIRST_HISTORY_REQUEST, 5)).toBe(true);
    const sent = { ...FIRST_HISTORY_REQUEST, sentAtMs: 1000 };
    expect(historyRequestDue(sent, 1000 + HISTORY_RESEND_MS - 1)).toBe(false);
    expect(historyRequestDue(sent, 1000 + HISTORY_RESEND_MS)).toBe(true);
    expect(historyRequestDue({ ...sent, done: true }, 1e15)).toBe(false);
  });
  it("stops asking after the ask cap with no answer", () => {
    let s = FIRST_HISTORY_REQUEST;
    for (let i = 0; i < HISTORY_MAX_ASKS; i += 1) {
      expect(historyRequestDue(s, (i + 1) * HISTORY_RESEND_MS * 2)).toBe(true);
      s = historyAsked(s, (i + 1) * HISTORY_RESEND_MS * 2);
    }
    expect(historyRequestDue(s, 1e15)).toBe(false);
    // An answered page asks afresh.
    expect(historyRequestDue(nextHistoryStep(s, ["7"]), 1e15)).toBe(true);
  });
});

describe("the states", () => {
  it("start once and round-trip", () => {
    const s = memory();
    noteReturnedConversation("a", "G", s);
    writeHistoryState("a", "G", { before: "5", pages: 2, sentAtMs: 3, done: false }, s);
    noteReturnedConversation("a", "G", s);
    expect(readHistoryStates("a", s)).toEqual({ G: { before: "5", pages: 2, sentAtMs: 3, asks: 0, done: false } });
  });
});
