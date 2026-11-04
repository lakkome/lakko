// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The recall record (ADR-0074 section 1.1; `docs/ux/identity-recovery.md`
 * section 18.4).
 *
 * A device that learns the account's keys were reset on another device
 * deletes everything the account kept on it at once, EXCEPT this record:
 * what it needs to ask "Was it you?" and, when the answer is no, to prove
 * to the server that it is a device of the account from before the reset.
 *
 * What it holds, and nothing else of the account:
 *
 *   - the account id (the row key, plain, as every per-account store has it);
 *   - the OLD account identity's 32-byte Ed25519 seed (not the 64-byte
 *     phrase seed and not the phrase): what signs the recall statements;
 *   - the username, for the screen;
 *   - when the device learned of the reset (unix ms): the screen's date and
 *     the local 30-day end of the record.
 *
 * It is sealed under the account's non-extractable device root key
 * (`key-store.ts`), the protection the phrase seed had, which is why the
 * staged wipe keeps that key until the record goes.
 *
 * ```text
 *   db   "lakko-recall-record"  v1
 *   └── store "records"  keyPath: "accountId"
 *         row { accountId: <b64 account id>,
 *               ciphertext: [12-byte IV | AES-GCM ciphertext+tag] }
 * ```
 *
 * The plaintext is `version(1) ‖ learned_at_ms(8 BE) ‖ seed(32) ‖
 * username (UTF-8, the rest)`.
 */

import type { DeviceCipher } from "./device-cipher";
import { requireAccountStoresWritable } from "./account-store-gate";

const DB_NAME = "lakko-recall-record";
const DB_VERSION = 1;
const STORE = "records";

/** The old account identity's Ed25519 seed length. */
export const RECALL_SEED_LENGTH = 32;
const PLAINTEXT_VERSION = 1;
const HEADER_LEN = 1 + 8 + RECALL_SEED_LENGTH;
/** A username is at most 32 characters of `[a-z0-9_]`; a guest has none. */
const MAX_USERNAME_BYTES = 128;

export interface RecallRecord {
  /** Standard base64 of the 16-byte account id (the form `AuthState` has). */
  readonly accountIdB64: string;
  /** The OLD account identity's 32-byte Ed25519 seed. */
  readonly oldSeed: Uint8Array;
  /** For the screen; empty for an account without a username. */
  readonly username: string;
  /** When this device learned of the reset, unix ms. */
  readonly learnedAtMs: number;
}

/** The record's plaintext. Throws on a seed of the wrong length. */
export function encodeRecallPlaintext(record: RecallRecord): Uint8Array {
  if (record.oldSeed.length !== RECALL_SEED_LENGTH) {
    throw new Error(`recall seed length ${record.oldSeed.length} != ${RECALL_SEED_LENGTH}`);
  }
  if (!Number.isSafeInteger(record.learnedAtMs) || record.learnedAtMs < 0) {
    throw new Error("recall record time is not a non-negative integer");
  }
  const name = new TextEncoder().encode(record.username);
  if (name.length > MAX_USERNAME_BYTES) throw new Error("recall record username is too long");
  const out = new Uint8Array(HEADER_LEN + name.length);
  out[0] = PLAINTEXT_VERSION;
  new DataView(out.buffer).setBigUint64(1, BigInt(record.learnedAtMs), false);
  out.set(record.oldSeed, 9);
  out.set(name, HEADER_LEN);
  return out;
}

/** The record of `accountIdB64` from its plaintext, or `null` when the bytes
 *  are not a record this build reads (never a partial one). */
export function decodeRecallPlaintext(accountIdB64: string, plain: Uint8Array): RecallRecord | null {
  if (plain.length < HEADER_LEN || plain.length > HEADER_LEN + MAX_USERNAME_BYTES) return null;
  if (plain[0] !== PLAINTEXT_VERSION) return null;
  const at = new DataView(plain.buffer, plain.byteOffset, plain.byteLength).getBigUint64(1, false);
  if (at > BigInt(Number.MAX_SAFE_INTEGER)) return null;
  let username: string;
  try {
    username = new TextDecoder("utf-8", { fatal: true }).decode(plain.subarray(HEADER_LEN));
  } catch {
    return null;
  }
  return {
    accountIdB64,
    oldSeed: plain.slice(9, HEADER_LEN),
    username,
    learnedAtMs: Number(at),
  };
}

