// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** The knock read floor: at most one read of a group's knocks per 10 s; and
 *  the knock cursor, which is the signed-in account's. */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  KNOCK_READ_MIN_INTERVAL_MS,
  clearKnockCursorsForAccount,
  readKnockCursor,
  resetKnockReadSlotsForTests,
  takeKnockReadSlot,
  writeKnockCursor,
} from "./knock-cursor";
import { bindContactsAccount } from "../contacts/local-store";

const CARA = "Y2FyYS1hY2NvdW50LTAwMDA=";
const EVE = "ZXZlLWFjY291bnQtMDAwMDA=";

let data: Map<string, string>;

beforeEach(() => {
  resetKnockReadSlotsForTests();
  data = new Map();
  (globalThis as unknown as { localStorage: Storage }).localStorage = {
    get length() {
      return data.size;
    },
    clear: () => data.clear(),
    getItem: (k: string) => data.get(k) ?? null,
    key: (i: number) => [...data.keys()][i] ?? null,
    removeItem: (k: string) => void data.delete(k),
    setItem: (k: string, v: string) => void data.set(k, String(v)),
  };
});

afterEach(() => bindContactsAccount(null));

describe("the knock cursor belongs to the signed-in account", () => {
  it("two accounts on one browser never see each other's cursor", () => {
    bindContactsAccount(CARA);
    writeKnockCursor("g", 7);
    expect(readKnockCursor("g")).toBe(7);

    bindContactsAccount(EVE);
    expect(readKnockCursor("g")).toBe(0);
    writeKnockCursor("g", 3);
    expect(readKnockCursor("g")).toBe(3);

    bindContactsAccount(CARA);
    expect(readKnockCursor("g")).toBe(7);
  });

  it("an explicit owner overrides the binding", () => {
    bindContactsAccount(CARA);
    writeKnockCursor("g", 9, EVE);
    expect(readKnockCursor("g")).toBe(0);
    expect(readKnockCursor("g", EVE)).toBe(9);
  });

  it("with no account a read is 0 and a write is a no-op", () => {
    writeKnockCursor("g", 5);
    expect(readKnockCursor("g")).toBe(0);
    expect(data.size).toBe(0);
  });

  it("never moves backward", () => {
    bindContactsAccount(CARA);
    writeKnockCursor("g", 7);
    writeKnockCursor("g", 4);
    expect(readKnockCursor("g")).toBe(7);
  });

  it("the retired device-wide row is not read", () => {
    data.set("lakko.ds.knock-cursor.v1:g", "12");
    bindContactsAccount(CARA);
    expect(readKnockCursor("g")).toBe(0);
  });

  it("Forget device drops the account's cursors and the retired rows, never another account's", () => {
    data.set("lakko.ds.knock-cursor.v1:g", "12");
    writeKnockCursor("g", 7, CARA);
    writeKnockCursor("h", 8, CARA);
    writeKnockCursor("g", 3, EVE);

    clearKnockCursorsForAccount(CARA);

    expect(readKnockCursor("g", CARA)).toBe(0);
    expect(readKnockCursor("h", CARA)).toBe(0);
    expect(readKnockCursor("g", EVE)).toBe(3);
    expect(data.has("lakko.ds.knock-cursor.v1:g")).toBe(false);
  });
});

describe("takeKnockReadSlot", () => {
  it("allows one read per group per minute, however often the poll returns", () => {
    expect(KNOCK_READ_MIN_INTERVAL_MS).toBe(60_000);
    const t0 = 1_000_000;
    expect(takeKnockReadSlot("g", t0)).toBe(true);
    // A burst of short journal fetches (two a second, the rate iOS was
    // measured at) gets no further read inside the window.
    for (let t = t0 + 500; t < t0 + 60_000; t += 500) {
      expect(takeKnockReadSlot("g", t)).toBe(false);
    }
    expect(takeKnockReadSlot("g", t0 + 60_000)).toBe(true);
    expect(takeKnockReadSlot("g", t0 + 60_001)).toBe(false);
  });

  it("each group has its own window", () => {
    expect(takeKnockReadSlot("a", 5)).toBe(true);
    expect(takeKnockReadSlot("b", 6)).toBe(true);
    expect(takeKnockReadSlot("a", 7)).toBe(false);
  });

  it("a clock that went backwards reads as due, so it can never stop knock reads", () => {
    expect(takeKnockReadSlot("g", 50_000)).toBe(true);
    expect(takeKnockReadSlot("g", 40_000)).toBe(true);
  });
});
