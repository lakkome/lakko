// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * IndexedDB backend for the cross-device sync snapshot, encrypted under the
 * device root key. The same shape as `read-watermark-idb.ts`.
 *
 * ```text
 *   db   "lakko-device-sync"  v1
 *   └── store "rows"  keyPath: "accountId"
 *         row { accountId: <b64>, ciphertext: Uint8Array  // [12-byte IV | AES-GCM ct+tag] }
 * ```
 *
 * One row per account; the plaintext is the JSON of
 * `sync-state-store.ts`'s {@link encodeSyncState}: the ADR-0058 synced state
 * bytes plus the folder edits not yet sent. It holds DRAFTS, which are
 * messages the person has not sent yet, and folder names, so it is never
 * written in the clear.
 */

import {
  decodeSyncState,
  encodeSyncState,
  type SyncStateRow,
} from "./sync-state-store";

const DB_NAME = "lakko-device-sync";
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

async function encryptBlob(deviceKey: CryptoKey, plaintext: Uint8Array): Promise<Uint8Array> {
  const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
  const ct = new Uint8Array(
    await crypto.subtle.encrypt({ name: "AES-GCM", iv: iv.slice() }, deviceKey, plaintext.slice()),
  );
  const out = new Uint8Array(IV_LEN + ct.length);
  out.set(iv, 0);
  out.set(ct, IV_LEN);
  return out;
}

async function decryptBlob(deviceKey: CryptoKey, blob: Uint8Array): Promise<Uint8Array> {
  if (blob.length < IV_LEN + 16) throw new Error("encrypted device-sync row is too short");
  return new Uint8Array(
    await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: blob.slice(0, IV_LEN) },
      deviceKey,
      blob.slice(IV_LEN),
    ),
  );
}

/**
 * Read one account's snapshot. `null` when there is no row, no key, or the
 * row cannot be opened - all three mean "start from what the journal brings",
 * which is where this client was before the snapshot existed.
 */
export async function loadSyncState(
  accountId: string,
  deviceKey: CryptoKey | null,
): Promise<SyncStateRow | null> {
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
    return decodeSyncState(new TextDecoder().decode(plain));
  } catch {
    // A row this key cannot open (the device key changed) is unusable, not an
    // error to surface: the snapshot starts empty and is rewritten.
    return null;
  }
}

/** Replace one account's snapshot. */
export async function saveSyncState(
  accountId: string,
  deviceKey: CryptoKey | null,
  state: SyncStateRow,
): Promise<void> {
  if (deviceKey === null) return;
  const plain = new TextEncoder().encode(encodeSyncState(state));
  const ciphertext = await encryptBlob(deviceKey, plain);
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).put({ accountId, ciphertext } satisfies PersistedRow);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

/** Forget one account's snapshot, for logout and forget-this-device. */
export async function deleteSyncState(accountId: string): Promise<void> {
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).delete(accountId);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

/**
 * Merge a restored backup's synced state into this account's stored row
 * (ADR-0058 section 5.2). Always a merge, never "seed only when empty": the
 * fold keeps the newer register from each side. `merge` combines two fold
 * byte strings (`mergeRestoredFold` over the wasm core); the pending folder
 * edits already stored are kept as they are.
 */
export async function mergeSyncStateFromBackup(
  accountId: string,
  deviceKey: CryptoKey | null,
  synced: Uint8Array,
  merge: (local: Uint8Array | null, backup: Uint8Array) => Uint8Array,
): Promise<void> {
  if (deviceKey === null || synced.length === 0) return;
  const existing = await loadSyncState(accountId, deviceKey);
  const local = existing !== null && existing.synced.length > 0 ? existing.synced : null;
  await saveSyncState(accountId, deviceKey, {
    synced: merge(local, synced),
    pending: existing?.pending ?? [],
  });
}
