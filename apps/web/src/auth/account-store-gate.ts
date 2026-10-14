// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The storage-level backstop behind Forget device: once an account's local
 * data is being wiped, nothing writes that account's data again until the
 * account lands on this browser anew.
 *
 * ## The defect this closes
 *
 * Nothing cancels a promise. Work that captured account A (a poll, a save
 * after an encrypt, a session landing) and is suspended on an `await` when
 * `forgetDevice` runs resumes afterwards and writes A's row back, after the
 * wipe (`wipe-account.ts`) deleted it: a forgotten account's messages, keys
 * or MLS state are on the device again. The binding checks in `AuthContext`
 * stop the auth flows; this module stops the write itself, whoever issues it.
 *
 * ## Why a check at transaction time is enough
 *
 * Every per-account IndexedDB store deletes ROWS (none deletes a database),
 * and IndexedDB runs read-write transactions with overlapping scope in the
 * order they were created, across connections and across tabs. So for one
 * store there are exactly two cases:
 *
 * - the write's transaction was created BEFORE the wipe's delete transaction:
 *   it commits first and the delete removes its row. Nothing to wait for;
 * - it was created AFTER: its row would survive the wipe. That is the case
 *   the gate refuses.
 *
 * Hence the rule for a store: ask the gate synchronously, immediately before
 * `db.transaction(..., "readwrite")`, with no `await` in between - after the
 * encrypt, after the database open. The wipe seals BEFORE its first delete,
 * so every transaction created after the seal is refused and every one
 * created before it is ordered ahead of the delete. "Waiting for the writes
 * in flight" is what the transaction order already does; an in-flight write
 * that has not reached its transaction yet is refused when it gets there.
 *
 * `localStorage` / `sessionStorage` writes are synchronous, so there the
 * check and the write are one step: {@link installAccountStorageGate} puts
 * the check in `Storage.prototype.setItem` for every key that names a sealed
 * account (every per-account key carries the account id). One place, so a
 * store added later is covered without remembering to ask.
 *
 * ## Other writers
 *
 * - Another tab: it shares the stores. The wipe records a digest of the
 *   account id in a short-lived `localStorage` marker, which a tab reads
 *   synchronously at check time and matches against the accounts it has
 *   handled; from then on that tab keeps the account sealed in memory until
 *   it lands the account itself. (A digest, not the id: Forget device leaves
 *   nothing on the device that names the account, not even for a minute.)
 * - The service workers (`public/sw*.js`): they open the push routing
 *   database read-only and write no per-account store.
 *
 * ## Scopes
 *
 * A wipe seals everything. A plain sign-out seals only the session-token
 * store: sign-out deletes nothing else, and a late write of the account's own
 * rows (an MLS snapshot that records a ratchet step already taken) belongs on
 * disk. Skipping it would be the harmful outcome. The one row a late write
 * must not bring back after a sign-out is the session token.
 *
 * ## Memory
 *
 * The gate guards stores, not memory. A cache that holds an account's rows
 * and saves them later would write the pre-wipe rows back once the account
 * lands again and the gate opens. Such caches register with
 * `account-memory.ts`; the seal for a wipe drops them, and so does the first
 * landing of an account that was sealed by a wipe (in-flight work may have
 * refilled them in between).
 *
 * The three durable MLS writes (the MLS snapshot, the commit outbox, the
 * DeviceGroup snapshot) use {@link requireAccountStoresWritable} and THROW:
 * their callers publish only after the persist succeeded, and a persist that
 * silently did nothing followed by a publish reuses a ratchet position at the
 * next sign-in. Every other store skips.
 */

import { dropAccountMemory } from "./account-memory";

/** A durable write was refused because its account is sealed. */
export class AccountStoresSealedError extends Error {
  override name = "AccountStoresSealedError";
  constructor() {
    super("the local stores of this account are sealed");
  }
}

/** The slice of `Storage` the cross-tab marker uses; injectable for tests. */
export interface GateStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

/** The cross-tab marker: digests of the accounts wiped in the last
 *  {@link WIPED_MARKER_TTL_MS}. Neither the key nor the value names one. */
export const WIPED_MARKER_KEY = "lakko.account-store-gate.wiped.v1";
/** How long a wipe stays in the marker: long enough for every open tab to
 *  have received its `storage` event (chosen, not measured). */
export const WIPED_MARKER_TTL_MS = 60_000;

interface WipedEntry {
  /** {@link idDigest} of one form of the account id. */
  readonly h: string;
  readonly at: number;
}

