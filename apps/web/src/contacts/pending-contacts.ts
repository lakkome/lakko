// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0079 section 5 — the person this account added through their invite
 * link, before their app has answered.
 *
 * After **Add** on an invite page, the inviter is a contact at once on this
 * side (UX section 4): they appear in the contact list and the chat list
 * under the name the link states, and this account may write to them. The
 * messages are NOT kept here: they go into the ONE held-send queue
 * (`chat/held-send-queue.ts`, ADR-0073 R.13b, sealed under the device key in
 * the message log) under {@link pendingQueueKey}, the inviter's account id,
 * which names no conversation so nothing flushes it. When the inviter's
 * Welcome lands they move, in order, to the queue slot of the new
 * one-to-one, which sends them, and the record goes. iOS and Android keep
 * them the same way (reworked 2026-10-17).
 *
 * One row per account, AES-256-GCM under the device key, every write behind
 * the account store gate (ADR-0077).
 *
 * ```text
 *   db   "lakko-pending-contacts"  v2
 *   └── store "rows"  keyPath: "accountId"
 *         row { accountId, ciphertext: Uint8Array  // [12-byte IV | ct+tag] }
 *         plaintext: JSON { v: 3, contacts: PendingContact[] }
 * ```
 *
 * The rules (what is shown, which line the chat shows, which record a
 * joined one-to-one adopts) are the pure functions below; the store is a
 * thin layer under them.
 */
import { createSignal, type Accessor } from "solid-js";

import { registerAccountMemory } from "../auth/account-memory";
import { accountStoresWritable } from "../auth/account-store-gate";
import { getDeviceKey } from "../auth/key-store";
import { heldSends, persistHeldSends } from "../chat/held-send-queue";
import type { DeviceCipher } from "../auth/device-cipher";

/** After this long without an answer the chat line changes (UX section 4). */
export const PENDING_STALE_MS = 7 * 24 * 60 * 60 * 1000;

/** The inviter, as the link named them. */
export interface PendingContact {
  /** The inviter's 16-byte account id, lower-case hex. */
  readonly accountIdHex: string;
  /** The inviter's account key (Ed25519), standard base64: the key the
   *  one-to-one's roster and the contact list name them by. */
  readonly signaturePubkeyB64: string;
  /** The inviter's X25519 key, standard base64. */
  readonly x25519PubkeyB64: string;
  /** The display name the link states, or `""`. */
  readonly displayName: string;
  /** The username the link states, or `""`. */
  readonly username: string;
  /** When **Add** was pressed: the 7-day line counts from here. */
  readonly addedAtMs: number;
  /** The id of the invite link the request answered (16 bytes, lower-case
   *  hex): a link refusal must name it (ADR-0079 amendment of 2026-10-19). */
  readonly linkIdHex: string;
}

/** The held-send queue key of what is written to a pending contact before
 *  the chat exists: the inviter's account id (32 hex characters), never a
 *  conversation id (64), so the held-send flush never sends from it. */
export function pendingQueueKey(contact: Pick<PendingContact, "accountIdHex">): string {
  return contact.accountIdHex;
}

/** The last message written to a pending contact (the chat list's preview
 *  line), or `""`. */
export function lastPendingMessageText(contact: Pick<PendingContact, "accountIdHex">): string {
  const key = pendingQueueKey(contact);
  const held = heldSends.all().find(([k]) => k === key)?.[1] ?? [];
  return held[held.length - 1]?.payload.text ?? "";
}

/** The part of the held-send store the hand-over uses. */
export interface HeldQueue<T> {
  take(key: string): { readonly heldAtMs: number; readonly payload: T }[];
  hold(key: string, payload: T, heldAtMs: number): void;
}

/**
 * The hand-over (ADR-0079 section 5): move what was written to `contact`
 * from its pending key to the held-send slot of the joined one-to-one, in
 * the order written. Their give-up clock starts at `nowMs`: they could not
 * be sent before the conversation existed. Returns how many moved. Pure over
 * the store it is given.
 */
export function moveHeldToConversation<T>(
  store: HeldQueue<T>,
  contact: Pick<PendingContact, "accountIdHex">,
  conversationHex: string,
  nowMs: number,
): number {
  const items = store.take(pendingQueueKey(contact));
  for (const item of items) store.hold(conversationHex, item.payload, nowMs);
  return items.length;
}

