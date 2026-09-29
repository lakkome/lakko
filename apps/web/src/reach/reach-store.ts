// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0058 - encrypted per-account rows for scoped discovery: this device's
 * reach book, the grants it received, the "who can find me" setting and the
 * introduction bundles its contacts relayed.
 *
 * Reach grants are secrets (whoever holds one can write to the owner's
 * mailbox), so every row is sealed under the device root key, the posture of
 * `chat/group-name-idb.ts`: AES-256-GCM, a fresh 12-byte IV per write.
 *
 * ```text
 *   db   "lakko-reach"  v1
 *   └── store "rows"  keyPath: "key"   ("<accountIdB64>|<kind>")
 *         row { key, ciphertext: Uint8Array  // [12-byte IV | AES-GCM ct+tag] }
 * ```
 *
 * Without a device key (a half-reset device) or without IndexedDB (the host
 * tests) every read is `null` and every write a no-op: callers keep their
 * state in memory for the session.
 */
import { getDeviceKey } from "../auth/key-store";

const DB_NAME = "lakko-reach";
const DB_VERSION = 1;
const STORE = "rows";
const IV_LEN = 12;

/** The rows kept per account. */
export type ReachRowKind = "book" | "grants" | "visibility" | "introductions" | "publication";

interface PersistedRow {
  key: string;
  ciphertext: Uint8Array;
}

function rowKey(accountIdB64: string, kind: ReachRowKind): string {
  return `${accountIdB64}|${kind}`;
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
      if (!db.objectStoreNames.contains(STORE)) db.createObjectStore(STORE, { keyPath: "key" });
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

async function encrypt(key: CryptoKey, plaintext: Uint8Array): Promise<Uint8Array> {
  const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
  const ct = new Uint8Array(
    await crypto.subtle.encrypt({ name: "AES-GCM", iv: iv.slice() }, key, plaintext.slice()),
  );
  const out = new Uint8Array(IV_LEN + ct.length);
  out.set(iv, 0);
  out.set(ct, IV_LEN);
  return out;
}

async function decrypt(key: CryptoKey, blob: Uint8Array): Promise<Uint8Array> {
  if (blob.length < IV_LEN + 16) throw new Error("reach row is too short");
  return new Uint8Array(
    await crypto.subtle.decrypt({ name: "AES-GCM", iv: blob.slice(0, IV_LEN) }, key, blob.slice(IV_LEN)),
  );
}

/** Read one row's plaintext JSON; `null` when absent, unreadable, or no key. */
export async function loadReachRow(
  accountIdB64: string,
  kind: ReachRowKind,
): Promise<string | null> {
  let deviceKey: CryptoKey | null;
  try {
    deviceKey = await getDeviceKey(accountIdB64);
  } catch {
    return null;
  }
  if (deviceKey === null) return null;
  let db: IDBDatabase;
  try {
    db = await openDb();
  } catch {
    return null;
  }
  let row: PersistedRow | undefined;
  try {
    const tx = db.transaction(STORE, "readonly");
    const req = tx.objectStore(STORE).get(rowKey(accountIdB64, kind));
    row = await new Promise<PersistedRow | undefined>((resolve, reject) => {
      req.onsuccess = (): void => resolve(req.result as PersistedRow | undefined);
      req.onerror = (): void => reject(req.error ?? new Error("idb get failed"));
    });
    await awaitTx(tx);
  } catch {
    return null;
  } finally {
    db.close();
  }
  if (row === undefined) return null;
  try {
    return new TextDecoder().decode(await decrypt(deviceKey, row.ciphertext));
  } catch {
    return null;
  }
}

/** Write one row. Never throws: a failed write only costs durability. */
export async function saveReachRow(
  accountIdB64: string,
  kind: ReachRowKind,
  json: string,
): Promise<void> {
  try {
    const deviceKey = await getDeviceKey(accountIdB64);
    if (deviceKey === null) return;
    const ciphertext = await encrypt(deviceKey, new TextEncoder().encode(json));
    const db = await openDb();
    try {
      const tx = db.transaction(STORE, "readwrite");
      tx.objectStore(STORE).put({ key: rowKey(accountIdB64, kind), ciphertext } satisfies PersistedRow);
      await awaitTx(tx);
    } finally {
      db.close();
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn(`[reach] could not persist the ${kind} row`, err);
  }
}

/** Erase every row of an account (forget device). */
export async function deleteReachRows(accountIdB64: string): Promise<void> {
  try {
    const db = await openDb();
    try {
      const tx = db.transaction(STORE, "readwrite");
      const store = tx.objectStore(STORE);
      for (const kind of ["book", "grants", "visibility", "introductions", "publication"] as const) {
        store.delete(rowKey(accountIdB64, kind));
      }
      await awaitTx(tx);
    } finally {
      db.close();
    }
  } catch {
    /* nothing to erase */
  }
}