/**
 * A 64-bit FNV-1a digest of an account id, as hex. Not a secret-keeping
 * construction and it does not need to be one: an account id is 16 random
 * bytes, so the digest tells a reader of the marker nothing, and a tab that
 * holds the id recognises it. Synchronous, which the check has to be.
 */
function idDigest(id: string): string {
  let hash = 0xcbf29ce484222325n;
  for (let i = 0; i < id.length; i++) {
    hash ^= BigInt(id.charCodeAt(i));
    hash = (hash * 0x100000001b3n) & 0xffffffffffffffffn;
  }
  return hash.toString(16).padStart(16, "0");
}

/** Sealed in this realm for every store (wiped, not landed again since). */
const sealed = new Set<string>();
/** Sealed in this realm for the session-token store only (signed out). */
const sessionSealed = new Set<string>();
/** Every account id this realm has been asked about or has opened: what a
 *  marker digest from another tab is matched against. */
const known = new Set<string>();
/** Being wiped in this realm right now: {@link openAccountStores} leaves
 *  these sealed, so a landing that races the wipe cannot write between its
 *  deletes. */
const wiping = new Set<string>();

let markerStorage: GateStorage | null | undefined;
let installed = false;

function storageOrNull(): GateStorage | null {
  if (markerStorage !== undefined) return markerStorage;
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

function parseWiped(raw: string | null): WipedEntry[] {
  if (raw === null) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    const out: WipedEntry[] = [];
    for (const e of parsed) {
      if (typeof e !== "object" || e === null) continue;
      const { h, at } = e as { h?: unknown; at?: unknown };
      if (typeof h === "string" && h.length > 0 && typeof at === "number") out.push({ h, at });
    }
    return out;
  } catch {
    return [];
  }
}

function readWiped(): WipedEntry[] {
  try {
    return parseWiped(storageOrNull()?.getItem(WIPED_MARKER_KEY) ?? null);
  } catch {
    return [];
  }
}

function writeWiped(entries: readonly WipedEntry[]): void {
  const storage = storageOrNull();
  if (storage === null) return;
  try {
    if (entries.length === 0) {
      storage.removeItem(WIPED_MARKER_KEY);
      return;
    }
    // (The marker's own key names no account, so the installed `setItem`
    // gate lets it through.)
    storage.setItem(WIPED_MARKER_KEY, JSON.stringify(entries));
  } catch {
    // Storage blocked: the seal still holds in this realm.
  }
}

/** Seal, in this realm, every account it knows that `entries` names. */
function absorb(entries: readonly WipedEntry[]): void {
  if (entries.length === 0) return;
  const digests = new Set(entries.map((e) => e.h));
  for (const id of known) {
    if (!sealed.has(id) && digests.has(idDigest(id))) sealed.add(id);
  }
}

/** Take what the marker says into this realm's memory. */
function absorbMarker(): void {
  absorb(readWiped());
}

/**
 * Whether a write of `accountId`'s data may be issued NOW. Ask synchronously,
 * immediately before the transaction is created (see the module doc); `false`
 * means skip the write.
 */
export function accountStoresWritable(accountId: string): boolean {
  known.add(accountId);
  if (sealed.has(accountId)) return false;
  // A wipe in another tab whose `storage` event has not reached this realm.
  absorbMarker();
  return !sealed.has(accountId);
}

/** {@link accountStoresWritable} for the durable MLS writes: throws
 *  {@link AccountStoresSealedError} instead of answering `false`. */
export function requireAccountStoresWritable(accountId: string): void {
  if (!accountStoresWritable(accountId)) throw new AccountStoresSealedError();
}

/** The session-token store: also closed by a plain sign-out in this realm. */
export function accountSessionWritable(accountId: string): boolean {
  return !sessionSealed.has(accountId) && accountStoresWritable(accountId);
}

/**
 * Whether a `localStorage` / `sessionStorage` key may be written: `false`
 * when it names a sealed account. Every per-account key carries the account
 * id (standard base64, or base64url for the two stores filed that way).
 *
 * In a tab that did not run the wipe, the sealed accounts are the ones that
 * tab has handled (opened, or asked the gate about) - which is every account
 * it could be writing for.
 */
export function storageKeyWritable(key: string): boolean {
  absorbMarker();
  for (const id of sealed) {
    if (key.includes(id)) return false;
  }
  return true;
}

/** A plain sign-out: from now on a late write cannot bring `accountId`'s
 *  session token back. Nothing else is closed, nothing is deleted. */
