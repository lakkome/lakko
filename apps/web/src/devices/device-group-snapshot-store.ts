// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * IndexedDB-backed encrypted DeviceGroup snapshot cache
 * (Phase 2a3b2-bootstrap).
 *
 * The **DeviceGroup** (ADR-0007) is a per-user MLS group whose
 * leaves are the user's own devices. To keep the same handle
 * across browser sessions we snapshot the underlying OpenMLS
 * provider state via `Identity.snapshotStorage()` and persist
 * the resulting blob here, wrapped under the device root key.
 *
 * Distinct from:
 *
 *   * The **conversation-group** snapshot store (already
 *     shipped in 1.16e6 for the chat groups) — that one caches
 *     the AccountIdentity provider's storage; this one caches
 *     the **DeviceIdentity** provider's storage. The two
 *     providers live in independent OpenMLS instances and
 *     never share state (ADR-0007 §"binding the two").
 *
 *   * The DeviceIdentity **seed** store
 *     (`device-identity-store.ts`) — that one caches the
 *     32-byte random seed the DeviceIdentity is derived from;
 *     this one caches the OpenMLS provider state built on top
 *     of that derived identity (group state, ratchet trees,
 *     leaf private keys).
 *
 * ## Schema
 *
 * ```text
 *   db   "lakko-device-group-snapshots"  v2
 *   ├── store "snapshots"  keyPath: "accountId"  (string)
 *   │     row {
 *   │       accountId:  <b64-uuidv7>,
 *   │       ciphertext: Uint8Array,   // [12-byte IV | AES-GCM ciphertext+tag]
 *   │       journalCursor?: number,   // DS id of the last journal row the
 *   │                                 // snapshot has consumed
 *   │       generation: number,       // bumped by every write
 *   │     }
 *   └── store "pending"    keyPath: "key"  (string "<kind>:<b64-uuidv7>")
 *         row {
 *           key:        "commit:<account>" | "rejoin:<account>",
 *           accountId:  <b64-uuidv7>,
 *           ciphertext: Uint8Array,   // [12-byte IV | AES-GCM ciphertext+tag]
 *         }
 * ```
 *
 * ## The journal cursor lives in the same row
 *
 * The sync receive loop used to start at cursor 0 on every page load and
 * re-feed the whole journal to OpenMLS (see `sync/journal-batch.ts`). Its
 * position is stored HERE, in the snapshot row and the same `put`, rather than
 * in a store of its own, for the reason `chat/durable-cursor.ts` gives for the
 * chat journals: a cursor may lag the durable MLS state but must never lead
 * it. One write cannot record a position the snapshot beside it has not
 * reached. A write that carries no cursor (every mutation path other than the
 * receive loop) keeps the stored one, in the same transaction, so a snapshot
 * taken after a local publish never discards the loop's position. Deleting the
 * row (`forgetDevice`) deletes the cursor with it.
 *
 * The cursor is stored in the clear: it is a DS journal position, a value the
 * DS itself assigned and already knows.
 *
 * ## The generation (ADR-0081 decision 3)
 *
 * Every write bumps `generation`. The sync runtime's long-lived handle
 * remembers the generation it loaded and writes only while it is still the
 * stored one (`expectedGeneration`): when a commit of this device made its
 * next epoch current through another handle, the long-lived handle is an
 * epoch behind, and its write is refused rather than putting the old epoch
 * back over the new one. A refused write is an error like any other failed
 * write (fail-closed): the receive loop holds its cursor and reloads.
 *
 * ## Pending (ADR-0081 decisions 2 and 4)
 *
 * The `pending` store holds, beside the snapshot and never over it, the one
 * DeviceGroup commit this device has in flight (`commit:<account>`, see
 * `device-group-commit.ts`) and the state of a rejoin in progress
 * (`rejoin:<account>`, see `device-group-rejoin.ts`). Both are opaque here:
 * JSON the caller builds, encrypted under the device key like the snapshot,
 * since both carry MLS private state. Deleting the snapshot deletes them.
 */

