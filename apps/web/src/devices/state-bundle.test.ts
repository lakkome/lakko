// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { bindContactsAccount } from "../contacts/local-store";
import {
  applyStateBundle,
  clearKnownGroupIds,
  decodeStateBundle,
  loadKnownGroupIds,
  pendingKnownGroups,
  saveKnownGroupIds,
  encodeStateBundle,
  isStateRow,
  unwrapStateRow,
  wrapStateRow,
  type StateBundleV3,
} from "./state-bundle";

const bundle: StateBundleV3 = {
  v: 3,
  contacts: [
    {
      signaturePubkeyB64: "c2ln",
      displayName: "Bob",
      acceptedAtMs: 1_700_000_000_000,
      accountIdHex: "00112233445566778899aabbccddeeff",
    },
  ],
  pins: [
    {
      accountIdB64: "ABEiM0RVZneImaq7zN3u/w==",
      pinnedSignaturePubkeyB64: "c2ln",
      rotationMarker: 0,
      provenPredecessorsB64: [],
    },
  ],
  knownGroupIdsB64: ["Z3JvdXA="],
  ownPredecessors: [],
  groupNames: [],
  profiles: [],
  pendingContacts: [],
};

describe("state bundle codec (ADR-0050 section 2.9)", () => {
  it("round-trips", () => {
    expect(decodeStateBundle(encodeStateBundle(bundle))).toEqual(bundle);
  });

  it("refuses a bundle of another version or a malformed contact", () => {
    for (const v of [1, 2, 4, "3"]) {
      const other = { ...bundle, v } as unknown as StateBundleV3;
      expect(decodeStateBundle(encodeStateBundle(other))).toBeNull();
    }
    const broken = new TextEncoder().encode(
      JSON.stringify({
        v: 3,
        contacts: [{ displayName: 1 }],
        pins: [],
        knownGroupIdsB64: [],
        ownPredecessors: [],
        groupNames: [],
        profiles: [],
        pendingContacts: [],
      }),
    );
    expect(decodeStateBundle(broken)).toBeNull();
    expect(decodeStateBundle(new Uint8Array([0x7b]))).toBeNull();
  });

  it("tags the mailbox row so the other polls can skip it", () => {
    const sealed = new Uint8Array([1, 2, 3, 4]);
    const row = wrapStateRow(sealed);
    expect(isStateRow(row)).toBe(true);
    expect(unwrapStateRow(row)).toEqual(sealed);
    expect(isStateRow(sealed)).toBe(false);
    expect(unwrapStateRow(sealed)).toBeNull();
  });
});

describe("known group ids belong to the paired account (2026-09-30)", () => {
  const CARA = "Y2FyYS1hY2NvdW50LTAwMDA=";
  const EVE = "ZXZlLWFjY291bnQtMDAwMDA=";
  let data: Map<string, string>;

  beforeEach(() => {
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

  it("a bundle applied into one account never shows another account as synchronizing", () => {
    applyStateBundle(bundle, CARA);
    bindContactsAccount(EVE);
    expect(loadKnownGroupIds()).toEqual([]);
    expect(pendingKnownGroups([])).toBe(0);
    bindContactsAccount(CARA);
    expect(loadKnownGroupIds()).toEqual(["Z3JvdXA="]);
    expect(pendingKnownGroups([])).toBe(1);
  });

  it("with no account a read is empty and a write is a no-op", () => {
    saveKnownGroupIds(["Z3JvdXA="]);
    expect(data.size).toBe(0);
    expect(loadKnownGroupIds()).toEqual([]);
    expect(pendingKnownGroups([])).toBe(0);
  });

  it("the list clears itself for its own account once every group is held", () => {
    saveKnownGroupIds(["a", "b"], CARA);
    saveKnownGroupIds(["a"], EVE);
    expect(pendingKnownGroups(["a", "b"], CARA)).toBe(0);
    expect(loadKnownGroupIds(CARA)).toEqual([]);
    expect(loadKnownGroupIds(EVE)).toEqual(["a"]);
  });

  it("the retired device-wide list is not read, and Forget device drops only the own account's", () => {
    data.set("lakko.pairing.known-groups.v1", JSON.stringify(["old"]));
    saveKnownGroupIds(["a"], CARA);
    saveKnownGroupIds(["b"], EVE);
    bindContactsAccount(CARA);
    expect(loadKnownGroupIds()).toEqual(["a"]);

    clearKnownGroupIds(CARA);

    expect(loadKnownGroupIds(CARA)).toEqual([]);
    expect(loadKnownGroupIds(EVE)).toEqual(["b"]);
    expect(data.has("lakko.pairing.known-groups.v1")).toBe(false);
  });
});