export function sealAccountSession(accountId: string): void {
  sessionSealed.add(accountId);
}

/**
 * The wipe starts: seal every store of the account, here and (through the
 * marker) in every other tab. Call BEFORE the first delete. `ids` are all the
 * forms the account's rows are filed under.
 */
export function sealAccountStoresForWipe(ids: readonly string[], nowMs: number = Date.now()): void {
  const fresh = ids.filter((id) => id.length > 0);
  for (const id of fresh) {
    known.add(id);
    sealed.add(id);
    wiping.add(id);
  }
  const digests = fresh.map(idDigest);
  const kept = readWiped().filter(
    (e) => nowMs - e.at < WIPED_MARKER_TTL_MS && !digests.includes(e.h),
  );
  writeWiped([...kept, ...digests.map((h) => ({ h, at: nowMs }))]);
  // The in-memory caches of the account go with the seal, before the first
  // delete (`account-memory.ts`).
  dropAccountMemory(fresh, "wipe");
}

/** The wipe's deletes have all run. The account stays sealed until it lands
 *  again; from here a landing may open it. */
export function finishAccountWipe(ids: readonly string[]): void {
  for (const id of ids) wiping.delete(id);
}

/** Drop marker entries older than {@link WIPED_MARKER_TTL_MS}. The wiping tab
 *  calls this once the TTL has passed, and every page calls it at start, so
 *  a wiped account's id does not stay on the device in the marker. */
export function pruneWipedMarker(nowMs: number = Date.now()): void {
  const all = readWiped();
  const kept = all.filter((e) => nowMs - e.at < WIPED_MARKER_TTL_MS);
  if (kept.length !== all.length) writeWiped(kept);
}

/**
 * The account lands on this browser again (a sign-in, a sign-up, a restore
 * prompt, a page-load restore): its stores are open in this realm, and its
 * marker entry goes. Other tabs that learned of the wipe stay sealed until
 * they land the account themselves.
 *
 * An account whose wipe is still running in this realm stays sealed: a
 * landing that overlaps the wipe writes nothing, and the account opens at its
 * next landing.
 */
export function openAccountStores(ids: readonly string[]): void {
  const opened = ids.filter((id) => !wiping.has(id));
  // A wipe in another tab that this realm has not absorbed yet still counts:
  // what this realm holds in memory for the account predates it.
  for (const id of opened) known.add(id);
  absorbMarker();
  const reopened = opened.filter((id) => sealed.has(id));
  if (reopened.length > 0) {
    // Whatever in-flight work put back in memory since the wipe goes before
    // the landing's first write can save it (`account-memory.ts`).
    dropAccountMemory(opened, "wipe");
  }
  for (const id of opened) {
    known.add(id);
    sealed.delete(id);
    sessionSealed.delete(id);
  }
  const digests = opened.map(idDigest);
  const all = readWiped();
  const kept = all.filter((e) => !digests.includes(e.h));
  if (kept.length !== all.length) writeWiped(kept);
}

/**
 * Put the gate in front of `localStorage` / `sessionStorage` and listen for
 * wipes in other tabs. Idempotent; a no-op where there is no `Storage` (the
 * unit-test host, a worker). Returns the uninstall, for tests.
 */
export function installAccountStorageGate(): () => void {
  if (typeof Storage === "undefined" || installed) return () => undefined;
  const original = Storage.prototype.setItem;
  installed = true;
  Storage.prototype.setItem = function gatedSetItem(this: Storage, key: string, value: string): void {
    // A dropped write, like a store's own skip: the row belongs to an
    // account whose local data was wiped.
    if (!storageKeyWritable(String(key))) return;
    original.call(this, key, value);
  };
  const onStorage = (event: StorageEvent): void => {
    if (event.key !== WIPED_MARKER_KEY) return;
    absorb(parseWiped(event.newValue));
  };
  const target = typeof window === "undefined" ? null : window;
  target?.addEventListener("storage", onStorage);
  pruneWipedMarker();
  return () => {
    Storage.prototype.setItem = original;
    installed = false;
    target?.removeEventListener("storage", onStorage);
  };
}

/** Test seam: forget every seal and use `storage` for the marker
 *  (`undefined` restores the default). */
export function resetAccountStoreGateForTests(storage?: GateStorage | null): void {
  sealed.clear();
  sessionSealed.clear();
  wiping.clear();
  known.clear();
  markerStorage = storage;
}