interface RecallRow {
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

/** Seal `record` under `deviceKey` and write it. Overwrites a prior row. */
export async function putRecallRecord(record: RecallRecord, deviceKey: DeviceCipher): Promise<void> {
  const plain = encodeRecallPlaintext(record);
  const iv = crypto.getRandomValues(new Uint8Array(12));
  let ct: Uint8Array;
  try {
    ct = new Uint8Array(await deviceKey.encrypt(iv, plain));
  } finally {
    plain.fill(0);
  }
  const envelope = new Uint8Array(iv.length + ct.length);
  envelope.set(iv, 0);
  envelope.set(ct, iv.length);
  const db = await openDb();
  try {
    // A wiped account takes no late write (`account-store-gate.ts`). The
    // staged wipe's record is written BEFORE that wipe seals the account, so
    // this refuses only a write that outlived a wipe; it throws, because the
    // caller then keeps nothing and runs the full wipe.
    requireAccountStoresWritable(record.accountIdB64);
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE, "readwrite");
      tx.oncomplete = (): void => resolve();
      tx.onerror = (): void => reject(tx.error ?? new Error("IDB put tx error"));
      tx.objectStore(STORE).put({
        accountId: record.accountIdB64,
        ciphertext: envelope,
      } satisfies RecallRow);
    });
  } finally {
    db.close();
  }
}

/** The record of `accountIdB64`, or `null` when there is none. Throws when
 *  the row does not open under `deviceKey` or is not a record. */
export async function getRecallRecord(
  accountIdB64: string,
  deviceKey: DeviceCipher,
): Promise<RecallRecord | null> {
  const db = await openDb();
  let row: RecallRow | undefined;
  try {
    row = await new Promise((resolve, reject) => {
      const tx = db.transaction(STORE, "readonly");
      const req = tx.objectStore(STORE).get(accountIdB64);
      req.onsuccess = (): void => resolve(req.result as RecallRow | undefined);
      req.onerror = (): void => reject(req.error ?? new Error("IDB get error"));
    });
  } finally {
    db.close();
  }
  if (row === undefined) return null;
  if (row.ciphertext.length < 12 + 16 + HEADER_LEN) {
    throw new Error("recall record envelope too short");
  }
  const iv = row.ciphertext.slice(0, 12);
  const ct = row.ciphertext.slice(12);
  const plain = new Uint8Array(await deviceKey.decrypt(iv, ct));
  try {
    const record = decodeRecallPlaintext(accountIdB64, plain);
    if (record === null) throw new Error("recall record is not readable");
    return record;
  } finally {
    plain.fill(0);
  }
}

/** The account ids that have a record on this browser (plain row keys; the
 *  page load reads them to find a "Was it you?" screen that is owed). */
export async function listRecallAccounts(): Promise<string[]> {
  const db = await openDb();
  try {
    return await new Promise((resolve, reject) => {
      const tx = db.transaction(STORE, "readonly");
      const req = tx.objectStore(STORE).getAllKeys();
      req.onsuccess = (): void =>
        resolve((req.result as unknown[]).filter((k): k is string => typeof k === "string"));
      req.onerror = (): void => reject(req.error ?? new Error("IDB getAllKeys error"));
    });
  } finally {
    db.close();
  }
}

/** Whether a record row exists for `accountIdB64` (without opening it). */
export async function hasRecallRecord(accountIdB64: string): Promise<boolean> {
  return (await listRecallAccounts()).includes(accountIdB64);
}

/** Destroy the record of `accountIdB64`. No-op when absent. */
export async function deleteRecallRecord(accountIdB64: string): Promise<void> {
  const db = await openDb();
  try {
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE, "readwrite");
      tx.oncomplete = (): void => resolve();
      tx.onerror = (): void => reject(tx.error ?? new Error("IDB delete tx error"));
      tx.objectStore(STORE).delete(accountIdB64);
    });
  } finally {
    db.close();
  }
}