import type { DeviceCipher } from "../auth/device-cipher";
import { requireAccountStoresWritable } from "../auth/account-store-gate";

const DB_NAME = "lakko-device-group-snapshots";
const DB_VERSION = 2;
const STORE = "snapshots";
const PENDING_STORE = "pending";

/** What a `pending` row holds. */
export type DeviceGroupPendingKind = "commit" | "rejoin" | "admissions";

interface SnapshotRecord {
  accountId: string;
  /** `[12-byte IV | AES-GCM ciphertext+tag]`. Plaintext is
   *  the `Identity.snapshotStorage()` blob. */
  ciphertext: Uint8Array;
  /** DS id of the last DeviceGroup journal row this snapshot has consumed.
   *  Absent until the receive loop first persists a batch. */
  journalCursor?: number;
  /** Bumped by every write (see the module header). Absent on a row written
   *  before the field existed, read as 0. */
  generation?: number;
}

interface PendingRecord {
  key: string;
  accountId: string;
  ciphertext: Uint8Array;
}

/** A write that named the generation it expected found another one: the
 *  snapshot was replaced through another handle since this one loaded it. */
export class DeviceGroupSnapshotChangedError extends Error {
  override name = "DeviceGroupSnapshotChangedError";
  constructor() {
    super("DeviceGroup snapshot was replaced since this handle loaded it; not overwriting");
  }
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
      if (!db.objectStoreNames.contains(PENDING_STORE)) {
        db.createObjectStore(PENDING_STORE, { keyPath: "key" });
      }
    };
    req.onsuccess = (): void => resolve(req.result);
    req.onerror = (): void =>
      reject(req.error ?? new Error("indexedDB.open failed"));
  });
}

async function seal(plaintext: Uint8Array, deviceKey: DeviceCipher): Promise<Uint8Array> {
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ct = new Uint8Array(
    await deviceKey.encrypt(iv, plaintext.slice()),
  );
  const envelope = new Uint8Array(iv.length + ct.length);
  envelope.set(iv, 0);
  envelope.set(ct, iv.length);
  return envelope;
}

async function open(envelope: Uint8Array, deviceKey: DeviceCipher, what: string): Promise<Uint8Array> {
  if (envelope.length < 12 + 16) {
    throw new Error(`${what} envelope too short (${envelope.length} bytes)`);
  }
  const iv = envelope.slice(0, 12);
  const ct = envelope.slice(12);
  return new Uint8Array(await deviceKey.decrypt(iv, ct));
}

function pendingKey(kind: DeviceGroupPendingKind, accountIdB64: string): string {
  return `${kind}:${accountIdB64}`;
}

/** Encrypt + persist the DeviceGroup MLS snapshot for
 *  `accountIdB64`. Overwrites any prior row and resolves to the generation
 *  the row now carries.
 *
 *  `journalCursor`, when given, is stored with the snapshot in the same write
 *  (the receive loop passes the position the snapshot has consumed up to).
 *  When omitted, the cursor already stored is carried over inside the same
 *  transaction — see the module header.
 *
 *  `expectedGeneration`, when given, makes the write conditional: it is
 *  refused with {@link DeviceGroupSnapshotChangedError}, and nothing is
 *  written, unless the stored row still carries that generation. */
