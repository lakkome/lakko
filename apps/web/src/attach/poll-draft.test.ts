// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  canAddRow,
  canRemoveRow,
  CHECKLIST_ITEMS_MAX,
  CHECKLIST_ITEMS_MIN,
  enterAction,
  isChecklistDraftValid,
  isPollDraftValid,
  keyboardMoveTarget,
  movedTo,
  normaliseOptions,
  POLL_OPTIONS_MAX,
  POLL_OPTIONS_MIN,
  withAddedRow,
  withoutRow,
} from "./poll-draft";

const base = { question: "Lunch?", options: ["Pizza", "Soup"], multiple: false, anonymous: true };

describe("poll draft", () => {
  it("accepts a question and two options", () => {
    expect(isPollDraftValid(base)).toBe(true);
  });
  it("refuses an empty question, one option, eleven options, an over-long option", () => {
    expect(isPollDraftValid({ ...base, question: "  " })).toBe(false);
    expect(isPollDraftValid({ ...base, options: ["a"] })).toBe(false);
    expect(isPollDraftValid({ ...base, options: Array.from({ length: 11 }, (_, i) => `o${i}`) })).toBe(false);
    expect(isPollDraftValid({ ...base, options: ["a", "x".repeat(101)] })).toBe(false);
    expect(isPollDraftValid({ ...base, question: "q".repeat(256) })).toBe(false);
  });
  it("needs a correct answer in range and excludes multiple in a quiz", () => {
    expect(isPollDraftValid({ ...base, quiz: { correct: 1, explanation: "" } })).toBe(true);
    expect(isPollDraftValid({ ...base, quiz: { correct: 2, explanation: "" } })).toBe(false);
    expect(isPollDraftValid({ ...base, multiple: true, quiz: { correct: 0, explanation: "" } })).toBe(false);
    expect(isPollDraftValid({ ...base, quiz: { correct: 0, explanation: "e".repeat(201) } })).toBe(false);
  });
  it("counts characters, not bytes", () => {
    expect(isPollDraftValid({ ...base, options: ["a", "é".repeat(100)] })).toBe(true);
  });
  it("normalises options", () => {
    expect(normaliseOptions([" a ", "", "b", "  "])).toEqual(["a", "b"]);
  });
  it("shows Add only below the maximum and the remove icon only above the minimum (UX 7.1a)", () => {
    expect(canAddRow(9, POLL_OPTIONS_MAX)).toBe(true);
    expect(canAddRow(10, POLL_OPTIONS_MAX)).toBe(false);
    expect(canAddRow(CHECKLIST_ITEMS_MAX, CHECKLIST_ITEMS_MAX)).toBe(false);
    expect(canRemoveRow(2, POLL_OPTIONS_MIN)).toBe(false);
    expect(canRemoveRow(3, POLL_OPTIONS_MIN)).toBe(true);
    expect(canRemoveRow(1, CHECKLIST_ITEMS_MIN)).toBe(false);
    expect(canRemoveRow(2, CHECKLIST_ITEMS_MIN)).toBe(true);
  });
  it("adds a row at the end, never past the maximum, and never adds a trailing row by itself", () => {
    expect(withAddedRow(["a", "b"], "", POLL_OPTIONS_MAX)).toEqual(["a", "b", ""]);
    const full = Array.from({ length: 10 }, () => "x");
    expect(withAddedRow(full, "", POLL_OPTIONS_MAX)).toHaveLength(10);
  });
  it("removes a row, never below the minimum", () => {
    expect(withoutRow(["a", "b", "c"], 1, POLL_OPTIONS_MIN)).toEqual(["a", "c"]);
    expect(withoutRow(["a", "b"], 0, POLL_OPTIONS_MIN)).toEqual(["a", "b"]);
    expect(withoutRow(["a"], 0, CHECKLIST_ITEMS_MIN)).toEqual(["a"]);
    expect(withoutRow(["a", "b", "c"], 5, POLL_OPTIONS_MIN)).toEqual(["a", "b", "c"]);
  });
  it("Enter moves to the next row, adds one on the last, and does nothing on the last at the maximum", () => {
    expect(enterAction(0, 3, POLL_OPTIONS_MAX)).toEqual({ kind: "focus", to: 1 });
    expect(enterAction(2, 3, POLL_OPTIONS_MAX)).toEqual({ kind: "add", to: 3 });
    expect(enterAction(9, 10, POLL_OPTIONS_MAX)).toBeNull();
    expect(enterAction(4, 3, POLL_OPTIONS_MAX)).toBeNull();
  });
  it("Alt+Up and Alt+Down move a row one place, not past either end", () => {
    expect(keyboardMoveTarget(1, "ArrowUp", true, 3)).toBe(0);
    expect(keyboardMoveTarget(1, "ArrowDown", true, 3)).toBe(2);
    expect(keyboardMoveTarget(0, "ArrowUp", true, 3)).toBeNull();
    expect(keyboardMoveTarget(2, "ArrowDown", true, 3)).toBeNull();
    expect(keyboardMoveTarget(1, "ArrowUp", false, 3)).toBeNull();
    expect(keyboardMoveTarget(1, "Enter", true, 3)).toBeNull();
  });
  it("moves entries", () => {
    expect(movedTo(["a", "b", "c"], 0, 1)).toEqual(["b", "a", "c"]);
    expect(movedTo(["a", "b", "c"], 2, 0)).toEqual(["c", "a", "b"]);
    expect(movedTo(["a"], 0, 1)).toEqual(["a"]);
  });
  it("validates a checklist", () => {
    expect(isChecklistDraftValid({ title: "T", othersAdd: false, othersMark: true, items: [{ text: "x" }] })).toBe(true);
    expect(isChecklistDraftValid({ title: "T", othersAdd: false, othersMark: true, items: [] })).toBe(false);
    expect(isChecklistDraftValid({ title: "", othersAdd: false, othersMark: true, items: [{ text: "x" }] })).toBe(false);
    expect(
      isChecklistDraftValid({
        title: "T",
        othersAdd: false,
        othersMark: true,
        items: Array.from({ length: 31 }, () => ({ text: "x" })),
      }),
    ).toBe(false);
  });
});
