// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import type { PollBody } from "../../wasm/lakko_ffi_wasm";
import { canClosePoll, canRetract, checklistViewOf, firstName, pollViewOf } from "./poll-model";

const body = (over: Partial<PollBody> = {}): PollBody => ({
  id: "AAAAAAAAAAAAAAAAAAAAAA==",
  question: "Lunch?",
  options: ["Soup", "Salad"],
  multiple: false,
  ...over,
});

const base = {
  channel: false,
  verified: true,
  tally: { counts: [3, 1], voters: 4, closed: false },
  ownChoice: undefined,
  sending: false,
  failed: false,
  canAct: true,
  nowMs: 1000,
};

describe("the poll card view (UX 7.2)", () => {
  it("lets a member vote before voting and shows shares after", () => {
    const before = pollViewOf({ ...base, body: body() });
    expect(before.canVote).toBe(true);
    expect(before.voted).toBe(false);
    const after = pollViewOf({ ...base, body: body(), ownChoice: [0] });
    expect(after.voted).toBe(true);
    expect(after.options[0]?.mine).toBe(true);
    expect(after.options[0]?.percent).toBe(75);
  });

  it("is anonymous in a channel whatever its flags", () => {
    expect(pollViewOf({ ...base, body: body(), channel: true }).anonymous).toBe(true);
    expect(pollViewOf({ ...base, body: body({ anonymous: { secret: "x", ring0Hash: "y" } }) }).anonymous).toBe(true);
    expect(pollViewOf({ ...base, body: body() }).anonymous).toBe(false);
  });

  it("closes at its time and stops taking votes", () => {
    const v = pollViewOf({ ...base, body: body({ closeAtMs: 500 }) });
    expect(v.closed).toBe(true);
    expect(v.canVote).toBe(false);
    const open = pollViewOf({ ...base, body: body({ closeAtMs: 5000 }) });
    expect(open.closesAtMs).toBe(5000);
  });

  it("shows nothing and takes no vote when the poll did not verify (fail-closed)", () => {
    const v = pollViewOf({ ...base, body: body(), verified: false });
    expect(v.unverifiable).toBe(true);
    expect(v.canVote).toBe(false);
    expect(v.voters).toBe(0);
    expect(v.options.every((o) => o.count === 0)).toBe(true);
  });

  it("reveals the quiz answer only after voting or closing", () => {
    const quiz = body({ quiz: { correct: 1, explanation: "Fresh" } });
    expect(pollViewOf({ ...base, body: quiz }).options[1]?.correct).toBeUndefined();
    const voted = pollViewOf({ ...base, body: quiz, ownChoice: [0] });
    expect(voted.options[1]?.correct).toBe(true);
    expect(voted.explanation).toBe("Fresh");
  });

  it("does not take another tap while a vote is being sent", () => {
    const v = pollViewOf({ ...base, body: body(), sending: true });
    expect(v.canVote).toBe(false);
    expect(v.voted).toBe(true);
    expect(canRetract(v)).toBe(false);
  });

  it("offers View results only on a public poll after voting or closing", () => {
    expect(pollViewOf({ ...base, body: body(), ownChoice: [0] }).canViewResults).toBe(true);
    expect(pollViewOf({ ...base, body: body() }).canViewResults).toBe(false);
    expect(pollViewOf({ ...base, body: body(), channel: true, ownChoice: [0] }).canViewResults).toBe(false);
  });

  it("offers Close poll to the creator, and in a group to an owner or admin", () => {
    expect(canClosePoll({ isAuthor: true, isAdmin: false, group: false, closed: false })).toBe(true);
    expect(canClosePoll({ isAuthor: false, isAdmin: true, group: true, closed: false })).toBe(true);
    expect(canClosePoll({ isAuthor: false, isAdmin: true, group: false, closed: false })).toBe(false);
    expect(canClosePoll({ isAuthor: true, isAdmin: true, group: true, closed: true })).toBe(false);
  });
});

describe("the checklist card view (UX 8.2)", () => {
  const input = {
    title: "Trip",
    othersAdd: false,
    othersMark: true,
    items: [
      { item: { kind: "author" as const, id: 1 }, text: "tent", doneBy: "ME" },
      { item: { kind: "author" as const, id: 2 }, text: "map", doneBy: "THEM" },
      { item: { kind: "author" as const, id: 3 }, text: "food", doneBy: null },
    ],
    closed: false,
    isAuthor: false,
    canAct: true,
    oneToOne: true,
    ownKeyB64: "ME",
    firstNameOf: (k: string) => (k === "THEM" ? "Ana" : "Me"),
  };

  it("names the ticker in a one-to-one only when it was the other person", () => {
    const v = checklistViewOf(input);
    expect(v.items[0]?.doneByName).toBeUndefined();
    expect(v.items[1]?.doneByName).toBe("Ana");
    expect(v.doneCount).toBe(2);
  });

  it("follows the two flags for someone who is not the author", () => {
    const v = checklistViewOf(input);
    expect(v.items.every((i) => i.canToggle)).toBe(true);
    expect(v.canAdd).toBe(false);
    const author = checklistViewOf({ ...input, isAuthor: true, othersMark: false });
    expect(author.canAdd).toBe(true);
    expect(author.items[0]?.canToggle).toBe(true);
  });

  it("takes nothing once closed", () => {
    const v = checklistViewOf({ ...input, closed: true, isAuthor: true });
    expect(v.canAdd).toBe(false);
    expect(v.items.some((i) => i.canToggle)).toBe(false);
  });

  it("uses the first word of a name", () => {
    expect(firstName("Ana Maria Silva")).toBe("Ana");
    expect(firstName("  Bo ")).toBe("Bo");
  });
});