export async function putDeviceGroupSnapshot(
  accountIdB64: string,
  snapshot: Uint8Array,
  deviceKey: DeviceCipher,
  journalCursor?: number,
  expectedGeneration?: number,
): Promise<number> {
  if (snapshot.length === 0) {
    throw new Error("device-group snapshot must be non-empty");
  }
  const envelope = await seal(snapshot, deviceKey);

  const db = await openDb();
  try {
    let written = 0;
    requireAccountStoresWritable(accountIdB64);
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE, "readwrite");
      let refused = false;
      tx.oncomplete = (): void =>
        refused ? reject(new DeviceGroupSnapshotChangedError()) : resolve();
      tx.onerror = (): void =>
        reject(tx.error ?? new Error("IDB put tx error"));
      const store = tx.objectStore(STORE);
      const read = store.get(accountIdB64);
      read.onerror = (): void =>
        reject(read.error ?? new Error("IDB get (cursor carry-over) error"));
      read.onsuccess = (): void => {
        const prior = read.result as SnapshotRecord | undefined;
        const priorGeneration = prior === undefined ? null : (prior.generation ?? 0);
        if (expectedGeneration !== undefined && priorGeneration !== expectedGeneration) {
          refused = true;
          return;
        }
        const cursor = journalCursor ?? prior?.journalCursor;
        written = (priorGeneration ?? 0) + 1;
        const row: SnapshotRecord = {
          accountId: accountIdB64,
          ciphertext: envelope,
          generation: written,
          ...(cursor === undefined ? {} : { journalCursor: cursor }),
        };
        store.put(row);
      };
    });
    return written;
  } finally {
    db.close();
  }
}

async function readSnapshotRow(accountIdB64: string): Promise<SnapshotRecord | undefined> {
  const db = await openDb();
  try {
    return await new Promise((resolve, reject) => {
      const tx = db.transaction(STORE, "readonly");
      const req = tx.objectStore(STORE).get(accountIdB64);
      req.onsuccess = (): void => resolve(req.result as SnapshotRecord | undefined);
      req.onerror = (): void => reject(req.error ?? new Error("IDB get error"));
    });
  } finally {
    db.close();
  }
}

/** Read + decrypt the cached DeviceGroup MLS snapshot for
 *  `accountIdB64`. Returns `null` when absent. Throws on
 *  decrypt failure (device key rotated, tampered, truncated). */
export async function getDeviceGroupSnapshot(
  accountIdB64: string,
  deviceKey: DeviceCipher,
): Promise<Uint8Array | null> {
  const state = await getDeviceGroupSnapshotState(accountIdB64, deviceKey);
  return state === null ? null : state.snapshot;
}

/** {@link getDeviceGroupSnapshot} plus the generation the row carries (pass
 *  it back as `expectedGeneration`). */
export async function getDeviceGroupSnapshotState(
  accountIdB64: string,
  deviceKey: DeviceCipher,
): Promise<{ readonly snapshot: Uint8Array; readonly generation: number } | null> {
  const row = await readSnapshotRow(accountIdB64);
  if (row === undefined) return null;
  const snapshot = await open(row.ciphertext, deviceKey, "device-group snapshot");
  return { snapshot, generation: row.generation ?? 0 };
}

/** Read the DeviceGroup journal cursor stored with the snapshot, or `null`
 *  when there is no row or no cursor in it. Needs no device key: the cursor is
 *  not encrypted (see the module header). */
export async function getDeviceGroupJournalCursor(
  accountIdB64: string,
): Promise<number | null> {
  const row = await readSnapshotRow(accountIdB64);
  return row?.journalCursor ?? null;
}

/** Encrypt + persist one `pending` row of `accountIdB64` (ADR-0081),
 *  replacing the prior row of that kind. Fail-closed: a refused or failed
 *  write throws. */
export async function putDeviceGroupPending(
  accountIdB64: string,
  kind: DeviceGroupPendingKind,
  plaintext: Uint8Array,
  deviceKey: DeviceCipher,
): Promise<void> {
  const envelope = await seal(plaintext, deviceKey);
  const db = await openDb();
  try {
    requireAccountStoresWritable(accountIdB64);
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(PENDING_STORE, "readwrite");
      tx.oncomplete = (): void => resolve();
      tx.onerror = (): void => reject(tx.error ?? new Error("IDB pending put error"));
      const row: PendingRecord = {
        key: pendingKey(kind, accountIdB64),
        accountId: accountIdB64,
        ciphertext: envelope,
      };
      tx.objectStore(PENDING_STORE).put(row);
    });
  } finally {
    db.close();
  }
}

/** Read + decrypt one `pending` row, or `null` when there is none. Throws on
 *  a decrypt failure. */
