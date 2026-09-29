// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 — IndexedDB backend for the registrar-seed store, encrypted under
 * the device root key, the posture every per-account secret has on web
 * (`chat/group-name-idb.ts`, `profile/profile-idb.ts`).
 *
 * ```text
 *   db   "lakko-registrar-seeds"  v1
 *   └── store "rows"  keyPath: "accountId"
 *         row { accountId: <b64>, ciphertext: Uint8Array  // [12-byte IV | AES-GCM ct+tag] }
 * ```
 *
 * One row per account; the plaintext is the JSON of
 * `registrar-seeds.ts`'s {@link encodeRegistrarSeedRows}.
 *
 * The same shape, in its own database `lakko-pending-root-seeds`, keeps the
 * old root seed a new owner received until its takeover lands
 * (`root-handover.ts`, the second security pass, finding 4).
 */

import {
  decodeRegistrarSeedRows,
  encodeRegistrarSeedRows,
  type RegistrarSeedBackend,
  type StoredRegistrarSeed,
} from "./registrar-seeds";
import {
  decodePendingRootSeedRows,
  encodePendingRootSeedRows,
  type PendingRootSeedBackend,
} from "./root-handover";

const DB_NAME = "lakko-registrar-seeds";
const PENDING_ROOT_DB_NAME = "lakko-pending-root-seeds";
const DB_VERSION = 1;
const STORE = "rows";
const IV_LEN = 12;

interface PersistedRow {
  accountId: string;
  ciphertext: Uint8Array;
}

function openDb(name: string = DB_NAME): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === "undefined") {
      reject(new Error("IndexedDB unavailable in this environment"));
      return;
    }
    const req = indexedDB.open(name, DB_VERSION);
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
  if (blob.length < IV_LEN + 16) throw new Error("encrypted registrar-seed row is too short");
  return new Uint8Array(
    await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: blob.slice(0, IV_LEN) },
      deviceKey,
      blob.slice(IV_LEN),
    ),
  );
}

/** One encrypted JSON row per account in database `dbName`. */
class IdbEncryptedRows<T> {
  constructor(
    private readonly dbName: string,
    private readonly deviceKeyFor: (accountId: string) => Promise<CryptoKey | null>,
    private readonly encode: (rows: ReadonlyMap<string, T>) => string,
    private readonly decode: (json: string) => Map<string, T>,
  ) {}

  async load(accountId: string): Promise<ReadonlyMap<string, T> | null> {
    const deviceKey = await this.deviceKeyFor(accountId);
    if (deviceKey === null) return null;
    const db = await openDb(this.dbName);
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
      return this.decode(new TextDecoder().decode(plain));
    } catch {
      // A row this key cannot open is unusable: start empty (the owner's
      // next appointment re-sends a registrar seed; a pending root seed is
      // re-sent by the outgoing owner while its probe says it is root).
      return null;
    }
  }

  async save(accountId: string, rows: ReadonlyMap<string, T>): Promise<void> {
    const deviceKey = await this.deviceKeyFor(accountId);
    if (deviceKey === null) return;
    const ciphertext = await encryptBlob(deviceKey, new TextEncoder().encode(this.encode(rows)));
    const db = await openDb(this.dbName);
    try {
      const tx = db.transaction(STORE, "readwrite");
      tx.objectStore(STORE).put({ accountId, ciphertext } satisfies PersistedRow);
      await awaitTx(tx);
    } finally {
      db.close();
    }
  }
}

/** The production registrar-seed backend. `deviceKeyFor` returns the
 *  account's device root key, or `null` when this device holds none (the
 *  store then stays in memory for the session). */
export class IdbRegistrarSeedBackend
  extends IdbEncryptedRows<StoredRegistrarSeed>
  implements RegistrarSeedBackend
{
  constructor(deviceKeyFor: (accountId: string) => Promise<CryptoKey | null>) {
    super(DB_NAME, deviceKeyFor, encodeRegistrarSeedRows, decodeRegistrarSeedRows);
  }
}

/** The production pending-root-seed backend (same protection). */
export class IdbPendingRootSeedBackend
  extends IdbEncryptedRows<Uint8Array>
  implements PendingRootSeedBackend
{
  constructor(deviceKeyFor: (accountId: string) => Promise<CryptoKey | null>) {
    super(PENDING_ROOT_DB_NAME, deviceKeyFor, encodePendingRootSeedRows, decodePendingRootSeedRows);
  }
}

/** Erase an account's pending root seeds (forget device). */
export async function deletePendingRootSeedRow(accountId: string): Promise<void> {
  const db = await openDb(PENDING_ROOT_DB_NAME);
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).delete(accountId);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

/** Erase the row for an account (forget device). No-op when absent. */
export async function deleteRegistrarSeedRow(accountId: string): Promise<void> {
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).delete(accountId);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}