function isContact(v: unknown): v is PendingContact {
  if (typeof v !== "object" || v === null) return false;
  const c = v as Record<string, unknown>;
  return (
    typeof c["accountIdHex"] === "string" &&
    /^[0-9a-f]{32}$/.test(c["accountIdHex"] as string) &&
    typeof c["signaturePubkeyB64"] === "string" &&
    typeof c["x25519PubkeyB64"] === "string" &&
    typeof c["displayName"] === "string" &&
    typeof c["username"] === "string" &&
    typeof c["addedAtMs"] === "number" &&
    typeof c["linkIdHex"] === "string" &&
    /^[0-9a-f]{32}$/.test(c["linkIdHex"] as string)
  );
}

/** Parse the sealed plaintext; anything else reads as none. Pure. */
export function decodePendingContacts(json: string): PendingContact[] {
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return [];
  }
  const p = parsed as { v?: unknown; contacts?: unknown } | null;
  // Version 1 kept the messages inside the record and version 2 had no link
  // id; neither is read (nothing is deployed, no migration).
  if (p?.v !== 3) return [];
  const list = p.contacts;
  if (!Array.isArray(list)) return [];
  const out: PendingContact[] = [];
  for (const c of list) {
    if (!isContact(c)) continue;
    out.push({
      accountIdHex: c.accountIdHex,
      signaturePubkeyB64: c.signaturePubkeyB64,
      x25519PubkeyB64: c.x25519PubkeyB64,
      displayName: c.displayName,
      username: c.username,
      addedAtMs: c.addedAtMs,
      linkIdHex: c.linkIdHex,
    });
  }
  return out;
}

export function encodePendingContacts(list: readonly PendingContact[]): string {
  return JSON.stringify({ v: 3, contacts: list });
}

/** `list` with `contact` added, or the existing record for the same person
 *  kept with its first `addedAtMs` (a second Add through a fresh link does
 *  not reset the 7-day line). Pure. */
export function withPendingContact(
  list: readonly PendingContact[],
  contact: PendingContact,
): PendingContact[] {
  const at = list.findIndex((c) => c.accountIdHex === contact.accountIdHex);
  if (at === -1) return [...list, contact];
  const held = list[at] as PendingContact;
  const next: PendingContact = { ...contact, addedAtMs: Math.min(held.addedAtMs, contact.addedAtMs) };
  return list.map((c, i) => (i === at ? next : c));
}

/** `list` without the record of `accountIdHex`. Pure. */
export function withoutPendingContact(
  list: readonly PendingContact[],
  accountIdHex: string,
): PendingContact[] {
  return list.filter((c) => c.accountIdHex !== accountIdHex);
}


/**
 * ADR-0079 amendment of 2026-10-19 - the pending contact a link refusal
 * cancels: the one whose stated account key signed it over the link that
 * contact's request answered and this account's key (`verify` is the core's
 * `verifyLinkRefusal`). `null` when none does: a refusal nobody but the
 * inviter could sign, for another link or another person, changes nothing.
 * Pure over `verify`.
 */
export function pendingContactRefusedBy(
  list: readonly PendingContact[],
  refusal: Uint8Array,
  ownAccountKey: Uint8Array,
  verify: (wire: Uint8Array, inviterKey: Uint8Array, linkId: Uint8Array, ownKey: Uint8Array) => boolean,
): PendingContact | null {
  for (const c of list) {
    let inviterKey: Uint8Array;
    try {
      inviterKey = base64ToBytes(c.signaturePubkeyB64);
    } catch {
      continue;
    }
    if (verify(refusal, inviterKey, hexToBytes(c.linkIdHex), ownAccountKey)) return c;
  }
  return null;
}

function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** The line the pending chat shows (UX section 4). Pure. */
export function pendingLine(contact: PendingContact, nowMs: number): "waiting" | "stale" {
  return nowMs - contact.addedAtMs >= PENDING_STALE_MS ? "stale" : "waiting";
}

/**
 * The pending contacts the lists show: those the account does not already
 * hold as a contact with a one-to-one. Once the inviter's app has answered,
 * the ordinary contact row and chat row take over. Pure.
 */
