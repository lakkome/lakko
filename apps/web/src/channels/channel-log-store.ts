// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * IndexedDB-backed encrypted store for the per-account channel log.
 *
 * Same shape as `chat/message-log-store.ts`: one row per
 * `accountIdB64`, the JSON payload (`channel-log-codec.ts`) sealed
 * with AES-GCM under the device root key. The codec is a separate
 * module so it stays testable without IndexedDB.
 *
 * ```text
 *   db   "lakko-channel-log"  v1
 *   └── store "logs"  keyPath: "accountId" (string)
 *         row {
 *           accountId:  <b64-uuidv7>,
 *           ciphertext: Uint8Array   // [12-byte IV | AES-GCM ct+tag]
 *         }
 * ```
 *
 * Write serialisation lives in `state/channels-state.ts` (one
 * flush chain per page); this module is plain CRUD.
 */

import { accountStoresWritable, requireAccountStoresWritable } from "../auth/account-store-gate";
import type { ChannelLogPayload } from "./channel-log-codec";

const DB_NAME = "lakko-channel-log";
const DB_VERSION = 1;
const STORE = "logs";
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

function awaitTx<T>(tx: IDBTransaction, value: T): Promise<T> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = (): void => resolve(value);
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
  if (blob.length < IV_LEN + 16) {
    throw new Error("encrypted channel log is too short to be valid");
  }
  const iv = blob.slice(0, IV_LEN);
  const ct = blob.slice(IV_LEN);
  return new Uint8Array(await crypto.subtle.decrypt({ name: "AES-GCM", iv }, deviceKey, ct));
}

/** Persist (or replace) the channel log for an account. Rejects on an
 *  IDB / WebCrypto failure; the caller logs and retries on the next
 *  mutation.
 *
 *  A write for an account whose stores are sealed (`auth/account-store-gate`)
 *  is skipped, unless `durable` is set: then it THROWS
 *  `AccountStoresSealedError`, for a caller that publishes only after the
 *  write landed (the co-publisher knock, `flushChannelLogDurable`). */
export async function saveChannelLog(
  accountIdB64: string,
  deviceKey: CryptoKey,
  payload: ChannelLogPayload,
  options: { readonly durable?: boolean } = {},
): Promise<void> {
  const plaintext = new TextEncoder().encode(JSON.stringify(payload));
  const ciphertext = await encryptBlob(deviceKey, plaintext);
  const row: PersistedRow = { accountId: accountIdB64, ciphertext };
  const db = await openDb();
  try {
    // Asked synchronously right before the transaction (the gate's rule).
    if (options.durable === true) requireAccountStoresWritable(accountIdB64);
    else if (!accountStoresWritable(accountIdB64)) return;
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).put(row);
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
}

/**
 * Read + decrypt the channel log for `accountIdB64`. `null` when there
 * is no row, the AES-GCM open fails (device key rotated, blob tampered)
 * or the JSON does not parse — the caller starts from an empty log.
 */
export async function loadChannelLog(
  accountIdB64: string,
  deviceKey: CryptoKey,
): Promise<ChannelLogPayload | null> {
  const db = await openDb();
  let row: PersistedRow | undefined;
  try {
    const tx = db.transaction(STORE, "readonly");
    const req = tx.objectStore(STORE).get(accountIdB64);
    row = await new Promise<PersistedRow | undefined>((resolve, reject) => {
      req.onsuccess = (): void => resolve(req.result as PersistedRow | undefined);
      req.onerror = (): void => reject(req.error ?? new Error("idb get failed"));
    });
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
  if (row === undefined) return null;
  let plaintext: Uint8Array;
  try {
    plaintext = await decryptBlob(deviceKey, row.ciphertext);
  } catch {
    return null;
  }
  try {
    return JSON.parse(new TextDecoder().decode(plaintext)) as ChannelLogPayload;
  } catch {
    return null;
  }
}

/** Erase the channel log for `accountIdB64`. No-op when absent. Called
 *  from `forgetDevice` beside `deleteMessageLog`. */
export async function deleteChannelLog(accountIdB64: string): Promise<void> {
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).delete(accountIdB64);
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
}
