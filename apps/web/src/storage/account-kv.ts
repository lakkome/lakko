// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Small per-account JSON rows, sealed under the device key (the posture of
 * every other local store): `lakko-account-kv`, one row per
 * `(account, name)`.
 *
 * For the ADR-0087 client state that is too small to deserve a database of
 * its own: the group photo and reactions settings, the conversation handle,
 * the knock seeds an admin holds, the requests sent, the owed photo uploads.
 * Each caller owns one `name` and its own JSON shape; this module only seals,
 * stores and opens. A row this device key cannot open reads as absent.
 *
 *   db   "lakko-account-kv" v1
 *   store "rows"  keyPath "id" = `${accountId}|${name}`
 */

import { accountStoresWritable } from "../auth/account-store-gate";
import type { DeviceCipher } from "../auth/device-cipher";
import { getDeviceKey } from "../auth/key-store";

const DB_NAME = "lakko-account-kv";
const DB_VERSION = 1;
const STORE = "rows";
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

/** Read one row, `null` when absent or unreadable. Throws only when the
 *  database cannot be opened (the caller then knows it could not read). */
export async function loadAccountJson(
  accountId: string,
  name: string,
  deviceKey?: DeviceCipher | null,
): Promise<unknown> {
  const key = deviceKey === undefined ? await getDeviceKey(accountId) : deviceKey;
  if (key === null) return null;
  const db = await openDb();
  let row: PersistedRow | undefined;
  try {
    const tx = db.transaction(STORE, "readonly");
    const req = tx.objectStore(STORE).get(`${accountId}|${name}`);
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
    const plain = new Uint8Array(
      await key.decrypt(row.ciphertext.slice(0, IV_LEN), row.ciphertext.slice(IV_LEN)),
    );
    return JSON.parse(new TextDecoder().decode(plain)) as unknown;
  } catch {
    return null;
  }
}

/** Write one row. A sealed account (Forget device under way) writes nothing. */
export async function saveAccountJson(
  accountId: string,
  name: string,
  value: unknown,
  deviceKey?: DeviceCipher | null,
): Promise<void> {
  const key = deviceKey === undefined ? await getDeviceKey(accountId) : deviceKey;
  if (key === null) return;
  const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
  const plain = new TextEncoder().encode(JSON.stringify(value));
  const ct = new Uint8Array(await key.encrypt(iv.slice(), plain));
  const ciphertext = new Uint8Array(IV_LEN + ct.length);
  ciphertext.set(iv, 0);
  ciphertext.set(ct, IV_LEN);
  const db = await openDb();
  try {
    if (!accountStoresWritable(accountId)) return;
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).put({ id: `${accountId}|${name}`, ciphertext } satisfies PersistedRow);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

/** Forget device: every row of the account. */
export async function deleteAccountKv(accountId: string): Promise<void> {
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    const store = tx.objectStore(STORE);
    const req = store.getAllKeys();
    req.onsuccess = (): void => {
      for (const k of req.result) {
        if (typeof k === "string" && k.startsWith(`${accountId}|`)) store.delete(k);
      }
    };
    await awaitTx(tx);
  } finally {
    db.close();
  }
}
