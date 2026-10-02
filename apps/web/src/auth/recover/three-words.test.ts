// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  FRESH_BUDGET,
  THREE_WORDS_MAX_MISSES,
  THREE_WORDS_PAUSE_MS,
  afterMiss,
  budgetAt,
  clearBudget,
  formatCountdown,
  loadBudget,
  offersThreeWords,
  pauseRemainingMs,
  saveBudget,
  triesLeft,
  type ThreeWordsBudget,
} from "./three-words";

function memoryStorage(): { getItem(k: string): string | null; setItem(k: string, v: string): void; removeItem(k: string): void; dump(): Record<string, string> } {
  const rows = new Map<string, string>();
  return {
    getItem: (k) => rows.get(k) ?? null,
    setItem: (k, v) => void rows.set(k, v),
    removeItem: (k) => void rows.delete(k),
    dump: () => Object.fromEntries(rows),
  };
}

const T0 = 1_800_000_000_000;

describe("whether the three-words row is listed", () => {
  const yes = { cachedUsername: "alice", phraseOnDevice: true, keysOnDevice: true };

  it("is listed on a device that holds the phrase and the keys of a named cached account", () => {
    expect(offersThreeWords(yes)).toBe(true);
  });

  it("is absent without a cached account", () => {
    expect(offersThreeWords({ ...yes, cachedUsername: null })).toBe(false);
  });

  it("is absent for an account without a username (the sign-in is addressed by name)", () => {
    expect(offersThreeWords({ ...yes, cachedUsername: "" })).toBe(false);
    expect(offersThreeWords({ ...yes, cachedUsername: "   " })).toBe(false);
  });

  it("is absent when the device does not hold the phrase", () => {
    expect(offersThreeWords({ ...yes, phraseOnDevice: false })).toBe(false);
  });

  it("is absent when the device does not hold the keys", () => {
    expect(offersThreeWords({ ...yes, keysOnDevice: false })).toBe(false);
  });
});

describe("the three-words guess budget", () => {
  it("allows five misses and pauses on the fifth", () => {
    let b: ThreeWordsBudget = FRESH_BUDGET;
    for (let i = 1; i < THREE_WORDS_MAX_MISSES; i += 1) {
      b = afterMiss(b, T0 + i);
      expect(b.pausedUntilMs).toBeNull();
      expect(triesLeft(b, T0 + i)).toBe(THREE_WORDS_MAX_MISSES - i);
    }
    b = afterMiss(b, T0 + 10);
    expect(b.misses).toBe(5);
    expect(b.pausedUntilMs).toBe(T0 + 10 + THREE_WORDS_PAUSE_MS);
    expect(triesLeft(b, T0 + 11)).toBe(0);
  });

  it("pauses for exactly 15 minutes and reports the time left", () => {
    expect(THREE_WORDS_PAUSE_MS).toBe(15 * 60 * 1000);
    const paused: ThreeWordsBudget = { misses: 5, pausedUntilMs: T0 + THREE_WORDS_PAUSE_MS };
    expect(pauseRemainingMs(paused, T0)).toBe(THREE_WORDS_PAUSE_MS);
    expect(pauseRemainingMs(paused, T0 + 60_000)).toBe(14 * 60 * 1000);
    expect(pauseRemainingMs(paused, T0 + THREE_WORDS_PAUSE_MS - 1)).toBe(1);
    expect(pauseRemainingMs(paused, T0 + THREE_WORDS_PAUSE_MS)).toBe(0);
  });

  it("starts a fresh count once the pause has run out", () => {
    const paused: ThreeWordsBudget = { misses: 5, pausedUntilMs: T0 + THREE_WORDS_PAUSE_MS };
    expect(budgetAt(paused, T0 + THREE_WORDS_PAUSE_MS - 1)).toBe(paused);
    expect(budgetAt(paused, T0 + THREE_WORDS_PAUSE_MS)).toEqual(FRESH_BUDGET);
    expect(triesLeft(paused, T0 + THREE_WORDS_PAUSE_MS)).toBe(THREE_WORDS_MAX_MISSES);
    const next = afterMiss(paused, T0 + THREE_WORDS_PAUSE_MS + 5);
    expect(next).toEqual({ misses: 1, pausedUntilMs: null });
  });

  it("does not count a miss while paused", () => {
    const paused: ThreeWordsBudget = { misses: 5, pausedUntilMs: T0 + THREE_WORDS_PAUSE_MS };
    expect(afterMiss(paused, T0 + 1000)).toBe(paused);
  });

  it("never waits longer than a full pause when the clock was set back", () => {
    const paused: ThreeWordsBudget = { misses: 5, pausedUntilMs: T0 + THREE_WORDS_PAUSE_MS };
    expect(pauseRemainingMs(paused, T0 - 10 * 24 * 3600 * 1000)).toBe(THREE_WORDS_PAUSE_MS);
  });

  it("survives a reload: it is stored per account", () => {
    const storage = memoryStorage();
    const b = afterMiss(afterMiss(FRESH_BUDGET, T0), T0 + 1);
    saveBudget(storage, "acctA", b);
    expect(loadBudget(storage, "acctA")).toEqual(b);
    expect(loadBudget(storage, "acctB")).toEqual(FRESH_BUDGET);
    expect(Object.keys(storage.dump())).toEqual(["lakko:three-words-budget:acctA"]);
  });

  it("is cleared by a success", () => {
    const storage = memoryStorage();
    saveBudget(storage, "acctA", { misses: 4, pausedUntilMs: null });
    clearBudget(storage, "acctA");
    expect(loadBudget(storage, "acctA")).toEqual(FRESH_BUDGET);
    expect(storage.dump()).toEqual({});
  });

  it("reads a malformed row as a fresh budget", () => {
    const storage = memoryStorage();
    for (const raw of ["", "nope", "null", "[]", '{"misses":-1,"pausedUntilMs":null}', '{"misses":"2"}', '{"misses":1,"pausedUntilMs":"x"}']) {
      storage.setItem("lakko:three-words-budget:acctA", raw);
      expect(loadBudget(storage, "acctA")).toEqual(FRESH_BUDGET);
    }
  });

  it("holds without storage for the page lifetime", () => {
    expect(loadBudget(undefined, "acctA")).toEqual(FRESH_BUDGET);
    expect(() => saveBudget(undefined, "acctA", FRESH_BUDGET)).not.toThrow();
    expect(() => clearBudget(undefined, "acctA")).not.toThrow();
  });
});

describe("the countdown text", () => {
  it("is minutes and two-digit seconds, rounded up", () => {
    expect(formatCountdown(15 * 60 * 1000)).toBe("15:00");
    expect(formatCountdown(14 * 60 * 1000 + 59_000)).toBe("14:59");
    expect(formatCountdown(14 * 60 * 1000 + 58_001)).toBe("14:59");
    expect(formatCountdown(61_000)).toBe("1:01");
    expect(formatCountdown(1)).toBe("0:01");
    expect(formatCountdown(0)).toBe("0:00");
    expect(formatCountdown(-5)).toBe("0:00");
  });
});
