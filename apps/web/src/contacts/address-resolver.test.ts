// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074 section 3.5a - resolve before you address (rule 1), the keys of
 * one person (rule 2) and the read window of a predecessor mailbox (rule 4).
 */
import { afterEach, describe, expect, it } from "vitest";

import { base64Encode } from "../auth/account-id";
import {
  PREDECESSOR_MAILBOX_READ_MS,
  keysOfPerson,
  personKeysHex,
  predecessorMailboxReadable,
  resolveAccountKey,
  resolveAccountKeyHex,
  resolveBeforeAddress,
  setOwnAccountKeys,
} from "./address-resolver";
import { afterProvenRotation, afterReset, type AccountKeys } from "./predecessor";

const alice: AccountKeys = { currentKeyB64: "A1", provenPredecessorsB64: [] };
const bob: AccountKeys = { currentKeyB64: "B1", provenPredecessorsB64: [] };

describe("resolve before you address (rule 1)", () => {
  it("a proven predecessor is addressed at the current key", () => {
    const rotated = afterProvenRotation(alice, "A2");
    expect(resolveBeforeAddress("A1", [rotated, bob], null)).toBe("A2");
    expect(resolveBeforeAddress("A2", [rotated, bob], null)).toBe("A2");
    expect(resolveBeforeAddress("B1", [rotated, bob], null)).toBe("B1");
  });

  it("an UNPROVEN change is never resolved", () => {
    // A reset: the pin moves, and no key of before it is a predecessor.
    const reset = afterReset(alice, "A2");
    expect(resolveBeforeAddress("A1", [reset], null)).toBe("A1");
    // A reset after a proven rotation drops the proven chain too.
    const later = afterReset(afterProvenRotation(alice, "A2"), "A3");
    expect(resolveBeforeAddress("A1", [later], null)).toBe("A1");
    expect(resolveBeforeAddress("A2", [later], null)).toBe("A2");
    // A key nobody is pinned under names itself.
    expect(resolveBeforeAddress("ZZ", [alice, bob], null)).toBe("ZZ");
  });

  it("a chain resolves every link to the newest key", () => {
    const thrice = afterProvenRotation(afterProvenRotation(afterProvenRotation(alice, "A2"), "A3"), "A4");
    for (const k of ["A1", "A2", "A3", "A4"]) expect(resolveBeforeAddress(k, [thrice], null)).toBe("A4");
  });

  it("the own account is resolved like any other", () => {
    const own: AccountKeys = { currentKeyB64: "O2", provenPredecessorsB64: ["O1"] };
    // Another device of this account is bound to the predecessor key.
    expect(resolveBeforeAddress("O1", [bob], own)).toBe("O2");
    expect(resolveBeforeAddress("O2", [bob], own)).toBe("O2");
    expect(resolveBeforeAddress("O1", [bob], null)).toBe("O1");
  });

  it("a predecessor two accounts claim names nobody", () => {
    const a: AccountKeys = { currentKeyB64: "A2", provenPredecessorsB64: ["X"] };
    const b: AccountKeys = { currentKeyB64: "B2", provenPredecessorsB64: ["X"] };
    expect(resolveBeforeAddress("X", [a, b], null)).toBe("X");
    // And a current key is never remapped by another account's claim.
    const c: AccountKeys = { currentKeyB64: "C2", provenPredecessorsB64: ["A2"] };
    expect(resolveBeforeAddress("A2", [a, c], null)).toBe("A2");
  });
});

describe("the keys of one person (rule 2)", () => {
  it("current first, then the proven predecessors newest first", () => {
    const twice = afterProvenRotation(afterProvenRotation(alice, "A2"), "A3");
    expect(keysOfPerson("A1", [twice, bob], null)).toEqual(["A3", "A2", "A1"]);
    expect(keysOfPerson("A3", [twice, bob], null)).toEqual(["A3", "A2", "A1"]);
    expect(keysOfPerson("B1", [twice, bob], null)).toEqual(["B1"]);
    expect(keysOfPerson("ZZ", [twice, bob], null)).toEqual(["ZZ"]);
  });

  it("a reset leaves no other key", () => {
    const reset = afterReset(afterProvenRotation(alice, "A2"), "A3");
    expect(keysOfPerson("A3", [reset], null)).toEqual(["A3"]);
    expect(keysOfPerson("A1", [reset], null)).toEqual(["A1"]);
  });
});

describe("the runtime over bytes and hex", () => {
  afterEach(() => setOwnAccountKeys(null));

  it("maps the own predecessor to the own current key, and nothing else", () => {
    const current = new Uint8Array(32).fill(2);
    const old = new Uint8Array(32).fill(1);
    const stranger = new Uint8Array(32).fill(9);
    const hex = (b: Uint8Array): string => [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
    expect(resolveAccountKey(old)).toEqual(old);
    setOwnAccountKeys({
      currentKeyB64: base64Encode(current),
      provenPredecessorsB64: [base64Encode(old)],
    });
    expect(resolveAccountKey(old)).toEqual(current);
    expect(resolveAccountKey(current)).toEqual(current);
    expect(resolveAccountKey(stranger)).toEqual(stranger);
    expect(resolveAccountKeyHex(hex(old))).toBe(hex(current));
    expect(personKeysHex(hex(old))).toEqual([hex(current), hex(old)]);
    expect(personKeysHex(hex(stranger))).toEqual([hex(stranger)]);
    setOwnAccountKeys(null);
    expect(resolveAccountKey(old)).toEqual(old);
  });
});

describe("the predecessor mailbox is read for 30 days, then left (rule 4)", () => {
  const adopted = 1_700_000_000_000;
  it("read from the adoption until the 30 days are over", () => {
    expect(predecessorMailboxReadable(adopted, adopted)).toBe(true);
    expect(predecessorMailboxReadable(adopted, adopted + PREDECESSOR_MAILBOX_READ_MS - 1)).toBe(true);
    expect(predecessorMailboxReadable(adopted, adopted + PREDECESSOR_MAILBOX_READ_MS)).toBe(false);
    expect(predecessorMailboxReadable(adopted, adopted + 2 * PREDECESSOR_MAILBOX_READ_MS)).toBe(false);
  });
  it("the window is 30 days", () => {
    expect(PREDECESSOR_MAILBOX_READ_MS).toBe(30 * 24 * 60 * 60 * 1000);
  });
  it("no stored time reads (the caller stamps one); a clock gone backwards reads", () => {
    expect(predecessorMailboxReadable(null, adopted)).toBe(true);
    expect(predecessorMailboxReadable(adopted, adopted - 5_000)).toBe(true);
    expect(predecessorMailboxReadable(Number.NaN, adopted)).toBe(false);
  });
});
