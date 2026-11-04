// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0079 section 5 + the pairing state bundle v3 — what this account wrote
 * to a pending contact on ANOTHER of its devices, shown here as DISPLAY-ONLY
 * copies.
 *
 * The device where a message was typed keeps it in its own held-send queue
 * and sends it once the inviter's app answers. A device paired in the
 * meantime receives the queue as copies in the state bundle so the person
 * sees on it what they already wrote (one account, one conversation). A copy
 * is NEVER put into this device's held-send queue and never sent from here:
 * two senders would deliver every message twice.
 *
 * Where a copy lives:
 *
 *   * under the pending contact's account id (32 hex characters, the same
 *     key the held-send queue uses, `pendingQueueKey`) while the contact is
 *     pending — the pending chat shows it as an own row in the waiting state;
 *   * under the one-to-one's id (hex) once that conversation lands here
 *     ({@link moveCarriedCopies}, from the pending hand-over) — the chat
 *     shows it waiting at the end of the timeline.
 *
 * A copy goes when the real message arrives from this account's other
 * device in that one-to-one with the same text and the same number of
 * attachments ({@link matchArrivals}; the first copy in order, each arrival
 * at most once, remembered by message id so a reload cannot match the same
 * arrival twice), or {@link CARRIED_COPY_TTL_MS} after it was held.
 *
 * One row per account, AES-256-GCM under the device key, every write behind
 * the account store gate (ADR-0077), dropped with the account (Forget
 * device, Delete account) and from memory at sign-out.
 *
 * ```text
 *   db   "lakko-carried-copies"  v1
 *   └── store "rows"  keyPath: "accountId"
 *         row { accountId, ciphertext: Uint8Array  // [12-byte IV | ct+tag] }
 *         plaintext: JSON { v: 1, slots: { [key]: CarriedSlot } }
 * ```
 */
import { createSignal, type Accessor } from "solid-js";

import { registerAccountMemory } from "../auth/account-memory";
import { accountStoresWritable } from "../auth/account-store-gate";
import type { DeviceCipher } from "../auth/device-cipher";
import { getDeviceKey } from "../auth/key-store";

/** A copy unmatched this long after it was held is dropped. */
export const CARRIED_COPY_TTL_MS = 7 * 24 * 60 * 60 * 1000;
/** How many matched arrival ids one slot remembers. Chosen, not measured. */
const MAX_MATCHED_IDS = 500;

/** One display-only copy of a message held on another device. */
export interface CarriedCopy {
  readonly text: string;
  /** The sending client's own JSON form of the attachment references. Only
   *  their number is read (the match); nothing here fetches them. */
  readonly attachments: readonly unknown[];
  readonly replyTo: number | null;
  readonly heldAtMs: number;
}

/** The copies of one pending contact or one conversation. */
export interface CarriedSlot {
  readonly copies: readonly CarriedCopy[];
  /** DS message ids of arrivals that already replaced a copy. */
  readonly matched: readonly number[];
}

/** An own message from another device, as the match reads it. */
export interface OwnArrival {
  readonly messageId: number;
  readonly text: string;
  readonly attachmentCount: number;
}

/** The copies still within their lifetime. Pure. */
export function liveCopies(copies: readonly CarriedCopy[], nowMs: number): CarriedCopy[] {
  return copies.filter((c) => nowMs - c.heldAtMs < CARRIED_COPY_TTL_MS);
}

/**
 * Replace copies by the real messages: each arrival not seen before removes
 * the FIRST copy (in order) with the same text and the same number of
 * attachments, and is remembered. Expired copies go too. Returns the new
 * slot, or `null` when nothing changed. Pure.
 */
export function matchArrivals(
  slot: CarriedSlot,
  arrivals: readonly OwnArrival[],
  nowMs: number,
): CarriedSlot | null {
  let copies = liveCopies(slot.copies, nowMs);
  let changed = copies.length !== slot.copies.length;
  const matched = [...slot.matched];
  const seen = new Set(matched);
  for (const a of arrivals) {
    if (seen.has(a.messageId)) continue;
    const at = copies.findIndex((c) => c.text === a.text && c.attachments.length === a.attachmentCount);
    if (at === -1) continue;
    copies = copies.filter((_, i) => i !== at);
    matched.push(a.messageId);
    seen.add(a.messageId);
    changed = true;
  }
  if (!changed) return null;
  return { copies, matched: matched.slice(Math.max(0, matched.length - MAX_MATCHED_IDS)) };
}

/** `slots` with the copies of `from` appended to those of `to` (in order),
 *  and `from` gone. Pure. */
