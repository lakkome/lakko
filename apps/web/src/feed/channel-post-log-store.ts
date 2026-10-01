// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * IndexedDB-backed encrypted store for the per-account channel-post log
 * (ADR-0061 section 3). Same shape as `channels/channel-log-store.ts`: one
 * row per account, the JSON payload (`channel-post-log.ts`) sealed with
 * AES-GCM under the device root key.
 *
 * ```text
 *   db   "lakko-channel-post-log"  v1
 *   └── store "logs"  keyPath: "accountId"
 *         row { accountId, ciphertext: [12-byte IV | AES-GCM ct+tag] }
 * ```
 *
 * Writes are serialised by the one runner (`ChannelPostSync.tsx`); a
 * change is announced through `subscribeChannelPostLogChanged`, in process
 * and across tabs, so the feed reloads.
 */

import { decodeChannelPostLog, type ChannelPostLog } from "./channel-post-log";

const DB_NAME = "lakko-channel-post-log";
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

function awaitTx(tx: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = (): void => resolve();
    tx.onabort = (): void => reject(tx.error ?? new Error("idb tx aborted"));
    tx.onerror = (): void => reject(tx.error ?? new Error("idb tx errored"));
  });
}

/** Persist (or replace) the log for an account. */
export async function saveChannelPostLog(
  accountIdB64: string,
  deviceKey: CryptoKey,
  log: ChannelPostLog,
): Promise<void> {
  const plaintext = new TextEncoder().encode(JSON.stringify(log));
  const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
  const ct = new Uint8Array(
    await crypto.subtle.encrypt({ name: "AES-GCM", iv: iv.slice() }, deviceKey, plaintext),
  );
  const ciphertext = new Uint8Array(IV_LEN + ct.length);
  ciphertext.set(iv, 0);
  ciphertext.set(ct, IV_LEN);
  const row: PersistedRow = { accountId: accountIdB64, ciphertext };
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).put(row);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

/** Read the log for an account; `null` when absent, undecryptable or of an
 *  unknown version (the caller starts empty). */
export async function loadChannelPostLog(
  accountIdB64: string,
  deviceKey: CryptoKey,
): Promise<ChannelPostLog | null> {
  const db = await openDb();
  let row: PersistedRow | undefined;
  try {
    const tx = db.transaction(STORE, "readonly");
    const req = tx.objectStore(STORE).get(accountIdB64);
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
    const plaintext = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: row.ciphertext.slice(0, IV_LEN) },
      deviceKey,
      row.ciphertext.slice(IV_LEN),
    );
    return decodeChannelPostLog(JSON.parse(new TextDecoder().decode(plaintext)));
  } catch {
    return null;
  }
}

/** Erase the log for an account. Called from `forgetDevice`. */
export async function deleteChannelPostLog(accountIdB64: string): Promise<void> {
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).delete(accountIdB64);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

// ----- Change signal ------------------------------------------------------

const CHANGE_CHANNEL = "lakko/channel-post-log/v1";
const handlers = new Set<(accountIdB64: string) => void>();
let broadcast: BroadcastChannel | null = null;

function ensureBroadcast(): BroadcastChannel | null {
  if (typeof BroadcastChannel === "undefined") return null;
  if (broadcast === null) {
    broadcast = new BroadcastChannel(CHANGE_CHANNEL);
    broadcast.onmessage = (event: MessageEvent): void => {
      const account = (event.data as { accountIdB64?: unknown } | null)?.accountIdB64;
      if (typeof account === "string") for (const h of handlers) h(account);
    };
  }
  return broadcast;
}

/** Announce that an account's post log changed. */
export function publishChannelPostLogChanged(accountIdB64: string): void {
  for (const h of handlers) h(accountIdB64);
  ensureBroadcast()?.postMessage({ accountIdB64 });
}

/** Listen for changes; returns the unsubscribe. */
export function subscribeChannelPostLogChanged(handler: (accountIdB64: string) => void): () => void {
  ensureBroadcast();
  handlers.add(handler);
  return () => handlers.delete(handler);
}
