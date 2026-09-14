// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 — the viewer's own profile record: per account, and a stamp that
 * never moves backwards.
 */
import { describe, expect, it } from "vitest";

import { EMPTY_OWN_PROFILE, clearOwnProfile, loadOwnProfile, saveOwnProfile } from "./own-profile";

function memoryStorage() {
  const map = new Map<string, string>();
  return {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => {
      map.set(k, v);
    },
    removeItem: (k: string) => {
      map.delete(k);
    },
  };
}

describe("own profile", () => {
  it("is empty until saved", () => {
    expect(loadOwnProfile("acct", memoryStorage())).toEqual(EMPTY_OWN_PROFILE);
  });

  it("round-trips per account", () => {
    const storage = memoryStorage();
    saveOwnProfile("one", "Alice", 1000, storage);
    expect(loadOwnProfile("one", storage)).toEqual({ displayName: "Alice", updatedAtMs: 1000 });
    expect(loadOwnProfile("two", storage)).toEqual(EMPTY_OWN_PROFILE);
  });

  it("a save on a clock that went back still stamps strictly newer", () => {
    const storage = memoryStorage();
    saveOwnProfile("acct", "Alice", 5000, storage);
    const next = saveOwnProfile("acct", "Alice P.", 4000, storage);
    expect(next.updatedAtMs).toBe(5001);
  });

  it("clearing the name keeps a stamp, so receivers replace the old name", () => {
    const storage = memoryStorage();
    saveOwnProfile("acct", "Alice", 5000, storage);
    expect(saveOwnProfile("acct", null, 6000, storage)).toEqual({ displayName: null, updatedAtMs: 6000 });
  });

  it("malformed storage reads as empty; clear removes", () => {
    const storage = memoryStorage();
    storage.setItem("lakko.profile.own.v1.acct", "{nope");
    expect(loadOwnProfile("acct", storage)).toEqual(EMPTY_OWN_PROFILE);
    saveOwnProfile("acct", "Alice", 1, storage);
    clearOwnProfile("acct", storage);
    expect(loadOwnProfile("acct", storage)).toEqual(EMPTY_OWN_PROFILE);
  });
});
