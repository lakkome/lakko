// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The prepared files of unsent media messages (`docs/ux/connection-state.md`
 * section 7.3): the bytes an upload sends (a recording, a video note and its
 * poster frame), kept until the upload finished so a waiting voice message
 * or video note survives a reload and goes when the network returns.
 *
 * One record per outbox entry, encrypted under the account's device key
 * (`auth/device-cipher.ts`), behind the ADR-0077 store gate:
 *
 * ```text
 *   db   "lakko-unsent-files"  v1
 *   └── store "files"  keyPath: "key"
 *         row {
 *           key:        "<accountId>|<groupIdB64>|<tempId>",
 *           accountId:  <account id, standard base64>,
 *           ciphertext: Uint8Array   // [12-byte IV | AES-GCM ct+tag]
 *         }
 * ```
 *
 * The plaintext is `version(1) || flags(1) || [len(4 BE) || bytes] ||
 * [len(4 BE) || poster] || [count(2 BE) || count x part]`, each present when
 * its flag bit is set (bit 0 the file, bit 1 the poster, bit 2 the tray
 * parts). A part is `pflags(1) || [len || bytes] || [len || thumb] ||
 * [len || cover]` (pflags bit 0 present at all, 1 thumb, 2 cover). The message fields are not here: they are in the
 * message log's `outbox` list (`unsent-outbox.ts`).
 */

import type { DeviceCipher } from "../auth/device-cipher";
import { accountStoresWritable } from "../auth/account-store-gate";

const DB_NAME = "lakko-unsent-files";
const DB_VERSION = 1;
const STORE = "files";
const IV_LEN = 12;
const FORMAT = 1;

/** One composer-tray item's prepared bytes. */
export interface PreparedPartBytes {
  /** The bytes the upload sends (a photo after its re-encode). */
  readonly bytes: Uint8Array;
  /** Its thumb, uploaded as its own blob. */
  readonly thumb?: Uint8Array;
  /** An audio file's cover. */
  readonly cover?: Uint8Array;
}

/** What a prepared media message carries besides its message fields. */
export interface PreparedFile {
  /** The bytes the upload sends (after any resizing or re-encoding). */
  readonly bytes?: Uint8Array;
  /** A video note's poster frame, uploaded as its thumbnail. */
  readonly posterBytes?: Uint8Array;
  /** A message from the composer tray: one slot per item of the message, in
   *  order; `null` for an item already uploaded. */
  readonly parts?: readonly (PreparedPartBytes | null)[];
}

interface FileRow {
  key: string;
  accountId: string;
  ciphertext: Uint8Array;
}

/** The record key of one entry. */
export function unsentFileKey(accountIdB64: string, groupIdB64: string, tempId: number): string {
  return `${accountIdB64}|${groupIdB64}|${tempId}`;
}

/** The plaintext of a record. Pure. */
export function encodePreparedFile(file: PreparedFile): Uint8Array {
  const parts: Uint8Array[] = [];
  let flags = 0;
  let length = 2;
  for (const [bit, part] of [
    [1, file.bytes],
    [2, file.posterBytes],
  ] as const) {
    if (part === undefined) continue;
    flags |= bit;
    parts.push(part);
    length += 4 + part.length;
  }
  let tail: Uint8Array | null = null;
  if (file.parts !== undefined) {
    flags |= 4;
    tail = encodeParts(file.parts);
    length += tail.length;
  }
  const out = new Uint8Array(length);
  out[0] = FORMAT;
  out[1] = flags;
  let at = 2;
  const view = new DataView(out.buffer);
  for (const part of parts) {
    view.setUint32(at, part.length);
    at += 4;
    out.set(part, at);
    at += part.length;
  }
  if (tail !== null) out.set(tail, at);
  return out;
}

function encodeParts(parts: readonly (PreparedPartBytes | null)[]): Uint8Array {
  const chunks: Uint8Array[] = [];
  const head = new Uint8Array(2);
  new DataView(head.buffer).setUint16(0, parts.length);
  chunks.push(head);
  for (const p of parts) {
    const pflags = p === null ? 0 : 1 | (p.thumb === undefined ? 0 : 2) | (p.cover === undefined ? 0 : 4);
    chunks.push(new Uint8Array([pflags]));
    if (p === null) continue;
    for (const piece of [p.bytes, p.thumb, p.cover]) {
      if (piece === undefined) continue;
      const len = new Uint8Array(4);
      new DataView(len.buffer).setUint32(0, piece.length);
      chunks.push(len, piece);
    }
  }
  const total = chunks.reduce((n, c) => n + c.length, 0);
  const out = new Uint8Array(total);
  let at = 0;
  for (const c of chunks) {
    out.set(c, at);
    at += c.length;
  }
  return out;
}

