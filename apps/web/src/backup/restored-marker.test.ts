// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { MemoryStorage } from "../test/fake-storage";
import {
  answerForRejoinOutcome,
  clearRestoreMarker,
  isRestoreMarked,
  markRestoreFinished,
  recordRestoreAnswer,
  restoreAnswer,
  restoredConversationCount,
  restoredListState,
} from "./restored-marker";

describe("the restored marker", () => {
  it("is set only when the backup named a conversation, and keeps the count", () => {
    const s = new MemoryStorage();
    markRestoreFinished("a", 0, s);
    expect(isRestoreMarked("a", s)).toBe(false);
    expect(restoredConversationCount("a", s)).toBe(0);
    markRestoreFinished("a", 2, s);
    expect(isRestoreMarked("a", s)).toBe(true);
    expect(restoredConversationCount("a", s)).toBe(2);
  });

  it("is per account and goes for good", () => {
    const s = new MemoryStorage();
    markRestoreFinished("a", 1, s);
    expect(isRestoreMarked("b", s)).toBe(false);
    clearRestoreMarker("a", s);
    expect(isRestoreMarked("a", s)).toBe(false);
    expect(restoredConversationCount("a", s)).toBe(0);
  });
});

describe("the rejoin answer", () => {
  it("is unknown until recorded, and the latest recording wins", () => {
    const s = new MemoryStorage();
    expect(restoreAnswer("a", s)).toBeNull();
    recordRestoreAnswer("a", "answered", s);
    expect(restoreAnswer("a", s)).toBe("answered");
    recordRestoreAnswer("a", "founded", s);
    expect(restoreAnswer("a", s)).toBe("founded");
  });

  it("is kept per account, independent of when the marker is written", () => {
    const s = new MemoryStorage();
    // The rejoin can decide before the backup restore finishes.
    recordRestoreAnswer("a", "answered", s);
    markRestoreFinished("a", 3, s);
    expect(restoreAnswer("a", s)).toBe("answered");
    expect(restoreAnswer("b", s)).toBeNull();
  });

  it("survives a restart (it is read back from storage)", () => {
    const s = new MemoryStorage();
    recordRestoreAnswer("a", "founded", s);
    // A fresh read over the same storage is what a restart does.
    expect(restoreAnswer("a", s)).toBe("founded");
  });

  it("goes with the marker", () => {
    const s = new MemoryStorage();
    markRestoreFinished("a", 1, s);
    recordRestoreAnswer("a", "answered", s);
    clearRestoreMarker("a", s);
    expect(restoreAnswer("a", s)).toBeNull();
  });

  it("goes on a wipe even without a marker", () => {
    const s = new MemoryStorage();
    recordRestoreAnswer("a", "founded", s);
    clearRestoreMarker("a", s);
    expect(restoreAnswer("a", s)).toBeNull();
  });

  it("reads anything it did not write as unknown", () => {
    const s = new MemoryStorage();
    s.setItem("lakko.backup.restore-answer.v1.a", "maybe");
    expect(restoreAnswer("a", s)).toBeNull();
  });

  it("is answered for a join or a device that answered and founded only on founding", () => {
    expect(answerForRejoinOutcome("joined")).toBe("answered");
    expect(answerForRejoinOutcome("waiting")).toBe("answered");
    expect(answerForRejoinOutcome("founded")).toBe("founded");
    // No knock was made: nothing was answered and nothing founded.
    expect(answerForRejoinOutcome("no-handle")).toBeNull();
  });
});

describe("restoredListState (identity-recovery 7.2, amended 2026-11-11)", () => {
  const base = {
    marked: true,
    knownConversations: 2,
    conversationCount: 0,
    answer: "founded" as "answered" | "founded" | null,
  };

  it("is the card when this device founded", () => {
    expect(restoredListState(base)).toBe("card");
  });

  it("is the ordinary list when another device answered", () => {
    expect(restoredListState({ ...base, answer: "answered" })).toBe("none");
  });

  it("claims nothing until the answer is known", () => {
    expect(restoredListState({ ...base, answer: null })).toBe("none");
  });

  it("is nothing when the backup named no conversation, whatever the answer", () => {
    expect(restoredListState({ ...base, knownConversations: 0 })).toBe("none");
    expect(restoredListState({ ...base, knownConversations: 0, answer: "answered" })).toBe("none");
  });

  it("is nothing once a conversation arrived, and without a marker", () => {
    expect(restoredListState({ ...base, conversationCount: 1 })).toBe("none");
    expect(restoredListState({ ...base, conversationCount: 1, answer: "answered" })).toBe("none");
    expect(restoredListState({ ...base, marked: false })).toBe("none");
    expect(restoredListState({ ...base, marked: false, answer: "answered" })).toBe("none");
  });
});
