// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Discover's state on this device (ADR-0059, docs/ux/feed.md section 6),
 * sealed per account under the device root key like the feed's own state.
 * Nothing here leaves the device.
 *
 * ```text
 *   db   "lakko-discover"  v1
 *   └── store "rows"  keyPath: "key"      key = "<accountId>|prefs" or "<accountId>|shared"
 *         row { key, ciphertext: [12-byte IV | AES-GCM ct+tag] }
 * ```
 *
 * `prefs` holds the picked topics and language, the "Mix in random topics"
 * switch, the not-interested channels and hidden topics, the month's decoy
 * draw and the cache of fetched buckets. `shared` holds the group-shared
 * lists (`shared-channels.ts`). Both are wiped by `forgetDevice`.
 */

import type { DeviceCipher } from "../auth/device-cipher";
import { accountStoresWritable } from "../auth/account-store-gate";
import type { DirectoryItemWire } from "./cards";
import type { DecoyDraw } from "./decoys";

export const DISCOVER_PREFS_VERSION = 1;
/** Most not-interested channels kept (chosen). */
export const MAX_NOT_INTERESTED = 500;

/** One cached bucket (only picked buckets are kept; decoys are discarded). */
export interface CachedBucket {
  readonly fetchedAtMs: number;
  readonly items: readonly DirectoryItemWire[];
}

export interface DiscoverPrefs {
  readonly topics: readonly number[];
  /** `null` = not chosen yet; the app language is proposed. */
  readonly language: string | null;
  readonly mixRandom: boolean;
  readonly notInterested: readonly string[];
  readonly hiddenTopics: readonly number[];
  readonly decoys: DecoyDraw | null;
  readonly lastRefreshMs: number | null;
  /** `pickedKey` of the last refresh. */
  readonly lastFetchedKey: string | null;
  /** `<topic>:<language>` -> bucket. */
  readonly buckets: Readonly<Record<string, CachedBucket>>;
  /** The pick-topics card was answered with "Not now". */
  readonly introDismissed: boolean;
}

/** What one refresh of the directory produced: the fields a refresh owns. */
export interface RefreshOutcome {
  readonly decoys: DecoyDraw | null;
  /** Absent when every request failed: the cached buckets stay. */
  readonly fetched?: {
    readonly buckets: Readonly<Record<string, CachedBucket>>;
    readonly lastRefreshMs: number;
    readonly lastFetchedKey: string;
  };
}

/**
 * Write a refresh's outcome into the preferences as they are NOW.
 *
 * A refresh takes seconds, and the person keeps using the screen meanwhile:
 * they pick topics, hide a channel, answer the intro card. Each of those is
 * saved at once. Persisting the refresh as "the preferences from when it
 * started, plus the new buckets" wrote every one of them back out. So the
 * refresh contributes only the fields it owns (the decoy draw, the buckets
 * and the two refresh stamps), and everything else comes from `current`.
 */
export function withRefreshOutcome(current: DiscoverPrefs, outcome: RefreshOutcome): DiscoverPrefs {
  if (outcome.fetched === undefined) return { ...current, decoys: outcome.decoys };
  return {
    ...current,
    decoys: outcome.decoys,
    buckets: outcome.fetched.buckets,
    lastRefreshMs: outcome.fetched.lastRefreshMs,
    lastFetchedKey: outcome.fetched.lastFetchedKey,
  };
}

export function defaultDiscoverPrefs(): DiscoverPrefs {
  return {
    topics: [],
    language: null,
    mixRandom: true,
    notInterested: [],
    hiddenTopics: [],
    decoys: null,
    lastRefreshMs: null,
    lastFetchedKey: null,
    buckets: {},
    introDismissed: false,
  };
}

const TOPIC_OK = (t: unknown): t is number => typeof t === "number" && Number.isInteger(t) && t >= 0 && t <= 255;
const LANG_RE = /^[a-z]{2,3}$/;
const HEX_ID = /^[0-9a-f]{32}$/;

function decodeItems(raw: unknown): DirectoryItemWire[] {
  if (!Array.isArray(raw)) return [];
  const out: DirectoryItemWire[] = [];
  for (const i of raw) {
    if (typeof i !== "object" || i === null) continue;
    const r = i as Record<string, unknown>;
    if (typeof r.channel_id !== "string" || !HEX_ID.test(r.channel_id)) continue;
    if (typeof r.listing_b64 !== "string" || typeof r.subscribers !== "string") continue;
    out.push({ channel_id: r.channel_id, listing_b64: r.listing_b64, subscribers: r.subscribers });
    if (out.length >= 100) break;
  }
  return out;
}