export function shownPendingContacts(
  list: readonly PendingContact[],
  oneToOnePeerKeysB64: ReadonlySet<string>,
): PendingContact[] {
  return list.filter((c) => !oneToOnePeerKeysB64.has(c.signaturePubkeyB64));
}

/**
 * The pending contact a joined one-to-one belongs to: the one whose account
 * key is a person of the new conversation. `undefined` for a group, or for a
 * one-to-one with someone else. Pure.
 */
export function pendingContactForConversation(
  list: readonly PendingContact[],
  conversation: { readonly oneToOne: boolean; readonly personKeysB64: readonly string[] },
): PendingContact | undefined {
  if (!conversation.oneToOne) return undefined;
  const people = new Set(conversation.personKeysB64);
  return list.find((c) => people.has(c.signaturePubkeyB64));
}

/** The name a pending contact is shown by: the stated display name, else
 *  `@username`, else `null` (the caller's catalog fallback). Pure. */
export function pendingNameOf(contact: Pick<PendingContact, "displayName" | "username">): string | null {
  if (contact.displayName.trim().length > 0) return contact.displayName;
  if (contact.username.length > 0) return `@${contact.username}`;
  return null;
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

const DB_NAME = "lakko-pending-contacts";
// The store layout is unchanged; the plaintext's own `v` moved to 2.
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

async function read(accountIdB64: string, key: DeviceCipher): Promise<PendingContact[]> {
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
  if (row === undefined || row.ciphertext.length < IV_LEN + 16) return [];
  const plaintext = new Uint8Array(
    await key.decrypt(row.ciphertext.slice(0, IV_LEN), row.ciphertext.slice(IV_LEN)),
  );
  return decodePendingContacts(new TextDecoder().decode(plaintext));
}

async function write(accountIdB64: string, key: DeviceCipher, list: readonly PendingContact[]): Promise<void> {
  const db = await openDb();
  try {
    if (list.length === 0) {
      if (!accountStoresWritable(accountIdB64)) return;
      const tx = db.transaction(STORE, "readwrite");
      tx.objectStore(STORE).delete(accountIdB64);
      await awaitTx(tx);
      return;
    }
    const plaintext = new TextEncoder().encode(encodePendingContacts(list));
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

/** The account the reactive list was filled for. */
let loadedFor: string | null = null;
const [pending, setPending] = createSignal<readonly PendingContact[]>([]);

/** Reactive: the pending contacts of the account last loaded. */
export const pendingContacts: Accessor<readonly PendingContact[]> = pending;

// One writer per account at a time on this page.
const queues = new Map<string, Promise<unknown>>();

/**
 * Read the account's pending contacts into the reactive list. Once per
 * account per page (a later call returns what is held). Never throws.
 */
export async function loadPendingContacts(accountIdB64: string): Promise<void> {
  if (loadedFor === accountIdB64) return;
  loadedFor = accountIdB64;
  setPending([]);
  await (queues.get(accountIdB64) ?? Promise.resolve()).catch(() => undefined);
  const key = await deviceKeyOf(accountIdB64);
  if (key === null) return;
  try {
    const list = await read(accountIdB64, key);
    if (loadedFor === accountIdB64) setPending(list);
  } catch {
    /* an unreadable row reads as none */
  }
}

/**
 * Read the account's pending contacts from the store, behind the account's
 * write queue (so a write in flight is seen). `[]` when there are none or
 * the row cannot be read. Never throws.
 */
export function readPendingContacts(accountIdB64: string): Promise<readonly PendingContact[]> {
  const run = async (): Promise<readonly PendingContact[]> => {
    const key = await deviceKeyOf(accountIdB64);
    if (key === null) return [];
    try {
      return await read(accountIdB64, key);
    } catch {
      return [];
    }
  };
  const queued = (queues.get(accountIdB64) ?? Promise.resolve()).then(run, run);
  queues.set(accountIdB64, queued);
  return queued;
}

/**
 * Apply a change to the account's list: the reactive list at once, the
 * store behind the account's write queue. The change returns `null` to
 * refuse (nothing is written). Resolves to the new list, or `null` when
 * refused or when the write failed. Never throws.
 */
export function updatePendingContacts(
  accountIdB64: string,
  change: (list: readonly PendingContact[]) => PendingContact[] | null,
): Promise<readonly PendingContact[] | null> {
  const run = async (): Promise<readonly PendingContact[] | null> => {
    const key = await deviceKeyOf(accountIdB64);
    if (key === null) return null;
    let held: PendingContact[] = [];
    try {
      held = await read(accountIdB64, key);
    } catch {
      held = [];
    }
    const next = change(held);
    if (next === null) return null;
    try {
      await write(accountIdB64, key, next);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("pending-contacts: write failed", err instanceof Error ? err.name : "");
      return null;
    }
    if (loadedFor === accountIdB64) setPending(next);
    return next;
  };
  const queued = (queues.get(accountIdB64) ?? Promise.resolve()).then(run, run);
  queues.set(accountIdB64, queued);
  return queued;
}

/** Drop every pending contact of the account (Forget device, Delete
 *  account). Throws when the store cannot be opened; the caller logs. */
export async function deletePendingContactsForAccount(accountIdB64: string): Promise<void> {
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

/**
 * A pending contact goes: the record and the messages kept for it in the
 * held-send queue (UX section 7 item 4). Called when the inviter's app
 * refused the request (ADR-0079 amendment of 2026-10-19); no control lets B
 * delete a pending contact. Resolves to whether the record was removed.
 * Never throws.
 */
export async function removePendingContact(accountIdB64: string, accountIdHex: string): Promise<boolean> {
  const next = await updatePendingContacts(accountIdB64, (list) =>
    list.some((c) => c.accountIdHex === accountIdHex) ? withoutPendingContact(list, accountIdHex) : null,
  );
  if (heldSends.take(pendingQueueKey({ accountIdHex })).length > 0) await persistHeldSends(accountIdB64);
  return next !== null;
}

/**
 * ADR-0098: the pending contact's account was deleted. The messages kept for
 * it in the held-send queue are discarded, never sent; the record stays, read
 * as "Deleted account", until `Delete chat` removes it. Never throws.
 */
export async function dropHeldForDeletedPending(accountIdB64: string, accountIdHex: string): Promise<void> {
  try {
    if (heldSends.take(pendingQueueKey({ accountIdHex })).length > 0) await persistHeldSends(accountIdB64);
  } catch {
    // eslint-disable-next-line no-console
    console.warn("pending-contacts: held messages not dropped");
  }
}

/**
 * Hand the messages of the pending contact a joined conversation belongs to
 * over to `deliver` (it moves them from {@link pendingQueueKey} to the
 * held-send slot of the new one-to-one), then drop the record, inside the
 * account's write queue. The record is dropped only when `deliver` reports
 * the messages safely kept; otherwise it stays, and the next join or reload
 * tries again. Resolves to the contact that was handed over, or `null`.
 * Never throws.
 */
export function handOverPendingContact(
  accountIdB64: string,
  conversation: { readonly oneToOne: boolean; readonly personKeysB64: readonly string[] },
  deliver: (contact: PendingContact) => Promise<boolean>,
): Promise<PendingContact | null> {
  const run = async (): Promise<PendingContact | null> => {
    const key = await deviceKeyOf(accountIdB64);
    if (key === null) return null;
    let held: PendingContact[];
    try {
      held = await read(accountIdB64, key);
    } catch {
      return null;
    }
    const contact = pendingContactForConversation(held, conversation);
    if (contact === undefined) return null;
    let kept = false;
    try {
      kept = await deliver(contact);
    } catch {
      kept = false;
    }
    if (!kept) return null;
    const next = withoutPendingContact(held, contact.accountIdHex);
    try {
      await write(accountIdB64, key, next);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("pending-contacts: write failed", err instanceof Error ? err.name : "");
    }
    if (loadedFor === accountIdB64) setPending(next);
    return contact;
  };
  const queued = (queues.get(accountIdB64) ?? Promise.resolve()).then(run, run);
  queues.set(accountIdB64, queued);
  return queued;
}

// Forget device and sign-out: the reactive list is the session's.
registerAccountMemory(
  "pending-contacts",
  (ids) => {
    if (loadedFor === null || ids.includes(loadedFor)) {
      loadedFor = null;
      setPending([]);
    }
  },
  { signOut: true },
);
