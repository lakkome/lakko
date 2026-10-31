// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0084 — IndexedDB backend for the decrypted-avatar cache, encrypted
 * under the device root key (the posture of `profile-idb.ts`: a person's
 * photo is personal data and is never written to disk in the clear).
 *
 * ```text
 *   db   "lakko-avatars"  v1
 *   └── store "blobs"  keyPath: "id"
 *         row { id: "<accountId>|<blobIdB64>", ciphertext: Uint8Array  // [12-byte IV | AES-GCM ct+tag] }
 * ```
 *
 * The service worker never opens this database: background code shows
 * initials (ADR-0084 section 3).
 */

import { accountStoresWritable } from "../auth/account-store-gate";
import type { AvatarCacheBackend } from "./avatar-cache";
import type { DeviceCipher } from "../auth/device-cipher";

const DB_NAME = "lakko-avatars";
const DB_VERSION = 1;
const STORE = "blobs";
const IV_LEN = 12;

interface PersistedRow {
  id: string;
  ciphertext: Uint8Array;
}

function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === "undefined") {
      reject(new Error("IndexedDB unavailable in this environment"));
      return;
    }
    const req = indexedDB.open(DB_NAME, DB_VERSION);
    req.onupgradeneeded = (): void => {
      const db = req.result;
      if (!db.objectStoreNames.contains(STORE)) db.createObjectStore(STORE, { keyPath: "id" });
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

export class IdbAvatarBackend implements AvatarCacheBackend {
  constructor(
    private readonly accountId: string,
    private readonly deviceKey: () => Promise<DeviceCipher | null>,
  ) {}

  private rowId(blobIdB64: string): string {
    return `${this.accountId}|${blobIdB64}`;
  }

  async get(blobIdB64: string): Promise<Uint8Array | null> {
    const key = await this.deviceKey();
    if (key === null) return null;
    const db = await openDb();
    let row: PersistedRow | undefined;
    try {
      const tx = db.transaction(STORE, "readonly");
      const req = tx.objectStore(STORE).get(this.rowId(blobIdB64));
      row = await new Promise<PersistedRow | undefined>((resolve, reject) => {
        req.onsuccess = (): void => resolve(req.result as PersistedRow | undefined);
        req.onerror = (): void => reject(req.error ?? new Error("idb get failed"));
      });
      await awaitTx(tx);
    } finally {
      db.close();
    }
    if (row === undefined || row.ciphertext.length < IV_LEN + 16) return null;
    try {
      return new Uint8Array(
        await key.decrypt(row.ciphertext.slice(0, IV_LEN), row.ciphertext.slice(IV_LEN)),
      );
    } catch {
      return null;
    }
  }

  async put(blobIdB64: string, bytes: Uint8Array): Promise<void> {
    const key = await this.deviceKey();
    if (key === null) return;
    const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
    const ct = new Uint8Array(await key.encrypt(iv.slice(), bytes.slice()));
    const ciphertext = new Uint8Array(IV_LEN + ct.length);
    ciphertext.set(iv, 0);
    ciphertext.set(ct, IV_LEN);
    const db = await openDb();
    try {
      if (!accountStoresWritable(this.accountId)) return;
      const tx = db.transaction(STORE, "readwrite");
      tx.objectStore(STORE).put({ id: this.rowId(blobIdB64), ciphertext } satisfies PersistedRow);
      await awaitTx(tx);
    } finally {
      db.close();
    }
  }
}

/** Erase every cached avatar of an account (forget device). */
export async function deleteAvatarCache(accountId: string): Promise<void> {
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    const store = tx.objectStore(STORE);
    const req = store.getAllKeys();
    req.onsuccess = (): void => {
      for (const key of req.result) {
        if (typeof key === "string" && key.startsWith(`${accountId}|`)) store.delete(key);
      }
    };
    await awaitTx(tx);
  } finally {
    db.close();
  }
}
