// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.5 / R.8 - the service worker's read-only copy of the keys an
 * accepted reset retired.
 *
 * "Nothing from a superseded sender acts" holds in the app through
 * `chat/background-events.ts`. The service worker is the third receiver: a
 * removed device that still holds the old keys can publish into the old
 * group, the Delivery Service wakes this browser, and the worker would ring
 * or post a preview for it. The worker cannot read the pin store (it is
 * `localStorage`, which a worker has no access to), so the app mirrors the
 * ONE thing the worker needs here: the set of retired account keys.
 *
 * ```text
 *   db   "lakko-superseded-keys"  v2
 *   └── store "rows"  keyPath: "accountId"
 *         row { accountId: <b64>, keys: string[] }   // standard base64, sorted
 * ```
 *
 * Plaintext, like the pin store it is copied from: public account keys of
 * people this account knows, nothing else. The worker only reads it
 * (`public/sw.js` `lookupSupersededKeys`, C-59). Rewritten whenever the pin
 * store changes; removed with the account's data (`auth/wipe-account.ts`).
 *
 * The version is 2, not 1: the worker opens databases without a version, so
 * a wake that arrives before the app first wrote this one creates it EMPTY
 * at version 1. Opening at 2 from here always runs the upgrade that makes
 * the store.
 */

import { registerAccountMemory } from "../auth/account-memory";
import { accountStoresWritable } from "../auth/account-store-gate";

export const SUPERSEDED_MIRROR_DB = "lakko-superseded-keys";
const DB_VERSION = 2;
export const SUPERSEDED_MIRROR_STORE = "rows";

/** The mirror row, as the worker reads it. */
export interface SupersededMirrorRow {
  readonly accountId: string;
  readonly keys: readonly string[];
}

/**
 * The row for `accountIdB64`: every retired key once, sorted, so the same
 * set always writes the same row. Pure.
 */
export function supersededMirrorRow(
  accountIdB64: string,
  retiredKeysB64: Iterable<string>,
): SupersededMirrorRow {
  const keys = [...new Set([...retiredKeysB64].filter((k) => typeof k === "string" && k.length > 0))];
  keys.sort();
  return { accountId: accountIdB64, keys };
}

/** Whether two rows hold the same set (a write can be skipped). Pure. */
export function sameMirrorRow(a: SupersededMirrorRow | null, b: SupersededMirrorRow): boolean {
  if (a === null || a.accountId !== b.accountId || a.keys.length !== b.keys.length) return false;
  return a.keys.every((k, i) => k === b.keys[i]);
}

function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === "undefined") {
      reject(new Error("IndexedDB unavailable in this environment"));
      return;
    }
    const req = indexedDB.open(SUPERSEDED_MIRROR_DB, DB_VERSION);
    req.onupgradeneeded = (): void => {
      const db = req.result;
      if (!db.objectStoreNames.contains(SUPERSEDED_MIRROR_STORE)) {
        db.createObjectStore(SUPERSEDED_MIRROR_STORE, { keyPath: "accountId" });
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

/** What this session last wrote, so an unchanged set costs no transaction. */
let lastWritten: SupersededMirrorRow | null = null;

// After Forget device the row is gone; a remembered "already written" would
// skip the same set's write at the account's next sign-in on this page and
// leave the service worker without it.
registerAccountMemory(
  "superseded-mirror",
  (ids) => {
    if (lastWritten !== null && ids.includes(lastWritten.accountId)) lastWritten = null;
  },
  { signOut: true },
);

/**
 * Write the mirror for `accountIdB64`. Best effort: a failure leaves the
 * worker with the previous set (or none), which errs toward a notification.
 */
export async function writeSupersededMirror(
  accountIdB64: string,
  retiredKeysB64: Iterable<string>,
): Promise<void> {
  const row = supersededMirrorRow(accountIdB64, retiredKeysB64);
  if (sameMirrorRow(lastWritten, row)) return;
  const db = await openDb();
  try {
    // A wiped account takes no late write (`auth/account-store-gate.ts`).
    if (!accountStoresWritable(accountIdB64)) return;
    const tx = db.transaction(SUPERSEDED_MIRROR_STORE, "readwrite");
    tx.objectStore(SUPERSEDED_MIRROR_STORE).put(row);
    await awaitTx(tx);
    lastWritten = row;
  } finally {
    db.close();
  }
}

/** Forget the mirror of the account (its data is wiped). */
export async function clearSupersededMirrorForAccount(accountIdB64: string): Promise<void> {
  if (lastWritten?.accountId === accountIdB64) lastWritten = null;
  if (typeof indexedDB === "undefined") return;
  const db = await openDb();
  try {
    const tx = db.transaction(SUPERSEDED_MIRROR_STORE, "readwrite");
    tx.objectStore(SUPERSEDED_MIRROR_STORE).delete(accountIdB64);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}
