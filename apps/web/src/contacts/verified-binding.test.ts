// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  clearVerifiedBindingsForAccount,
  dropVerifiedBinding,
  memberHeldKey,
  readVerifiedBinding,
  recordVerifiedBinding,
  VERIFIED_BINDINGS_PREFIX,
  type VerifiedBinding,
} from "./verified-binding";

const ROSTER = "cm9zdGVy";
const OTHER = "b3RoZXI=";
const NEW = "bmV3";
const binding = (keyB64: string, keyGeneration = 1): VerifiedBinding => ({
  keyB64,
  keyGeneration,
  verifiedAtMs: 1,
});

describe("memberHeldKey: what a roster key is worth under an account id", () => {
  it("a pinned account is decided by its pin, whatever else is held", () => {
    expect(
      memberHeldKey({ pinned: true, binding: binding(ROSTER), rosterKeyB64: ROSTER, fetchedKeyB64: NEW }),
    ).toEqual({ kind: "pin" });
  });

  it("no binding, the directory serves the roster key: record, never a reset", () => {
    expect(
      memberHeldKey({ pinned: false, binding: undefined, rosterKeyB64: ROSTER, fetchedKeyB64: ROSTER }),
    ).toEqual({ kind: "record" });
  });

  it("THE ATTACK: no binding and the directory serves another key is no verdict", () => {
    // A member (key ROSTER) claims the id of an account whose key is OTHER
    // and which reset at some point. The claim establishes nothing.
    expect(
      memberHeldKey({ pinned: false, binding: undefined, rosterKeyB64: ROSTER, fetchedKeyB64: OTHER }),
    ).toEqual({ kind: "none" });
  });

  it("a binding naming the roster key, still served: confirmed", () => {
    expect(
      memberHeldKey({ pinned: false, binding: binding(ROSTER), rosterKeyB64: ROSTER, fetchedKeyB64: ROSTER }),
    ).toEqual({ kind: "confirmed" });
  });

  it("a binding naming the roster key, another key served: the change is judged from the binding", () => {
    expect(
      memberHeldKey({ pinned: false, binding: binding(ROSTER, 3), rosterKeyB64: ROSTER, fetchedKeyB64: NEW }),
    ).toEqual({ kind: "held", heldKeyB64: ROSTER, acceptedGeneration: 3 });
  });

  it("THE ATTACK, later: a binding naming ANOTHER key never lends itself to the roster key", () => {
    for (const fetchedKeyB64 of [ROSTER, OTHER, NEW]) {
      expect(
        memberHeldKey({ pinned: false, binding: binding(OTHER), rosterKeyB64: ROSTER, fetchedKeyB64 }),
      ).toEqual({ kind: "none" });
    }
  });
});

class MemoryStorage {
  private map = new Map<string, string>();
  getItem(k: string): string | null {
    return this.map.get(k) ?? null;
  }
  setItem(k: string, v: string): void {
    this.map.set(k, String(v));
  }
  removeItem(k: string): void {
    this.map.delete(k);
  }
}

describe("the verified-binding store", () => {
  const OWNER = "b3duZXI=";
  const ACCOUNT = "YWNjb3VudA==";
  beforeEach(() => {
    (globalThis as unknown as { localStorage: Storage }).localStorage =
      new MemoryStorage() as unknown as Storage;
  });
  afterEach(() => {
    delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
  });

  it("is write-once per account id", () => {
    expect(recordVerifiedBinding(ACCOUNT, binding(ROSTER), OWNER)).toBe(true);
    expect(recordVerifiedBinding(ACCOUNT, binding(OTHER), OWNER)).toBe(false);
    expect(readVerifiedBinding(ACCOUNT, OWNER)?.keyB64).toBe(ROSTER);
  });

  it("is per signed-in account, and writes nothing with none", () => {
    recordVerifiedBinding(ACCOUNT, binding(ROSTER), OWNER);
    expect(readVerifiedBinding(ACCOUNT, "c29tZW9uZQ==")).toBeUndefined();
    expect(recordVerifiedBinding(ACCOUNT, binding(ROSTER), "")).toBe(false);
  });

  it("drops one binding, and the whole row with the account", () => {
    recordVerifiedBinding(ACCOUNT, binding(ROSTER), OWNER);
    recordVerifiedBinding("eA==", binding(OTHER), OWNER);
    dropVerifiedBinding(ACCOUNT, OWNER);
    expect(readVerifiedBinding(ACCOUNT, OWNER)).toBeUndefined();
    expect(readVerifiedBinding("eA==", OWNER)?.keyB64).toBe(OTHER);
    clearVerifiedBindingsForAccount(OWNER);
    expect(localStorage.getItem(VERIFIED_BINDINGS_PREFIX + OWNER)).toBeNull();
  });

  it("a junk row or entry reads as no binding", () => {
    localStorage.setItem(VERIFIED_BINDINGS_PREFIX + OWNER, "not json");
    expect(readVerifiedBinding(ACCOUNT, OWNER)).toBeUndefined();
    localStorage.setItem(
      VERIFIED_BINDINGS_PREFIX + OWNER,
      JSON.stringify({ [ACCOUNT]: { keyB64: 5, keyGeneration: "x" } }),
    );
    expect(readVerifiedBinding(ACCOUNT, OWNER)).toBeUndefined();
  });
});