/** Lenient per field: a malformed field falls back to its default alone. */
export function decodeDiscoverPrefs(raw: unknown): DiscoverPrefs {
  const d = defaultDiscoverPrefs();
  if (typeof raw !== "object" || raw === null) return d;
  const r = raw as Record<string, unknown>;
  if (r.version !== DISCOVER_PREFS_VERSION) return d;
  const topics = Array.isArray(r.topics) ? [...new Set(r.topics.filter(TOPIC_OK))] : d.topics;
  const language = typeof r.language === "string" && LANG_RE.test(r.language) ? r.language : null;
  const notInterested = Array.isArray(r.notInterested)
    ? r.notInterested.filter((x): x is string => typeof x === "string" && HEX_ID.test(x)).slice(-MAX_NOT_INTERESTED)
    : [];
  const hiddenTopics = Array.isArray(r.hiddenTopics) ? [...new Set(r.hiddenTopics.filter(TOPIC_OK))] : [];
  let decoys: DecoyDraw | null = null;
  if (typeof r.decoys === "object" && r.decoys !== null) {
    const x = r.decoys as Record<string, unknown>;
    if (
      typeof x.month === "string" &&
      /^\d{4}-\d{2}$/.test(x.month) &&
      typeof x.language === "string" &&
      Array.isArray(x.order) &&
      x.order.every(TOPIC_OK)
    ) {
      decoys = { month: x.month, language: x.language, order: x.order as number[] };
    }
  }
  const buckets: Record<string, CachedBucket> = {};
  if (typeof r.buckets === "object" && r.buckets !== null) {
    for (const [k, v] of Object.entries(r.buckets as Record<string, unknown>)) {
      if (!/^\d{1,3}:[a-z]{2,3}$/.test(k) || typeof v !== "object" || v === null) continue;
      const b = v as Record<string, unknown>;
      if (typeof b.fetchedAtMs !== "number") continue;
      buckets[k] = { fetchedAtMs: b.fetchedAtMs, items: decodeItems(b.items) };
    }
  }
  return {
    topics,
    language,
    mixRandom: typeof r.mixRandom === "boolean" ? r.mixRandom : true,
    notInterested,
    hiddenTopics,
    decoys,
    lastRefreshMs: typeof r.lastRefreshMs === "number" ? r.lastRefreshMs : null,
    lastFetchedKey: typeof r.lastFetchedKey === "string" ? r.lastFetchedKey : null,
    buckets,
    introDismissed: r.introDismissed === true,
  };
}

export function encodeDiscoverPrefs(p: DiscoverPrefs): unknown {
  return { version: DISCOVER_PREFS_VERSION, ...p };
}

/** The bucket cache key. */
export function bucketKey(topic: number, language: string): string {
  return `${topic}:${language}`;
}

// ----- Sealed IndexedDB rows ---------------------------------------------------

const DB_NAME = "lakko-discover";
const STORE = "rows";
const IV_LEN = 12;

export type DiscoverRowKind = "prefs" | "shared";

function rowKey(accountId: string, kind: DiscoverRowKind): string {
  return `${accountId}|${kind}`;
}

function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === "undefined") {
      reject(new Error("IndexedDB unavailable in this environment"));
      return;
    }
    const req = indexedDB.open(DB_NAME, 1);
    req.onupgradeneeded = (): void => {
      if (!req.result.objectStoreNames.contains(STORE)) {
        req.result.createObjectStore(STORE, { keyPath: "key" });
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

/** The row's plaintext, or `null` when absent or not openable with this key. */
export async function loadDiscoverRow(
  accountId: string,
  kind: DiscoverRowKind,
  deviceKey: DeviceCipher,
): Promise<string | null> {
  const db = await openDb();
  let row: { ciphertext: Uint8Array } | undefined;
  try {
    const tx = db.transaction(STORE, "readonly");
    const req = tx.objectStore(STORE).get(rowKey(accountId, kind));
    row = await new Promise((resolve, reject) => {
      req.onsuccess = (): void => resolve(req.result as { ciphertext: Uint8Array } | undefined);
      req.onerror = (): void => reject(req.error ?? new Error("idb get failed"));
    });
    await done(tx);
  } finally {
    db.close();
  }
  if (row === undefined || row.ciphertext.length < IV_LEN + 16) return null;
  try {
    const plain = await deviceKey.decrypt(row.ciphertext.slice(0, IV_LEN), row.ciphertext.slice(IV_LEN));
    return new TextDecoder().decode(plain);
  } catch {
    return null;
  }
}

export async function saveDiscoverRow(
  accountId: string,
  kind: DiscoverRowKind,
  deviceKey: DeviceCipher,
  plaintext: string,
): Promise<void> {
  const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
  const ct = new Uint8Array(
    await deviceKey.encrypt(iv, new TextEncoder().encode(plaintext)),
  );
  const ciphertext = new Uint8Array(IV_LEN + ct.length);
  ciphertext.set(iv, 0);
  ciphertext.set(ct, IV_LEN);
  const db = await openDb();
  try {
    if (!accountStoresWritable(accountId)) return;
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).put({ key: rowKey(accountId, kind), ciphertext });
    await done(tx);
  } finally {
    db.close();
  }
}

/** Erase both rows of an account (forget device). */
export async function deleteDiscoverRows(accountId: string): Promise<void> {
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).delete(rowKey(accountId, "prefs"));
    tx.objectStore(STORE).delete(rowKey(accountId, "shared"));
    await done(tx);
  } finally {
    db.close();
  }
}

export async function loadDiscoverPrefs(accountId: string, deviceKey: DeviceCipher): Promise<DiscoverPrefs> {
  const json = await loadDiscoverRow(accountId, "prefs", deviceKey);
  if (json === null) return defaultDiscoverPrefs();
  try {
    return decodeDiscoverPrefs(JSON.parse(json));
  } catch {
    return defaultDiscoverPrefs();
  }
}

export async function saveDiscoverPrefs(
  accountId: string,
  deviceKey: DeviceCipher,
  prefs: DiscoverPrefs,
): Promise<void> {
  await saveDiscoverRow(accountId, "prefs", deviceKey, JSON.stringify(encodeDiscoverPrefs(prefs)));
}
