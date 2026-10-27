// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { initialPickerIndex, pickerKey, suggestWords } from "./picker";

describe("initialPickerIndex", () => {
  it("starts on the current value", () => {
    expect(initialPickerIndex(2, 4)).toBe(2);
  });
  it("starts on the first row when the value is not in the list", () => {
    expect(initialPickerIndex(-1, 4)).toBe(0);
    expect(initialPickerIndex(9, 4)).toBe(0);
  });
  it("has no active row in an empty list", () => {
    expect(initialPickerIndex(0, 0)).toBe(-1);
  });
});

describe("pickerKey while closed", () => {
  it.each(["Enter", " ", "ArrowDown", "ArrowUp"])("%j opens on the current value", (key) => {
    expect(pickerKey(key, false, 2, 4)).toEqual({ action: "open", index: 2, prevent: true });
  });
  it("ignores other keys and lets them through", () => {
    expect(pickerKey("a", false, 1, 4)).toEqual({ action: "none", index: 1, prevent: false });
    expect(pickerKey("Escape", false, 1, 4).action).toBe("none");
    expect(pickerKey("Tab", false, 1, 4).prevent).toBe(false);
  });
});

describe("pickerKey while open", () => {
  it("moves down and stops at the last row", () => {
    expect(pickerKey("ArrowDown", true, 1, 3)).toEqual({ action: "move", index: 2, prevent: true });
    expect(pickerKey("ArrowDown", true, 2, 3).index).toBe(2);
  });
  it("moves up and stops at the first row", () => {
    expect(pickerKey("ArrowUp", true, 1, 3).index).toBe(0);
    expect(pickerKey("ArrowUp", true, 0, 3).index).toBe(0);
  });
  it("jumps with Home and End", () => {
    expect(pickerKey("Home", true, 2, 5).index).toBe(0);
    expect(pickerKey("End", true, 0, 5).index).toBe(4);
  });
  it("picks the active row on Enter and Space", () => {
    expect(pickerKey("Enter", true, 1, 3)).toEqual({ action: "pick", index: 1, prevent: true });
    expect(pickerKey(" ", true, 0, 3).action).toBe("pick");
  });
  it("picks nothing when no row is active, but still holds the key", () => {
    expect(pickerKey("Enter", true, -1, 0)).toEqual({ action: "none", index: -1, prevent: true });
  });
  it("closes on Escape (prevented) and on Tab (not prevented)", () => {
    expect(pickerKey("Escape", true, 1, 3)).toEqual({ action: "close", index: 1, prevent: true });
    expect(pickerKey("Tab", true, 1, 3)).toEqual({ action: "close", index: 1, prevent: false });
  });
  it("keeps an empty list at -1", () => {
    expect(pickerKey("ArrowDown", true, -1, 0).index).toBe(-1);
    expect(pickerKey("End", true, -1, 0).index).toBe(-1);
  });
});

describe("suggestWords", () => {
  const list = ["abandon", "ability", "able", "about", "above", "absent", "zoo"];
  it("offers words with the typed prefix, in list order, up to the limit", () => {
    expect(suggestWords("ab", list, 3)).toEqual(["abandon", "ability", "able"]);
  });
  it("is case and whitespace insensitive", () => {
    expect(suggestWords(" ZO ", list)).toEqual(["zoo"]);
  });
  it("offers nothing for an empty field or no match", () => {
    expect(suggestWords("", list)).toEqual([]);
    expect(suggestWords("qq", list)).toEqual([]);
  });
  it("goes away once the field holds the only matching word", () => {
    expect(suggestWords("zoo", list)).toEqual([]);
  });
  it("keeps offering longer words when the typed word is a prefix of others", () => {
    expect(suggestWords("abl", list)).toEqual(["able"]);
    expect(suggestWords("able", list)).toEqual([]);
  });
});
