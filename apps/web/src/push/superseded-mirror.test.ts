// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0065 R.8 - the service worker's copy of the retired keys. */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { FakeIndexedDb } from "../test/fake-storage";
import {
  clearSupersededMirrorForAccount,
  sameMirrorRow,
  SUPERSEDED_MIRROR_DB,
  SUPERSEDED_MIRROR_STORE,
  supersededMirrorRow,
  writeSupersededMirror,
} from "./superseded-mirror";

const ACCOUNT = "YWNjb3VudA==";

describe("supersededMirrorRow", () => {
  it("holds every retired key once, sorted, so one set always writes one row", () => {
    expect(supersededMirrorRow(ACCOUNT, ["b", "a", "b", ""])).toEqual({ accountId: ACCOUNT, keys: ["a", "b"] });
    expect(supersededMirrorRow(ACCOUNT, new Map([["z", 1], ["y", 2]]).keys())).toEqual({
      accountId: ACCOUNT,
      keys: ["y", "z"],
    });
  });
  it("no retired key is an empty row, not no row: the worker then drops nothing", () => {
    expect(supersededMirrorRow(ACCOUNT, [])).toEqual({ accountId: ACCOUNT, keys: [] });
  });
});

describe("sameMirrorRow", () => {
  const row = supersededMirrorRow(ACCOUNT, ["a", "b"]);
  it("is the same set for the same account", () => {
    expect(sameMirrorRow(row, supersededMirrorRow(ACCOUNT, ["b", "a"]))).toBe(true);
  });
  it("differs on a key, on the account, and from nothing", () => {
    expect(sameMirrorRow(row, supersededMirrorRow(ACCOUNT, ["a"]))).toBe(false);
    expect(sameMirrorRow(row, supersededMirrorRow(ACCOUNT, ["a", "c"]))).toBe(false);
    expect(sameMirrorRow(row, supersededMirrorRow("b3RoZXI=", ["a", "b"]))).toBe(false);
    expect(sameMirrorRow(null, row)).toBe(false);
  });
});

describe("the mirror in IndexedDB", () => {
  let idb: FakeIndexedDb;
  beforeEach(() => {
    idb = new FakeIndexedDb();
    (globalThis as unknown as Record<string, unknown>)["indexedDB"] = idb;
  });
  afterEach(() => {
    delete (globalThis as unknown as Record<string, unknown>)["indexedDB"];
  });

  const stored = (): string[] =>
    idb
      .walk()
      .filter((r) => r.where.includes(SUPERSEDED_MIRROR_DB) && r.where.includes(SUPERSEDED_MIRROR_STORE))
      .map((r) => r.text);

  it("writes the row the worker reads, replaces it when the set changes, and removes it with the account", async () => {
    await writeSupersededMirror(ACCOUNT, ["k2", "k1"]);
    expect(stored().join("|")).toContain("k1");
    expect(stored().join("|")).toContain(ACCOUNT);
    await writeSupersededMirror(ACCOUNT, ["k3"]);
    const after = stored().join("|");
    expect(after).toContain("k3");
    expect(after).not.toContain("k1");
    await clearSupersededMirrorForAccount(ACCOUNT);
    expect(stored().join("|")).not.toContain(ACCOUNT);
  });
});
