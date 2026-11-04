// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0082 section 5 — the IndexedDB backend of `result-store.ts`.
 *
 * ```text
 *   db   "lakko-translations"  v1
 *   └── store "rows"  keyPath: "id"
 *         row { id: <hex digest>, accountId: <b64>, cls: "result" | "prefs" | "key",
 *               conv?: <hex tag>,
 *               ciphertext: Uint8Array  // [12-byte IV | AES-GCM ct+tag] }
 * ```
 *
 * `conv` lets a deleted conversation take its rows with it without naming
 * the conversation: it is HMAC-SHA-256 under a per-account key that is kept
 * in this database only as ciphertext under the device key (the one `key`
 * row), so a reader of the database who holds a conversation id cannot tell
 * which rows are that conversation's. A row without a tag (written before
 * the tag existed) is removed by Forget device and "Delete all" only.
 *
 * Every value is AES-GCM under the account's device root key, the posture of
 * `chat/pin-idb.ts`: a translation or a transcript is the text of a message,
 * so it never reaches the disk in the clear. Every read-write transaction
 * asks the ADR-0077 gate synchronously right before it is created, after the
 * encrypt and the open, with no await in between (`account-store-gate.ts`).
 */

import { accountStoresWritable } from "../auth/account-store-gate";
import type { ResultBackend, RowClass } from "./result-store";

export const TRANSLATIONS_DB_NAME = "lakko-translations";
const DB_VERSION = 1;
const STORE = "rows";
const IV_LEN = 12;

interface PersistedRow {
  id: string;
  accountId: string;
  cls: RowClass | "key";
  conv?: string;
  ciphertext: Uint8Array;
}

/**
 * The device key as `auth/key-store.ts` hands it out (its `DeviceCipher`):
 * a browser WebCrypto key or the desktop host's key. Named here by shape so
 * this module imports nothing outside the allowed list
 * (`fail-closed.test.ts`).
 */
export interface RowCipher {
  encrypt(iv: Uint8Array, plaintext: Uint8Array): Promise<ArrayBuffer>;
  decrypt(iv: Uint8Array, ciphertext: Uint8Array): Promise<ArrayBuffer>;
}

const TAG_DOMAIN = "lakko/translation/conversation-tag/v1";

/** The per-account tag keys, once opened. Dropped with the account's rows. */
const tagKeys = new Map<string, Promise<CryptoKey | null>>();

async function keyRowId(accountId: string): Promise<string> {
  const digest = new Uint8Array(
    await crypto.subtle.digest("SHA-256", new TextEncoder().encode(`${TAG_DOMAIN}\u0000key\u0000${accountId}`)),
  );
  return hexOf(digest);
}

function hexOf(bytes: Uint8Array): string {
  let hex = "";
  for (const b of bytes) hex += b.toString(16).padStart(2, "0");
  return hex;
}

function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === "undefined") {
      reject(new Error("IndexedDB unavailable in this environment"));
      return;
    }
    const req = indexedDB.open(TRANSLATIONS_DB_NAME, DB_VERSION);
    req.onupgradeneeded = (): void => {
      const db = req.result;
      if (!db.objectStoreNames.contains(STORE)) {
        // No index: the rows of one device are few, and a scan keeps the
        // store within what the test fakes implement.
        db.createObjectStore(STORE, { keyPath: "id" });
      }
    };
    req.onsuccess = (): void => resolve(req.result);
    req.onerror = (): void => reject(req.error ?? new Error("indexedDB.open failed"));
  });
}

function awaitTx(tx: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = (): void => resolve();
    tx.onabort = (): void => reject(tx.error ?? new Error("idb tx aborted"));
    tx.onerror = (): void => reject(tx.error ?? new Error("idb tx errored"));
  });
}

function awaitReq<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    req.onsuccess = (): void => resolve(req.result);
    req.onerror = (): void => reject(req.error ?? new Error("idb request failed"));
  });
}

async function encryptBlob(key: RowCipher, plaintext: Uint8Array): Promise<Uint8Array> {
  const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
  const ct = new Uint8Array(await key.encrypt(iv, plaintext));
  const out = new Uint8Array(IV_LEN + ct.length);
  out.set(iv, 0);
  out.set(ct, IV_LEN);
  return out;
}

async function decryptBlob(key: RowCipher, blob: Uint8Array): Promise<Uint8Array> {
  if (blob.length < IV_LEN + 16) throw new Error("encrypted translation row is too short");
  return new Uint8Array(await key.decrypt(blob.slice(0, IV_LEN), blob.slice(IV_LEN)));
}

/** The production backend. With no device key, reads find nothing and writes do nothing. */
export class IdbResultBackend implements ResultBackend {
  constructor(private readonly deviceKeyFor: (accountId: string) => Promise<RowCipher | null>) {}

