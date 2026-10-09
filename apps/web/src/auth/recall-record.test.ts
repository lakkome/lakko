// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The recall record (ADR-0066 section 1.1): what it holds, that it is sealed
 * under the device key, and that it survives a reload (the store, over the
 * in-memory IndexedDB fake).
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { FakeIndexedDb } from "../test/fake-storage";
import {
  decodeRecallPlaintext,
  deleteRecallRecord,
  encodeRecallPlaintext,
  getRecallRecord,
  hasRecallRecord,
  listRecallAccounts,
  putRecallRecord,
  RECALL_SEED_LENGTH,
  type RecallRecord,
} from "./recall-record";

const ACCOUNT = "AAECAwQFBgcICQoLDA0ODw==";
const record = (over: Partial<RecallRecord> = {}): RecallRecord => ({
  accountIdB64: ACCOUNT,
  oldSeed: new Uint8Array(RECALL_SEED_LENGTH).fill(7),
  username: "alice",
  learnedAtMs: 1_800_000_000_000,
  ...over,
});

async function deviceKey(): Promise<CryptoKey> {
  return crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
}

let idb: FakeIndexedDb;
beforeEach(() => {
  idb = new FakeIndexedDb();
  (globalThis as unknown as Record<string, unknown>)["indexedDB"] = idb;
});
afterEach(() => {
  delete (globalThis as unknown as Record<string, unknown>)["indexedDB"];
});

describe("what the record holds", () => {
  it("is the time, the 32-byte old account seed and the username, and nothing else", () => {
    const plain = encodeRecallPlaintext(record());
    // version, 8 bytes of time, the seed, then the name.
    expect(plain).toHaveLength(1 + 8 + 32 + "alice".length);
    expect(plain[0]).toBe(1);
    expect(Array.from(plain.slice(9, 41))).toEqual(new Array(32).fill(7));
    expect(new TextDecoder().decode(plain.slice(41))).toBe("alice");
    const back = decodeRecallPlaintext(ACCOUNT, plain);
    expect(back).toEqual(record());
    expect(Object.keys(back ?? {}).sort()).toEqual(["accountIdB64", "learnedAtMs", "oldSeed", "username"]);
  });

  it("holds an account without a username", () => {
    expect(decodeRecallPlaintext(ACCOUNT, encodeRecallPlaintext(record({ username: "" })))?.username).toBe("");
  });

  it("refuses a seed that is not the 32-byte account seed (never the 64-byte phrase seed)", () => {
    expect(() => encodeRecallPlaintext(record({ oldSeed: new Uint8Array(64) }))).toThrow();
    expect(() => encodeRecallPlaintext(record({ oldSeed: new Uint8Array(31) }))).toThrow();
  });

  it("reads bytes that are not a record as none, never as a partial one", () => {
    const plain = encodeRecallPlaintext(record());
    expect(decodeRecallPlaintext(ACCOUNT, plain.slice(0, 40))).toBeNull();
    expect(decodeRecallPlaintext(ACCOUNT, new Uint8Array(0))).toBeNull();
    const otherVersion = plain.slice();
    otherVersion[0] = 2;
    expect(decodeRecallPlaintext(ACCOUNT, otherVersion)).toBeNull();
    const badName = new Uint8Array([...plain.slice(0, 41), 0xff, 0xfe]);
    expect(decodeRecallPlaintext(ACCOUNT, badName)).toBeNull();
    expect(decodeRecallPlaintext(ACCOUNT, new Uint8Array(41 + 200))).toBeNull();
  });
});

describe("the store", () => {
  it("round-trips under the device key and is found again by a new page", async () => {
    const key = await deviceKey();
    await putRecallRecord(record(), key);
    expect(await listRecallAccounts()).toEqual([ACCOUNT]);
    expect(await hasRecallRecord(ACCOUNT)).toBe(true);
    expect(await getRecallRecord(ACCOUNT, key)).toEqual(record());
  });

  it("stores ciphertext: neither the seed nor the username is in the row", async () => {
    const key = await deviceKey();
    await putRecallRecord(record({ username: "alice_in_the_row" }), key);
    const rows = idb.walk().filter((e) => e.where === "lakko-recall-record/records");
    expect(rows).toHaveLength(1);
    expect(rows[0]?.text.includes("alice_in_the_row")).toBe(false);
    // The row's own fields: the account id (the key) and the envelope.
    expect(rows[0]?.text.includes(ACCOUNT)).toBe(true);
  });

  it("does not open under another device key", async () => {
    await putRecallRecord(record(), await deviceKey());
    await expect(getRecallRecord(ACCOUNT, await deviceKey())).rejects.toBeDefined();
  });

  it("is gone once destroyed", async () => {
    const key = await deviceKey();
    await putRecallRecord(record(), key);
    await deleteRecallRecord(ACCOUNT);
    expect(await getRecallRecord(ACCOUNT, key)).toBeNull();
    expect(await hasRecallRecord(ACCOUNT)).toBe(false);
    expect(await listRecallAccounts()).toEqual([]);
  });
});
