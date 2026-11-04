// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * IndexedDB-backed store for the device root `CryptoKey`.
 *
 * `CryptoKey` instances created with `extractable: false` are
 * *handles*, not key bytes. The browser keeps the material inside
 * the WebCrypto sandbox and serialises only the opaque reference
 * via the structured-clone algorithm — that is what lets us drop
 * a non-extractable key into IDB without leaking the bytes to
 * page-level JavaScript.
 *
 * One key per account; the schema is intentionally minimal:
 *
 * ```text
 *   db   "lakko-auth"  v1
 *   ├── store "device_keys"  keyPath: "accountId"  (string)
 *         row { accountId: <b64-uuidv7>, key: CryptoKey }
 * ```
 *
 * The functions trade in {@link DeviceCipher}. On the web the record
 * still holds the bare `CryptoKey` (the service worker,
 * `public/sw-push-decrypt.js`, reads it straight from this store, so the
 * format stays byte-identical); a {@link WebDeviceCipher} is wrapped around
 * it on read. In the desktop build (ADR-0085 section 2) the key never
 * enters the page: the store gate is asked here, then the Tauri host keeps
 * the key under the operating system's key store. A
 * {@link KeyStoreUnavailableError} from the host propagates; it is never
 * read as "no key", which would re-derive or fall back.
 *
 * The IndexedDB path has no Node test (IDB does not exist there); the
 * desktop path is tested in `key-store.test.ts` against a mocked host.
 */

import { isDesktop } from "../platform";
import {
  type DeviceCipher,
  HostDeviceCipher,
  WebDeviceCipher,
  hostDeleteDeviceKey,
  hostGetDeviceKey,
  hostPutDeviceKey,
} from "./device-cipher";
import {
  accountStoresWritable,
  accountStoresWritableSince,
  type WipeMark,
} from "./account-store-gate";

const DB_NAME = "lakko-auth";
const DB_VERSION = 1;
const STORE = "device_keys";

interface DeviceKeyRecord {
  /** base64(account_id) — the same string the rest of the auth
   *  layer uses for routing. */
  accountId: string;
  /** Non-extractable AES-256-GCM `CryptoKey`. Survives IDB
   *  round-trip via structured-clone. */
  key: CryptoKey;
}

/** Open (or create on first run) the auth database. */
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
    req.onerror = (): void =>
      reject(req.error ?? new Error("indexedDB.open failed"));
  });
}

/**
 * Wrap a transaction operation in a `Promise`. Centralises the
 * `oncomplete` / `onabort` plumbing so the call sites stay tidy.
 */
function awaitTx<T>(tx: IDBTransaction, value: T): Promise<T> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = (): void => resolve(value);
    tx.onabort = (): void => reject(tx.error ?? new Error("idb tx aborted"));
    tx.onerror = (): void => reject(tx.error ?? new Error("idb tx errored"));
  });
}

/** Persist a device root key for the given account. Replaces an
 *  existing record if present. Resolves `false` when the store gate refused
 *  the write (the account's data was wiped; under `wipes`, wiped since that
 *  mark was taken). */
export async function putDeviceKey(
  accountIdB64: string,
  cipher: DeviceCipher,
  wipes?: WipeMark,
): Promise<boolean> {
  const writable = (): boolean =>
    wipes === undefined
      ? accountStoresWritable(accountIdB64)
      : accountStoresWritableSince(wipes, accountIdB64);
  if (isDesktop()) {
    if (!(cipher instanceof HostDeviceCipher)) {
      throw new Error("putDeviceKey: the desktop store takes a host cipher only");
    }
    if (!writable()) return false;
    await hostPutDeviceKey(accountIdB64, cipher);
    return true;
  }
  if (!(cipher instanceof WebDeviceCipher)) {
    throw new Error("putDeviceKey: the web store takes a WebCrypto cipher only");
  }
  const key = cipher.cryptoKey;
  const db = await openDb();
  try {
    // Asked after the open, immediately before the transaction (the gate rule).
    if (!writable()) return false;
    const tx = db.transaction(STORE, "readwrite");
    const record: DeviceKeyRecord = { accountId: accountIdB64, key };
    tx.objectStore(STORE).put(record);
    await awaitTx(tx, undefined);
    return true;
  } finally {
    db.close();
  }
}

/** Read the device root key for the given account, or `null` if
 *  no record exists. */
export async function getDeviceKey(
  accountIdB64: string,
): Promise<DeviceCipher | null> {
  if (isDesktop()) return hostGetDeviceKey(accountIdB64);
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readonly");
    const get = tx.objectStore(STORE).get(accountIdB64);
    const record = await new Promise<DeviceKeyRecord | undefined>(
      (resolve, reject) => {
        get.onsuccess = (): void =>
          resolve(get.result as DeviceKeyRecord | undefined);
        get.onerror = (): void =>
          reject(get.error ?? new Error("idb get failed"));
      },
    );
    return record?.key ? new WebDeviceCipher(record.key) : null;
  } finally {
    db.close();
  }
}

/** Erase the device root key for the given account. No-op if
 *  there was no record. */
export async function deleteDeviceKey(accountIdB64: string): Promise<void> {
  if (isDesktop()) {
    await hostDeleteDeviceKey(accountIdB64);
    return;
  }
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).delete(accountIdB64);
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
}