  /**
   * The account's tag key: read from its row (opened with the device key),
   * or made and written on first use. `null` when there is no device key,
   * the account is sealed, or the row cannot be opened.
   */
  private tagKey(accountId: string, create: boolean): Promise<CryptoKey | null> {
    const cached = tagKeys.get(accountId);
    if (cached !== undefined) return cached;
    const p = (async (): Promise<CryptoKey | null> => {
      const deviceKey = await this.deviceKeyFor(accountId);
      if (deviceKey === null) return null;
      const id = await keyRowId(accountId);
      const db = await openDb();
      try {
        const tx = db.transaction(STORE, "readonly");
        const row = (await awaitReq(tx.objectStore(STORE).get(id))) as PersistedRow | undefined;
        await awaitTx(tx);
        let raw: Uint8Array | null = null;
        if (row !== undefined && row.accountId === accountId) {
          try {
            raw = await decryptBlob(deviceKey, row.ciphertext);
          } catch {
            raw = null;
          }
        }
        if (raw === null) {
          if (!create) return null;
          raw = crypto.getRandomValues(new Uint8Array(32));
          const ciphertext = await encryptBlob(deviceKey, raw);
          if (!accountStoresWritable(accountId)) return null;
          const wtx = db.transaction(STORE, "readwrite");
          wtx.objectStore(STORE).put({ id, accountId, cls: "key", ciphertext } satisfies PersistedRow);
          await awaitTx(wtx);
        }
        return await crypto.subtle.importKey("raw", raw.slice(), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
      } finally {
        db.close();
      }
    })();
    tagKeys.set(accountId, p);
    // A failure is not cached: the next call tries again.
    p.then(
      (k) => {
        if (k === null && tagKeys.get(accountId) === p) tagKeys.delete(accountId);
      },
      () => {
        if (tagKeys.get(accountId) === p) tagKeys.delete(accountId);
      },
    );
    return p;
  }

  /** The tag a conversation's rows carry, or `null` without a tag key. */
  private async tag(accountId: string, conversation: string, create: boolean): Promise<string | null> {
    const key = await this.tagKey(accountId, create);
    if (key === null) return null;
    const mac = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(`${TAG_DOMAIN}\u0000${conversation}`));
    return hexOf(new Uint8Array(mac));
  }

  async get(accountId: string, id: string): Promise<Uint8Array | null> {
    const key = await this.deviceKeyFor(accountId);
    if (key === null) return null;
    const db = await openDb();
    let row: PersistedRow | undefined;
    try {
      const tx = db.transaction(STORE, "readonly");
      row = (await awaitReq(tx.objectStore(STORE).get(id))) as PersistedRow | undefined;
      await awaitTx(tx);
    } finally {
      db.close();
    }
    if (row === undefined || row.accountId !== accountId) return null;
    try {
      return await decryptBlob(key, row.ciphertext);
    } catch {
      // A row this key cannot open (the device key changed) is unusable: the
      // result is made again on the next request.
      return null;
    }
  }

  async put(
    accountId: string,
    id: string,
    cls: RowClass,
    plaintext: Uint8Array,
    conversation: string,
  ): Promise<void> {
    const key = await this.deviceKeyFor(accountId);
    if (key === null) return;
    const ciphertext = await encryptBlob(key, plaintext);
    const conv = await this.tag(accountId, conversation, true);
    // Without a tag the row could not be found again when the conversation is
    // deleted, so it is not written at all.
    if (conv === null) return;
    const db = await openDb();
    try {
      if (!accountStoresWritable(accountId)) return;
      const tx = db.transaction(STORE, "readwrite");
      tx.objectStore(STORE).put({ id, accountId, cls, conv, ciphertext } satisfies PersistedRow);
      await awaitTx(tx);
    } finally {
      db.close();
    }
  }

  async delete(accountId: string, ids: readonly string[]): Promise<void> {
    if (ids.length === 0) return;
    const db = await openDb();
    try {
      // A delete after a wipe sealed the account is harmless, but the rule is
      // one rule: no read-write transaction for a sealed account.
      if (!accountStoresWritable(accountId)) return;
      const tx = db.transaction(STORE, "readwrite");
      const store = tx.objectStore(STORE);
      for (const id of ids) store.delete(id);
      await awaitTx(tx);
    } finally {
      db.close();
    }
  }

  async deleteClass(accountId: string, cls: RowClass): Promise<number> {
    const db = await openDb();
    try {
      if (!accountStoresWritable(accountId)) return 0;
      const tx = db.transaction(STORE, "readwrite");
      const store = tx.objectStore(STORE);
      const rows = ((await awaitReq(store.getAll())) as PersistedRow[]).filter((r) => r.accountId === accountId);
      let n = 0;
      for (const r of rows) {
        if (r.cls !== cls) continue;
        store.delete(r.id);
        n++;
      }
      await awaitTx(tx);
      return n;
    } finally {
      db.close();
    }
  }

  async deleteConversation(accountId: string, conversation: string): Promise<number> {
    const conv = await this.tag(accountId, conversation, false);
    if (conv === null) return 0;
    const db = await openDb();
    try {
      if (!accountStoresWritable(accountId)) return 0;
      const tx = db.transaction(STORE, "readwrite");
      const store = tx.objectStore(STORE);
      const rows = (await awaitReq(store.getAll())) as PersistedRow[];
      let n = 0;
      for (const r of rows) {
        if (r.accountId !== accountId || r.conv !== conv) continue;
        store.delete(r.id);
        n++;
      }
      await awaitTx(tx);
      return n;
    } finally {
      db.close();
    }
  }

  async count(accountId: string, cls: RowClass): Promise<number> {
    const db = await openDb();
    try {
      const tx = db.transaction(STORE, "readonly");
      const rows = (await awaitReq(tx.objectStore(STORE).getAll())) as PersistedRow[];
      await awaitTx(tx);
      return rows.filter((r) => r.accountId === accountId && r.cls === cls).length;
    } finally {
      db.close();
    }
  }
}

/** Erase every row of an account (Forget device). No-op when absent. */
export async function deleteTranslationRows(accountId: string): Promise<void> {
  tagKeys.delete(accountId);
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    const store = tx.objectStore(STORE);
    const rows = (await awaitReq(store.getAll())) as PersistedRow[];
    for (const r of rows) if (r.accountId === accountId) store.delete(r.id);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}
