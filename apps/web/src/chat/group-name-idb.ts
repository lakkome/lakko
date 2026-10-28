// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0051 — IndexedDB backend for the shared-group-name store, encrypted
 * under the device root key. The same posture as `profile/profile-idb.ts`:
 * a group name is content, so it is never written to disk in the clear.
 *
 * ```text
 *   db   "lakko-group-names"  v1
 *   └── store "rows"  keyPath: "accountId"
 *         row { accountId: <b64>, ciphertext: Uint8Array  // [12-byte IV | AES-GCM ct+tag] }
 * ```
 *
 * One row per account; the plaintext is the JSON of
 * `group-name-store.ts`'s {@link encodeGroupNameRow}.
 */

import type { DeviceCipher } from "../auth/device-cipher";
import { accountStoresWritable } from "../auth/account-store-gate";
import {
  decodeGroupNameRow,
  encodeGroupNameRow,
  type GroupNameBackend,
  type StoredGroupName,
} from "./group-name-store";

const DB_NAME = "lakko-group-names";
const DB_VERSION = 1;
const STORE = "rows";
const IV_LEN = 12;

interface PersistedRow {
  accountId: string;
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
      if (!db.objectStoreNames.contains(STORE)) {
        db.createObjectStore(STORE, { keyPath: "accountId" });
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

async function encryptBlob(deviceKey: DeviceCipher, plaintext: Uint8Array): Promise<Uint8Array> {
  const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
  const ct = new Uint8Array(
    await deviceKey.encrypt(iv, plaintext),
  );
  const out = new Uint8Array(IV_LEN + ct.length);
  out.set(iv, 0);
  out.set(ct, IV_LEN);
  return out;
}

async function decryptBlob(deviceKey: DeviceCipher, blob: Uint8Array): Promise<Uint8Array> {
  if (blob.length < IV_LEN + 16) throw new Error("encrypted group-name row is too short");
  return new Uint8Array(
    await deviceKey.decrypt(blob.slice(0, IV_LEN), blob.slice(IV_LEN)),
  );
}

/**
 * Read one account's rows straight from IndexedDB — for callers that run
 * outside the provider (`chat/rehydrate.ts`, which titles restored groups
 * before any component is mounted). `null` when there is no row, no key,
 * or the row cannot be opened.
 */
export async function loadGroupNameRows(
  accountId: string,
  deviceKey: DeviceCipher | null,
): Promise<ReadonlyMap<string, StoredGroupName> | null> {
  if (deviceKey === null) return null;
  const db = await openDb();
  let row: PersistedRow | undefined;
  try {
    const tx = db.transaction(STORE, "readonly");
    const req = tx.objectStore(STORE).get(accountId);
    row = await new Promise<PersistedRow | undefined>((resolve, reject) => {
      req.onsuccess = (): void => resolve(req.result as PersistedRow | undefined);
      req.onerror = (): void => reject(req.error ?? new Error("idb get failed"));
    });
    await awaitTx(tx);
  } finally {
    db.close();
  }
  if (row === undefined) return null;
  try {
    const plain = await decryptBlob(deviceKey, row.ciphertext);
    return decodeGroupNameRow(new TextDecoder().decode(plain));
  } catch {
    // A row this key cannot open (the device key changed) is unusable, not
    // an error to surface: the store starts empty and rewrites it.
    return null;
  }
}

/**
 * The production backend: one encrypted row per account.
 *
 * @param deviceKeyFor  the account's device root key, or `null` when this
 *                      device holds none (the store then stays in memory)
 */
export class IdbGroupNameBackend implements GroupNameBackend {
  constructor(private readonly deviceKeyFor: (accountId: string) => Promise<DeviceCipher | null>) {}

  async load(accountId: string): Promise<ReadonlyMap<string, StoredGroupName> | null> {
    return loadGroupNameRows(accountId, await this.deviceKeyFor(accountId));
  }

  async save(accountId: string, rows: ReadonlyMap<string, StoredGroupName>): Promise<void> {
    const deviceKey = await this.deviceKeyFor(accountId);
    if (deviceKey === null) return;
    const json = encodeGroupNameRow(rows);
    const ciphertext = await encryptBlob(deviceKey, new TextEncoder().encode(json));
    const db = await openDb();
    try {
      if (!accountStoresWritable(accountId)) return;
      const tx = db.transaction(STORE, "readwrite");
      tx.objectStore(STORE).put({ accountId, ciphertext } satisfies PersistedRow);
      await awaitTx(tx);
    } finally {
      db.close();
    }
  }
}

/** Erase the row for an account (forget device). No-op when absent. */
export async function deleteGroupNameRow(accountId: string): Promise<void> {
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).delete(accountId);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}
