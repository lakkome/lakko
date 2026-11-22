// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { clearHeldSince, heldSince } from "./held-since";

function memoryStorage(): Storage {
  const map = new Map<string, string>();
  return {
    get length() {
      return map.size;
    },
    clear: () => map.clear(),
    getItem: (k) => map.get(k) ?? null,
    key: (i) => [...map.keys()][i] ?? null,
    removeItem: (k) => void map.delete(k),
    setItem: (k, v) => void map.set(k, v),
  };
}

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage = memoryStorage();
});
afterEach(() => {
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
});

describe("heldSince", () => {
  it("keeps the first time a group was held and gives a later group a later time", () => {
    expect(heldSince("acct", ["aa"], 100).get("aa")).toBe(100);
    const next = heldSince("acct", ["aa", "bb"], 200);
    expect(next.get("aa")).toBe(100);
    expect(next.get("bb")).toBe(200);
  });

  it("forgets groups no longer held, per account, and on clear", () => {
    heldSince("acct", ["aa", "bb"], 100);
    expect(heldSince("acct", ["bb"], 300).get("bb")).toBe(100);
    expect(heldSince("acct", ["aa"], 400).get("aa")).toBe(400);
    expect(heldSince("other", ["aa"], 500).get("aa")).toBe(500);
    clearHeldSince("acct");
    expect(heldSince("acct", ["aa"], 600).get("aa")).toBe(600);
  });

  it("without storage it orders nothing", () => {
    delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
    expect(heldSince("acct", ["aa"], 100).get("aa")).toBeUndefined();
  });
});
