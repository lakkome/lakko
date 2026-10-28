// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0082 section 5 — the results store over the IndexedDB backend: only
 * ciphertext reaches the database, no row names a conversation or a message,
 * a sealed account takes no write (ADR-0077), a deleted message takes its
 * results, and Forget device leaves nothing of the account.
 */

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { resetAccountStoreGateForTests, sealAccountStoresForWipe } from "../auth/account-store-gate";
import { FakeIndexedDb } from "../test/fake-storage";
import { deleteTranslationRows, IdbResultBackend, TRANSLATIONS_DB_NAME } from "./result-idb";
import { decodePrefs, decodeResult, ResultStore, rowId } from "./result-store";

const A = "YWNjb3VudEE=";
const B = "YWNjb3VudEI=";
const CONV = "Z3JvdXAx";

let idb: FakeIndexedDb;
let key: CryptoKey;

beforeEach(async () => {
  idb = new FakeIndexedDb();
  (globalThis as unknown as Record<string, unknown>)["indexedDB"] = idb;
  resetAccountStoreGateForTests(null);
  key = await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
});

afterEach(() => {
  delete (globalThis as unknown as Record<string, unknown>)["indexedDB"];
  resetAccountStoreGateForTests(null);
});

function store(): ResultStore {
  return new ResultStore(new IdbResultBackend(() => Promise.resolve(key)));
}

/** Every value in the translations database, as text. */
function everything(): string {
  const db = idb.dbs.get(TRANSLATIONS_DB_NAME);
  if (db === undefined) return "";
  const out: string[] = [];
  for (const s of db.stores.values()) {
    for (const [k, v] of (s as unknown as { rows: Map<unknown, unknown> }).rows ?? []) {
      out.push(JSON.stringify(k), JSON.stringify(v, (_k, x) => (x instanceof Uint8Array ? [...x] : x)));
    }
  }
  return out.join("\n");
}

const translation = {
  kind: "translation",
  lastOpId: 0,
  source: "es",
  target: "en",
  text: "hello friends",
} as const;

describe("the translation result store", () => {
  it("round-trips a result and the conversation choices", async () => {
    const s = store();
    await s.saveResult(A, "text", CONV, 7, translation);
    await s.savePrefs(A, CONV, { autoTranslate: true, translationSource: "es", transcriptionLanguage: null, dismissed: ["de"] });
    const fresh = store();
    expect(await fresh.result(A, "text", CONV, 7)).toEqual(translation);
    expect((await fresh.prefs(A, CONV)).autoTranslate).toBe(true);
    expect(await fresh.count(A)).toBe(1);
  });

  it("writes ciphertext only, under ids that name neither conversation nor message", async () => {
    const s = store();
    await s.saveResult(A, "text", CONV, 7, translation);
    const dump = everything();
    expect(dump.length).toBeGreaterThan(0);
    expect(dump).not.toContain("hello friends");
    expect(dump).not.toContain(CONV);
    expect(dump).not.toContain('"es"');
    const id = await rowId(A, "text", CONV, 7);
    expect(id).toMatch(/^[0-9a-f]{64}$/);
  });

  it("takes no write for a sealed account", async () => {
    sealAccountStoresForWipe([A]);
    const s = store();
    await s.saveResult(A, "text", CONV, 7, translation);
    expect(await store().result(A, "text", CONV, 7)).toBeNull();
  });

  it("forgets a deleted message, every slot of it", async () => {
    const s = store();
    await s.saveResult(A, "text", CONV, 7, translation);
    await s.saveResult(A, "transcript", CONV, 7, {
      kind: "transcript",
      lastOpId: 0,
      language: "es",
      text: "hola",
      complete: true,
    });
    await s.forgetMessage(A, CONV, 7);
    const fresh = store();
    expect(await fresh.result(A, "text", CONV, 7)).toBeNull();
    expect(await fresh.result(A, "transcript", CONV, 7)).toBeNull();
    expect(await fresh.count(A)).toBe(0);
  });

  it("Forget device removes one account's rows and leaves another's", async () => {
    const s = store();
    await s.saveResult(A, "text", CONV, 7, translation);
    await s.saveResult(B, "text", CONV, 7, translation);
    await deleteTranslationRows(A);
    const fresh = store();
    expect(await fresh.count(A)).toBe(0);
    expect(await fresh.count(B)).toBe(1);
  });

  it("Delete all keeps the conversation choices", async () => {
    const s = store();
    await s.saveResult(A, "text", CONV, 7, translation);
    await s.savePrefs(A, CONV, { autoTranslate: true, translationSource: null, transcriptionLanguage: null, dismissed: [] });
    expect(await s.deleteAll(A)).toBe(1);
    const fresh = store();
    expect(await fresh.count(A)).toBe(0);
    expect((await fresh.prefs(A, CONV)).autoTranslate).toBe(true);
  });

  it("a deleted conversation takes its rows, and only its rows", async () => {
    const OTHER = "Z3JvdXAy";
    const s = store();
    await s.saveResult(A, "text", CONV, 7, translation);
    await s.saveResult(A, "transcript", CONV, 8, {
      kind: "transcript",
      lastOpId: 0,
      language: "es",
      text: "hola",
      complete: true,
    });
    await s.savePrefs(A, CONV, { autoTranslate: true, translationSource: null, transcriptionLanguage: null, dismissed: [] });
    await s.saveResult(A, "text", OTHER, 7, translation);
    await s.saveResult(B, "text", CONV, 7, translation);
    // The tag names no conversation in the clear.
    expect(everything()).not.toContain(CONV);
    expect(everything()).not.toContain(OTHER);

    await s.forgetConversation(A, CONV);
    const fresh = store();
    expect(await fresh.result(A, "text", CONV, 7)).toBeNull();
    expect(await fresh.result(A, "transcript", CONV, 8)).toBeNull();
    expect((await fresh.prefs(A, CONV)).autoTranslate).toBe(false);
    expect(await fresh.result(A, "text", OTHER, 7)).toEqual(translation);
    expect(await fresh.result(B, "text", CONV, 7)).toEqual(translation);
  });

  it("decodes nothing it does not recognise", () => {
    const enc = (v: unknown): Uint8Array => new TextEncoder().encode(JSON.stringify(v));
    expect(decodeResult(enc({ kind: "x", lastOpId: 1, text: "a" }))).toBeNull();
    expect(decodeResult(new Uint8Array([0xff]))).toBeNull();
    expect(decodePrefs(new Uint8Array([0xff])).autoTranslate).toBe(false);
  });
});