export function movedSlots(
  slots: Readonly<Record<string, CarriedSlot>>,
  from: string,
  to: string,
): Record<string, CarriedSlot> | null {
  const source = slots[from];
  if (source === undefined || from === to) return null;
  const target = slots[to] ?? { copies: [], matched: [] };
  const out: Record<string, CarriedSlot> = { ...slots };
  delete out[from];
  out[to] = { copies: [...target.copies, ...source.copies], matched: target.matched };
  return out;
}

function isCopy(v: unknown): v is CarriedCopy {
  if (typeof v !== "object" || v === null) return false;
  const c = v as Record<string, unknown>;
  return (
    typeof c["text"] === "string" &&
    Array.isArray(c["attachments"]) &&
    (c["replyTo"] === null || typeof c["replyTo"] === "number") &&
    typeof c["heldAtMs"] === "number" &&
    Number.isFinite(c["heldAtMs"])
  );
}

/** Parse the sealed plaintext; anything else reads as none. Pure. */
export function decodeCarriedCopies(json: string): Record<string, CarriedSlot> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return {};
  }
  const p = parsed as { v?: unknown; slots?: unknown } | null;
  if (p?.v !== 1 || typeof p.slots !== "object" || p.slots === null) return {};
  const out: Record<string, CarriedSlot> = {};
  for (const [key, raw] of Object.entries(p.slots as Record<string, unknown>)) {
    if (!/^[0-9a-f]+$/.test(key) || typeof raw !== "object" || raw === null) continue;
    const r = raw as { copies?: unknown; matched?: unknown };
    const copies = Array.isArray(r.copies) ? (r.copies as unknown[]).filter(isCopy) : [];
    const matched = Array.isArray(r.matched)
      ? (r.matched as unknown[]).filter((m): m is number => typeof m === "number" && Number.isSafeInteger(m))
      : [];
    out[key] = { copies, matched };
  }
  return out;
}

