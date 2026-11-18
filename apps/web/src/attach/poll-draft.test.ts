// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  isChecklistDraftValid,
  isPollDraftValid,
  moved,
  normaliseOptions,
  withoutEmptyAt,
  withTrailingEmpty,
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
  it("keeps one empty field trailing until the cap", () => {
    expect(withTrailingEmpty(["a", "b"])).toEqual(["a", "b", ""]);
    expect(withTrailingEmpty(["a", ""])).toEqual(["a", ""]);
    expect(withTrailingEmpty(Array.from({ length: 10 }, () => "x"))).toHaveLength(10);
  });
  it("removes an emptied field on leave, not the trailing one, keeping two", () => {
    expect(withoutEmptyAt(["a", "", "c", ""], 1)).toEqual(["a", "c", ""]);
    expect(withoutEmptyAt(["a", "", ""], 2)).toEqual(["a", "", ""]);
    expect(withoutEmptyAt(["", "b"], 0)).toEqual(["", "b"]);
  });
  it("moves entries", () => {
    expect(moved(["a", "b", "c"], 0, 1)).toEqual(["b", "a", "c"]);
    expect(moved(["a"], 0, 1)).toEqual(["a"]);
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