/** The record a plaintext reads back as; `null` for anything else. Pure. */
export function decodePreparedFile(plain: Uint8Array): PreparedFile | null {
  if (plain.length < 2 || plain[0] !== FORMAT) return null;
  const flags = plain[1] as number;
  if ((flags & ~7) !== 0) return null;
  const view = new DataView(plain.buffer, plain.byteOffset, plain.byteLength);
  let at = 2;
  const take = (): Uint8Array | null => {
    if (at + 4 > plain.length) return null;
    const len = view.getUint32(at);
    at += 4;
    if (at + len > plain.length) return null;
    const part = plain.slice(at, at + len);
    at += len;
    return part;
  };
  let bytes: Uint8Array | undefined;
  let posterBytes: Uint8Array | undefined;
  if ((flags & 1) !== 0) {
    const part = take();
    if (part === null) return null;
    bytes = part;
  }
  if ((flags & 2) !== 0) {
    const part = take();
    if (part === null) return null;
    posterBytes = part;
  }
  let parts: (PreparedPartBytes | null)[] | undefined;
  if ((flags & 4) !== 0) {
    if (at + 2 > plain.length) return null;
    const count = view.getUint16(at);
    at += 2;
    parts = [];
    for (let i = 0; i < count; i += 1) {
      if (at + 1 > plain.length) return null;
      const pflags = plain[at] as number;
      at += 1;
      if ((pflags & ~7) !== 0) return null;
      if (pflags === 0) {
        parts.push(null);
        continue;
      }
      if ((pflags & 1) === 0) return null;
      const main = take();
      if (main === null) return null;
      let thumb: Uint8Array | undefined;
      let cover: Uint8Array | undefined;
      if ((pflags & 2) !== 0) {
        const p = take();
        if (p === null) return null;
        thumb = p;
      }
      if ((pflags & 4) !== 0) {
        const p = take();
        if (p === null) return null;
        cover = p;
      }
      parts.push({ bytes: main, ...(thumb === undefined ? {} : { thumb }), ...(cover === undefined ? {} : { cover }) });
    }
  }
  if (at !== plain.length) return null;
  return {
    ...(bytes === undefined ? {} : { bytes }),
    ...(posterBytes === undefined ? {} : { posterBytes }),
    ...(parts === undefined ? {} : { parts }),
  };
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
        db.createObjectStore(STORE, { keyPath: "key" });
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

/** Store (or replace) one entry's prepared file. Rejects on a store or
 *  cipher failure; a wiped account's write is dropped (ADR-0077). */
export async function putUnsentFile(
  accountIdB64: string,
  deviceKey: DeviceCipher,
  groupIdB64: string,
  tempId: number,
  file: PreparedFile,
): Promise<void> {
  if (!accountStoresWritable(accountIdB64)) return;
  const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
  const ct = new Uint8Array(await deviceKey.encrypt(iv, encodePreparedFile(file)));
  const ciphertext = new Uint8Array(IV_LEN + ct.length);
  ciphertext.set(iv, 0);
  ciphertext.set(ct, IV_LEN);
  const row: FileRow = {
    key: unsentFileKey(accountIdB64, groupIdB64, tempId),
    accountId: accountIdB64,
    ciphertext,
  };
  const db = await openDb();
  try {
    // Asked again with no await before the transaction (the gate's rule).
    if (!accountStoresWritable(accountIdB64)) return;
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).put(row);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

/** Read one entry's prepared file; `null` when absent or unreadable. */
export async function getUnsentFile(
  accountIdB64: string,
  deviceKey: DeviceCipher,
  groupIdB64: string,
  tempId: number,
): Promise<PreparedFile | null> {
  const db = await openDb();
  let row: FileRow | undefined;
  try {
    const tx = db.transaction(STORE, "readonly");
    const req = tx.objectStore(STORE).get(unsentFileKey(accountIdB64, groupIdB64, tempId));
    row = await new Promise<FileRow | undefined>((resolve, reject) => {
      req.onsuccess = (): void => resolve(req.result as FileRow | undefined);
      req.onerror = (): void => reject(req.error ?? new Error("idb get failed"));
    });
    await awaitTx(tx);
  } finally {
    db.close();
  }
  if (row === undefined || row.ciphertext.length < IV_LEN + 16) return null;
  try {
    const plain = new Uint8Array(
      await deviceKey.decrypt(row.ciphertext.slice(0, IV_LEN), row.ciphertext.slice(IV_LEN)),
    );
    return decodePreparedFile(plain);
  } catch {
    return null;
  }
}

/** Delete one entry's prepared file. No-op when absent. */
export async function deleteUnsentFile(
  accountIdB64: string,
  groupIdB64: string,
  tempId: number,
): Promise<void> {
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).delete(unsentFileKey(accountIdB64, groupIdB64, tempId));
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

/** Every record key this store holds for `accountIdB64` (tests and the
 *  sweep of orphans read it). */
export async function unsentFileKeysFor(accountIdB64: string): Promise<string[]> {
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readonly");
    const req = tx.objectStore(STORE).getAllKeys();
    const keys = await new Promise<IDBValidKey[]>((resolve, reject) => {
      req.onsuccess = (): void => resolve(req.result);
      req.onerror = (): void => reject(req.error ?? new Error("idb getAllKeys failed"));
    });
    await awaitTx(tx);
    const prefix = `${accountIdB64}|`;
    return keys.filter((k): k is string => typeof k === "string" && k.startsWith(prefix));
  } finally {
    db.close();
  }
}

/** Forget device, a signed-out or reset device's wipe, account deletion:
 *  every prepared file of the account goes. */
export async function deleteUnsentFilesForAccount(accountIdB64: string): Promise<void> {
  const keys = await unsentFileKeysFor(accountIdB64);
  if (keys.length === 0) return;
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    const store = tx.objectStore(STORE);
    for (const k of keys) store.delete(k);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}