export function encodeCarriedCopies(slots: Readonly<Record<string, CarriedSlot>>): string {
  return JSON.stringify({ v: 1, slots });
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

const DB_NAME = "lakko-carried-copies";
const DB_VERSION = 1;
const STORE = "rows";
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
      if (!db.objectStoreNames.contains(STORE)) db.createObjectStore(STORE, { keyPath: "accountId" });
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

async function deviceKeyOf(accountIdB64: string): Promise<DeviceCipher | null> {
  try {
    return await getDeviceKey(accountIdB64);
  } catch {
    return null;
  }
}

async function read(accountIdB64: string, key: DeviceCipher): Promise<Record<string, CarriedSlot>> {
  const db = await openDb();
  let row: PersistedRow | undefined;
  try {
    const tx = db.transaction(STORE, "readonly");
    const req = tx.objectStore(STORE).get(accountIdB64);
    row = await new Promise<PersistedRow | undefined>((resolve, reject) => {
      req.onsuccess = (): void => resolve(req.result as PersistedRow | undefined);
      req.onerror = (): void => reject(req.error ?? new Error("idb get failed"));
    });
  } finally {
    db.close();
  }
  if (row === undefined || row.ciphertext.length < IV_LEN + 16) return {};
  const plaintext = new Uint8Array(
    await key.decrypt(row.ciphertext.slice(0, IV_LEN), row.ciphertext.slice(IV_LEN)),
  );
  return decodeCarriedCopies(new TextDecoder().decode(plaintext));
}

async function write(
  accountIdB64: string,
  key: DeviceCipher,
  slots: Readonly<Record<string, CarriedSlot>>,
): Promise<void> {
  const db = await openDb();
  try {
    if (Object.keys(slots).length === 0) {
      if (!accountStoresWritable(accountIdB64)) return;
      const tx = db.transaction(STORE, "readwrite");
      tx.objectStore(STORE).delete(accountIdB64);
      await awaitTx(tx);
      return;
    }
    const plaintext = new TextEncoder().encode(encodeCarriedCopies(slots));
    const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
    const ct = new Uint8Array(await key.encrypt(iv, plaintext));
    const blob = new Uint8Array(IV_LEN + ct.length);
    blob.set(iv, 0);
    blob.set(ct, IV_LEN);
    // A wiped account takes no late write (ADR-0077).
    if (!accountStoresWritable(accountIdB64)) return;
    const tx = db.transaction(STORE, "readwrite");
    const row: PersistedRow = { accountId: accountIdB64, ciphertext: blob };
    tx.objectStore(STORE).put(row);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

let loadedFor: string | null = null;
const [slotsSignal, setSlots] = createSignal<Readonly<Record<string, CarriedSlot>>>({});

/** Reactive: every slot of the account last loaded. */
export const carriedSlots: Accessor<Readonly<Record<string, CarriedSlot>>> = slotsSignal;

/** Reactive: the live copies held under `key` (a pending contact's account
 *  id, or a conversation id in hex). */
export function carriedCopiesFor(key: string, nowMs: number = Date.now()): CarriedCopy[] {
  return liveCopies(slotsSignal()[key]?.copies ?? [], nowMs);
}

const queues = new Map<string, Promise<unknown>>();

/** Read the account's copies into the reactive view, once per account per
 *  page. Never throws. */
export async function loadCarriedCopies(accountIdB64: string): Promise<void> {
  if (loadedFor === accountIdB64) return;
  loadedFor = accountIdB64;
  setSlots({});
  await (queues.get(accountIdB64) ?? Promise.resolve()).catch(() => undefined);
  const key = await deviceKeyOf(accountIdB64);
  if (key === null) return;
  try {
    const slots = await read(accountIdB64, key);
    if (loadedFor === accountIdB64) setSlots(slots);
  } catch {
    /* an unreadable row reads as none */
  }
}

/**
 * Apply a change to the account's slots behind the account's write queue.
 * `change` returns `null` to write nothing. Resolves to whether something
 * was written. Never throws.
 */
export function updateCarriedCopies(
  accountIdB64: string,
  change: (slots: Readonly<Record<string, CarriedSlot>>) => Record<string, CarriedSlot> | null,
): Promise<boolean> {
  const run = async (): Promise<boolean> => {
    const key = await deviceKeyOf(accountIdB64);
    if (key === null) return false;
    let held: Record<string, CarriedSlot> = {};
    try {
      held = await read(accountIdB64, key);
    } catch {
      held = {};
    }
    const next = change(held);
    if (next === null) return false;
    try {
      await write(accountIdB64, key, next);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("carried-copies: write failed", err instanceof Error ? err.name : "");
      return false;
    }
    if (loadedFor === accountIdB64) setSlots(next);
    return true;
  };
  const queued = (queues.get(accountIdB64) ?? Promise.resolve()).then(run, run);
  queues.set(accountIdB64, queued);
  return queued;
}

/** File copies under `key` when none are held there yet (a second bundle
 *  for the same contact does not duplicate them). Never throws. */
export function storeCarriedCopies(
  accountIdB64: string,
  key: string,
  copies: readonly CarriedCopy[],
): Promise<boolean> {
  if (copies.length === 0) return Promise.resolve(false);
  return updateCarriedCopies(accountIdB64, (slots) =>
    slots[key] !== undefined ? null : { ...slots, [key]: { copies: [...copies], matched: [] } },
  );
}

/** The pending contact `fromKey` became the one-to-one `toKey`: its copies
 *  follow. Never throws. */
export function moveCarriedCopies(accountIdB64: string, fromKey: string, toKey: string): Promise<boolean> {
  return updateCarriedCopies(accountIdB64, (slots) => movedSlots(slots, fromKey, toKey));
}

/** The pending contact went without a conversation (a link refusal): its
 *  copies go. Never throws. */
export function dropCarriedCopies(accountIdB64: string, key: string): Promise<boolean> {
  return updateCarriedCopies(accountIdB64, (slots) => {
    if (slots[key] === undefined) return null;
    const out = { ...slots };
    delete out[key];
    return out;
  });
}

/** Own messages from another device arrived under `key`: replace the
 *  matching copies. Never throws. */
export function matchCarriedCopies(
  accountIdB64: string,
  key: string,
  arrivals: readonly OwnArrival[],
  nowMs: number = Date.now(),
): Promise<boolean> {
  return updateCarriedCopies(accountIdB64, (slots) => {
    const slot = slots[key];
    if (slot === undefined) return null;
    const next = matchArrivals(slot, arrivals, nowMs);
    if (next === null) return null;
    const out = { ...slots };
    if (next.copies.length === 0) delete out[key];
    else out[key] = next;
    return out;
  });
}

/** Drop every copy of the account (Forget device, Delete account). Throws
 *  when the store cannot be opened; the caller logs. */
export async function deleteCarriedCopiesForAccount(accountIdB64: string): Promise<void> {
  if (typeof indexedDB === "undefined") return;
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).delete(accountIdB64);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

// Forget device and sign-out: the reactive view is the session's.
registerAccountMemory(
  "carried-copies",
  (ids) => {
    if (loadedFor === null || ids.includes(loadedFor)) {
      loadedFor = null;
      setSlots({});
    }
  },
  { signOut: true },
);
