// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0065 R.2 - `add-member-key.ts`. */
import { describe, expect, it } from "vitest";

import { acceptableBoundKeys, devicesBoundTo, trustedBoundKeys } from "./add-member-key";

describe("acceptableBoundKeys", () => {
  it("with no pin only the expected key is accepted", () => {
    expect([...acceptableBoundKeys("k", undefined)]).toEqual(["k"]);
  });
  it("a pin whose chain does not hold the expected key adds nothing", () => {
    const keys = { currentKeyB64: "other", provenPredecessorsB64: ["older"] };
    expect([...acceptableBoundKeys("k", keys)]).toEqual(["k"]);
  });
  it("the expected current key also accepts its proven predecessors", () => {
    const keys = { currentKeyB64: "new", provenPredecessorsB64: ["old"] };
    expect([...acceptableBoundKeys("new", keys)].sort()).toEqual(["new", "old"]);
  });
  it("an expected proven predecessor also accepts the current key", () => {
    const keys = { currentKeyB64: "new", provenPredecessorsB64: ["old"] };
    expect([...acceptableBoundKeys("old", keys)].sort()).toEqual(["new", "old"]);
  });
  it("an empty expected key accepts nothing", () => {
    expect(acceptableBoundKeys("", { currentKeyB64: "new", provenPredecessorsB64: [] }).size).toBe(0);
  });
});

describe("acceptableBoundKeys and a reset", () => {
  const superseded = new Set(["retired"]);
  it("an expected key a reset retired accepts nothing", () => {
    const keys = { currentKeyB64: "new", provenPredecessorsB64: [] };
    expect(acceptableBoundKeys("retired", keys, superseded).size).toBe(0);
    expect(acceptableBoundKeys("retired", undefined, superseded).size).toBe(0);
  });
  it("a retired key is never accepted through a chain either", () => {
    const keys = { currentKeyB64: "new", provenPredecessorsB64: ["retired", "old"] };
    expect([...acceptableBoundKeys("new", keys, superseded)].sort()).toEqual(["new", "old"]);
  });
  it("reads a retired-key map as well as a set", () => {
    expect(acceptableBoundKeys("retired", undefined, new Map([["retired", "account"]])).size).toBe(0);
  });
});

describe("trustedBoundKeys (a path with no caller's key)", () => {
  it("a pinned account: the current key and every proven predecessor", () => {
    const keys = { currentKeyB64: "new", provenPredecessorsB64: ["old"] };
    expect([...trustedBoundKeys(keys, undefined)].sort()).toEqual(["new", "old"]);
  });
  it("the pin wins over a binding", () => {
    const keys = { currentKeyB64: "new", provenPredecessorsB64: [] };
    expect([...trustedBoundKeys(keys, "bound")]).toEqual(["new"]);
  });
  it("an account this device never pinned: the key of its verified binding", () => {
    expect([...trustedBoundKeys(undefined, "bound")]).toEqual(["bound"]);
  });
  it("neither pinned nor bound: no key, so nobody is added under the id", () => {
    expect(trustedBoundKeys(undefined, undefined).size).toBe(0);
  });
  it("a key a reset retired is never trusted", () => {
    const keys = { currentKeyB64: "new", provenPredecessorsB64: [] };
    expect([...trustedBoundKeys(keys, undefined, new Set(["old"]))]).toEqual(["new"]);
    expect(trustedBoundKeys(undefined, "old", new Set(["old"])).size).toBe(0);
  });
});

describe("devicesBoundTo", () => {
  const bound = (d: { bound: string | null }): string | null => d.bound;
  it("keeps the devices bound to an accepted key, in order", () => {
    const devices = [
      { id: 1, bound: "k" },
      { id: 2, bound: "attacker" },
      { id: 3, bound: "k" },
    ];
    expect(devicesBoundTo(devices, new Set(["k"]), bound).map((d) => d.id)).toEqual([1, 3]);
  });
  it("refuses a device whose binding does not verify", () => {
    expect(devicesBoundTo([{ bound: null }], new Set(["k"]), bound)).toEqual([]);
  });
  it("the directory serving another account's devices under the asked id adds nobody", () => {
    const devices = [{ bound: "someone-else" }, { bound: "someone-else" }];
    expect(devicesBoundTo(devices, acceptableBoundKeys("expected", undefined), bound)).toEqual([]);
  });
  it("a key a reset retired is not accepted for the new identity", () => {
    // After an accepted reset the pin's proven chain is empty: the old key
    // is a superseded key, in no chain.
    const accepted = acceptableBoundKeys("new", { currentKeyB64: "new", provenPredecessorsB64: [] });
    expect(devicesBoundTo([{ bound: "old" }, { bound: "new" }], accepted, bound)).toEqual([{ bound: "new" }]);
  });
});