export async function getDeviceGroupPending(
  accountIdB64: string,
  kind: DeviceGroupPendingKind,
  deviceKey: DeviceCipher,
): Promise<Uint8Array | null> {
  const db = await openDb();
  let row: PendingRecord | undefined;
  try {
    row = await new Promise((resolve, reject) => {
      const tx = db.transaction(PENDING_STORE, "readonly");
      const req = tx.objectStore(PENDING_STORE).get(pendingKey(kind, accountIdB64));
      req.onsuccess = (): void => resolve(req.result as PendingRecord | undefined);
      req.onerror = (): void => reject(req.error ?? new Error("IDB pending get error"));
    });
  } finally {
    db.close();
  }
  if (row === undefined) return null;
  return open(row.ciphertext, deviceKey, "device-group pending");
}

/** Delete one `pending` row. No-op when absent. */
export async function deleteDeviceGroupPending(
  accountIdB64: string,
  kind: DeviceGroupPendingKind,
): Promise<void> {
  const db = await openDb();
  try {
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(PENDING_STORE, "readwrite");
      tx.oncomplete = (): void => resolve();
      tx.onerror = (): void => reject(tx.error ?? new Error("IDB pending delete error"));
      tx.objectStore(PENDING_STORE).delete(pendingKey(kind, accountIdB64));
    });
  } finally {
    db.close();
  }
}

/**
 * ADR-0081 decision 2, step 4 - make a commit's next-epoch snapshot current:
 * one transaction writes the snapshot row (keeping the stored cursor, bumping
 * the generation, unconditionally: this device's commit landed, so this IS
 * the account's state) and leaves the pending commit row in place (its
 * Welcome may still be owed; the caller deletes it once delivered).
 * Fail-closed.
 */
export async function makeDeviceGroupSnapshotCurrent(
  accountIdB64: string,
  snapshot: Uint8Array,
  deviceKey: DeviceCipher,
): Promise<number> {
  return putDeviceGroupSnapshot(accountIdB64, snapshot, deviceKey);
}

/** Erase the cached DeviceGroup snapshot and every pending row of the
 *  account (its in-flight commit, a rejoin in progress). No-op when absent.
 *  Called from `forgetDevice` (ADR-0077: the wipe seals the account's stores
 *  first; a delete is always allowed) and when a device drops its DeviceGroup
 *  state to rejoin (ADR-0081 decision 4). */
export async function deleteDeviceGroupSnapshot(
  accountIdB64: string,
): Promise<void> {
  const db = await openDb();
  try {
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction([STORE, PENDING_STORE], "readwrite");
      tx.oncomplete = (): void => resolve();
      tx.onerror = (): void =>
        reject(tx.error ?? new Error("IDB delete tx error"));
      tx.objectStore(STORE).delete(accountIdB64);
      const pending = tx.objectStore(PENDING_STORE);
      pending.delete(pendingKey("commit", accountIdB64));
      pending.delete(pendingKey("rejoin", accountIdB64));
      pending.delete(pendingKey("admissions", accountIdB64));
    });
  } finally {
    db.close();
  }
}

/** ADR-0081 decision 4, step 1 - a device that fell behind drops its
 *  DeviceGroup state (the snapshot, an in-flight commit, its admission
 *  record) and KEEPS a rejoin in progress, so a reload goes on waiting for
 *  the Welcome it already asked for. */
export async function dropDeviceGroupStateForRejoin(accountIdB64: string): Promise<void> {
  const db = await openDb();
  try {
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction([STORE, PENDING_STORE], "readwrite");
      tx.oncomplete = (): void => resolve();
      tx.onerror = (): void => reject(tx.error ?? new Error("IDB delete tx error"));
      tx.objectStore(STORE).delete(accountIdB64);
      const pending = tx.objectStore(PENDING_STORE);
      pending.delete(pendingKey("commit", accountIdB64));
      pending.delete(pendingKey("admissions", accountIdB64));
    });
  } finally {
    db.close();
  }
}
