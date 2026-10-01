// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The person's own feeds on this device (ADR-0061 section 6): each a rule in
 * the core's canonical encoding, sealed per account under the device root
 * key in IndexedDB. They do not follow the person to other devices.
 *
 * ```text
 *   db   "lakko-feed-state"  v1
 *   └── store "state"  keyPath: "accountId"
 *         row { accountId, ciphertext: [12-byte IV | AES-GCM ct+tag] }
 *   plaintext  { version: 1, feeds: [{ id, rule: <base64url rule bytes> }] }
 * ```
 *
 * The codec (`decodeFeedState`) is pure and lenient per entry: a feed whose
 * id or bytes are malformed is dropped alone. Whether the bytes are a valid
 * rule is the core's to say when the feed is opened.
 */

import { base64UrlDecodeNoPad, base64UrlEncodeNoPad } from "../auth/account-id";

export const FEED_STATE_VERSION = 1;
/** Most feeds kept (chosen): a switcher row, not a library. */
export const MAX_SAVED_FEEDS = 20;

export interface SavedFeed {
  /** Local id, random, stable across edits. */
  readonly id: string;
  /** The rule's canonical bytes. */
  readonly rule: Uint8Array;
}

export interface FeedState {
  readonly feeds: readonly SavedFeed[];
}

const ID_RE = /^[A-Za-z0-9_-]{8,32}$/;

export function decodeFeedState(raw: unknown): FeedState {
  if (typeof raw !== "object" || raw === null) return { feeds: [] };
  const r = raw as Record<string, unknown>;
  if (r.version !== FEED_STATE_VERSION || !Array.isArray(r.feeds)) return { feeds: [] };
  const feeds: SavedFeed[] = [];
  for (const f of r.feeds) {
    if (typeof f !== "object" || f === null) continue;
    const { id, rule } = f as Record<string, unknown>;
    if (typeof id !== "string" || !ID_RE.test(id) || typeof rule !== "string") continue;
    if (feeds.some((x) => x.id === id)) continue;
    try {
      feeds.push({ id, rule: base64UrlDecodeNoPad(rule) });
    } catch {
      // A malformed entry is dropped alone.
    }
    if (feeds.length >= MAX_SAVED_FEEDS) break;
  }
  return { feeds };
}

export function encodeFeedState(state: FeedState): unknown {
  return {
    version: FEED_STATE_VERSION,
    feeds: state.feeds.map((f) => ({ id: f.id, rule: base64UrlEncodeNoPad(f.rule) })),
  };
}

/** A fresh local feed id. */
export function newFeedId(): string {
  return base64UrlEncodeNoPad(crypto.getRandomValues(new Uint8Array(12)));
}

/** Add or replace a feed; a new one goes to the end. */
export function upsertFeed(state: FeedState, feed: SavedFeed): FeedState {
  const i = state.feeds.findIndex((f) => f.id === feed.id);
  if (i >= 0) {
    const feeds = [...state.feeds];
    feeds[i] = feed;
    return { feeds };
  }
  return { feeds: [...state.feeds, feed].slice(0, MAX_SAVED_FEEDS) };
}

export function removeFeed(state: FeedState, id: string): FeedState {
  return { feeds: state.feeds.filter((f) => f.id !== id) };
}

// ----- IndexedDB ------------------------------------------------------------

const DB_NAME = "lakko-feed-state";
const STORE = "state";
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

export async function loadFeedState(accountIdB64: string, deviceKey: CryptoKey): Promise<FeedState> {
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
  if (row === undefined || row.ciphertext.length < IV_LEN + 16) return { feeds: [] };
  try {
    const plain = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: row.ciphertext.slice(0, IV_LEN) },
      deviceKey,
      row.ciphertext.slice(IV_LEN),
    );
    return decodeFeedState(JSON.parse(new TextDecoder().decode(plain)));
  } catch {
    return { feeds: [] };
  }
}

export async function saveFeedState(
  accountIdB64: string,
  deviceKey: CryptoKey,
  state: FeedState,
): Promise<void> {
  const plain = new TextEncoder().encode(JSON.stringify(encodeFeedState(state)));
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

export async function deleteFeedState(accountIdB64: string): Promise<void> {
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).delete(accountIdB64);
    await done(tx);
  } finally {
    db.close();
  }
}
