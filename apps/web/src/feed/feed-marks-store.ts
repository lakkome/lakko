// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The sealed store for the feed marks (`feed-marks.ts`): one row per
 * account, the JSON payload sealed with AES-GCM under the device root key,
 * the same shape as `feed-store.ts` beside it.
 *
 * ```text
 *   db   "lakko-feed-marks"  v1
 *   └── store "marks"  keyPath: "accountId"
 *         row { accountId, ciphertext: [12-byte IV | AES-GCM ct+tag] }
 * ```
 *
 * Three writers share it (the feed, the channel view, the hide toast's
 * Undo), so every write is a read-modify-write chained per account in this
 * page (`updateFeedMarks`). A change is announced in process and across
 * tabs so the lists showing a channel's count reload.
 */

import { getDeviceKey } from "../auth/key-store";
import {
  advanceChannelRead,
  decodeFeedMarks,
  emptyFeedMarks,
  encodeFeedMarks,
  type FeedMarks,
} from "./feed-marks";

const DB_NAME = "lakko-feed-marks";
const STORE = "marks";
const IV_LEN = 12;

function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === "undefined") {
      reject(new Error("IndexedDB unavailable in this environment"));
      return;
    }
    const req = indexedDB.open(DB_NAME, 1);
    req.onupgradeneeded = (): void => {
      if (!req.result.objectStoreNames.contains(STORE)) {
        req.result.createObjectStore(STORE, { keyPath: "accountId" });
      }
    };
    req.onsuccess = (): void => resolve(req.result);
    req.onerror = (): void => reject(req.error ?? new Error("indexedDB.open failed"));
  });
}

function done(tx: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = (): void => resolve();
    tx.onabort = (): void => reject(tx.error ?? new Error("idb tx aborted"));
    tx.onerror = (): void => reject(tx.error ?? new Error("idb tx errored"));
  });
}

export async function loadFeedMarks(accountIdB64: string, deviceKey: CryptoKey): Promise<FeedMarks> {
  const db = await openDb();
  let row: { ciphertext: Uint8Array } | undefined;
  try {
    const tx = db.transaction(STORE, "readonly");
    const req = tx.objectStore(STORE).get(accountIdB64);
    row = await new Promise((resolve, reject) => {
      req.onsuccess = (): void => resolve(req.result as { ciphertext: Uint8Array } | undefined);
      req.onerror = (): void => reject(req.error ?? new Error("idb get failed"));
    });
    await done(tx);
  } finally {
    db.close();
  }
  if (row === undefined || row.ciphertext.length < IV_LEN + 16) return emptyFeedMarks();
  try {
    const plain = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: row.ciphertext.slice(0, IV_LEN) },
      deviceKey,
      row.ciphertext.slice(IV_LEN),
    );
    return decodeFeedMarks(JSON.parse(new TextDecoder().decode(plain)));
  } catch {
    return emptyFeedMarks();
  }
}

async function saveFeedMarks(accountIdB64: string, deviceKey: CryptoKey, marks: FeedMarks): Promise<void> {
  const plain = new TextEncoder().encode(JSON.stringify(encodeFeedMarks(marks)));
  const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
  const ct = new Uint8Array(
    await crypto.subtle.encrypt({ name: "AES-GCM", iv: iv.slice() }, deviceKey, plain),
  );
  const ciphertext = new Uint8Array(IV_LEN + ct.length);
  ciphertext.set(iv, 0);
  ciphertext.set(ct, IV_LEN);
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).put({ accountId: accountIdB64, ciphertext });
    await done(tx);
  } finally {
    db.close();
  }
}

const writeChains = new Map<string, Promise<void>>();

/**
 * Read, change, write, in order with every other update for the account in
 * this page. `mutate` returning the same object writes nothing. Resolves to
 * the marks now stored.
 */
export function updateFeedMarks(
  accountIdB64: string,
  mutate: (current: FeedMarks) => FeedMarks,
): Promise<FeedMarks> {
  const prev = writeChains.get(accountIdB64) ?? Promise.resolve();
  const run = prev.then(async () => {
    const deviceKey = await getDeviceKey(accountIdB64);
    if (deviceKey === null) throw new Error("This device can't save this right now.");
    const current = await loadFeedMarks(accountIdB64, deviceKey);
    const next = mutate(current);
    if (next === current) return current;
    await saveFeedMarks(accountIdB64, deviceKey, next);
    publishFeedMarksChanged(accountIdB64);
    return next;
  });
  const tail = run.then(
    () => undefined,
    () => undefined,
  );
  writeChains.set(accountIdB64, tail);
  void tail.then(() => {
    if (writeChains.get(accountIdB64) === tail) writeChains.delete(accountIdB64);
  });
  return run;
}

/** Read the marks for an account, empty when there are none or no key. */
export async function readFeedMarks(accountIdB64: string): Promise<FeedMarks> {
  const deviceKey = await getDeviceKey(accountIdB64);
  if (deviceKey === null) return emptyFeedMarks();
  return loadFeedMarks(accountIdB64, deviceKey);
}

/** A channel post was read (in the feed or the channel): move the channel's
 *  newest-read forward to it. Never backwards. */
export function markChannelRead(accountIdB64: string, channelIdHex: string, cursor: number): Promise<FeedMarks> {
  return updateFeedMarks(accountIdB64, (m) => advanceChannelRead(m, channelIdHex, cursor));
}

/** Erase an account's marks. Called from `forgetDevice`. */
export async function deleteFeedMarks(accountIdB64: string): Promise<void> {
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).delete(accountIdB64);
    await done(tx);
  } finally {
    db.close();
  }
  publishFeedMarksChanged(accountIdB64);
}

// ----- Change signal ------------------------------------------------------

const CHANGE_CHANNEL = "lakko/feed-marks/v1";
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

export function publishFeedMarksChanged(accountIdB64: string): void {
  for (const h of handlers) h(accountIdB64);
  ensureBroadcast()?.postMessage({ accountIdB64 });
}

export function subscribeFeedMarksChanged(handler: (accountIdB64: string) => void): () => void {
  ensureBroadcast();
  handlers.add(handler);
  return () => handlers.delete(handler);
}
