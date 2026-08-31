// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * IndexedDB-backed encrypted store for plaintext chat
 * messages (Phase 2a3f1-web).
 *
 * Mirror of:
 *
 *   * `apps/ios/Lakko/Chat/MessageLogStore.swift`
 *   * `apps/android/.../chat/MessageLogStore.kt`
 *
 * Persists the per-account `{ groupIdHex → (cursor, messages) }`
 * map the UI currently keeps in component-local state. Before
 * this layer the timeline vanished on every page reload —
 * MLS state survived via the snapshot store but its forward-
 * secret chain keys could not re-decrypt the already-consumed
 * wire bytes.
 *
 * ## Schema
 *
 * One row per `accountIdB64`:
 *
 * ```text
 *   db   "lakko-message-log"  v1
 *   ├── store "logs"  keyPath: "accountId" (string)
 *         row {
 *           accountId:  <b64-uuidv7>,
 *           ciphertext: Uint8Array   // [12-byte IV | AES-GCM ct+tag]
 *         }
 * ```
 *
 * Plaintext is the JSON-encoded [`MessageLogPayload`]. Payload
 * `version` 2 adds `PersistedMessage.authorKeyB64`; a v1 row is
 * still read (there is no migration and none is needed — the absent
 * field fails the author gate closed exactly as a reloaded row
 * always did), so the transcript survives rather than being wiped
 * for a field whose absence is already safe.
 * Wrapped under the device root key (the non-extractable
 * AES-256-GCM `CryptoKey` from 1.16c3c2) — same posture as
 * the MLS snapshot blob.
 */

import { publishMessageLogChanged } from "./message-log-events";

const DB_NAME = "lakko-message-log";
const DB_VERSION = 1;
const STORE = "logs";
const IV_LEN = 12;

// ----- JSON payload (mirror of iOS / Android shapes) -------------

/**
 * Payload schema version this client WRITES.
 *
 * v2 adds [`PersistedMessage.authorKeyB64`]. A v1 payload is still
 * read: its rows simply carry no author identity, which fails the
 * ADR-0023 edit/delete gate closed — the same thing every reloaded
 * row did before the field existed. There is no migration and none
 * is owed; discarding the transcript would cost history to fix
 * nothing.
 */
export const MESSAGE_LOG_VERSION = 2;

export interface PersistedAttachment {
  /** Base64-encoded 32-byte SHA-256 blob id. */
  readonly blobIdB64: string;
  /** Base64-encoded 32-byte CEK. */
  readonly keyB64: string;
  /** Base64-encoded 12-byte AEAD nonce. */
  readonly nonceB64: string;
  readonly mime: string | null;
  /** Stored as a string because JS can't round-trip `u64` >2^53
   *  through `JSON.parse`. Always present (no `null`); empty
   *  attachments aren't a thing. */
  readonly sizeStr: string;
  readonly name: string | null;
}

export interface PersistedMessage {
  /** Stable across snapshots. Stored as a string for the same
   *  reason as `sizeStr`. */
  readonly messageId: string;
  readonly direction: "outgoing" | "incoming" | "system";
  readonly text: string;
  readonly attachments: readonly PersistedAttachment[];
  readonly createdAt: string;
  /**
   * ADR-0023 — the author's IDENTITY: their Ed25519 signature public
   * key, base64, resolved from the MLS sender leaf at receipt (see
   * `author-identity.ts`). `null` when the client could not resolve
   * it, and **absent entirely on a v1 row written before this field
   * existed** — both read back as "unknown author", which fails the
   * edit/delete gate closed. That is the same posture every reloaded
   * message had before this field existed; it never misattributes.
   *
   * Persisting it is what makes edit / delete survive a reload at
   * all: the leaf could not be persisted, because a leaf stored now
   * names a different member after the next commit. A signature key
   * does not move, so there is nothing left to defer. No secret is
   * added to the blob — the key is public, and already sits in the
   * MLS snapshot store beside it.
   */
  readonly authorKeyB64?: string | null;
}

export interface PersistedGroup {
  /** DS journal cursor (stringified i64). */
  readonly cursor: string;
  readonly messages: readonly PersistedMessage[];
}

/** Wire shape of the encrypted blob, post-decrypt + JSON-parse. */
export interface MessageLogPayload {
  readonly version: number;
  readonly groups: { readonly [groupIdHex: string]: PersistedGroup };
}

// ----- IDB plumbing ----------------------------------------------

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
    req.onerror = (): void =>
      reject(req.error ?? new Error("indexedDB.open failed"));
  });
}

function awaitTx<T>(tx: IDBTransaction, value: T): Promise<T> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = (): void => resolve(value);
    tx.onabort = (): void => reject(tx.error ?? new Error("idb tx aborted"));
    tx.onerror = (): void => reject(tx.error ?? new Error("idb tx errored"));
  });
}

async function encryptBlob(
  deviceKey: CryptoKey,
  plaintext: Uint8Array,
): Promise<Uint8Array> {
  const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
  const ct = new Uint8Array(
    await crypto.subtle.encrypt(
      { name: "AES-GCM", iv: iv.slice() },
      deviceKey,
      plaintext.slice(),
    ),
  );
  const out = new Uint8Array(IV_LEN + ct.length);
  out.set(iv, 0);
  out.set(ct, IV_LEN);
  return out;
}

async function decryptBlob(
  deviceKey: CryptoKey,
  blob: Uint8Array,
): Promise<Uint8Array> {
  if (blob.length < IV_LEN + 16) {
    throw new Error("encrypted message log is too short to be valid");
  }
  const iv = blob.slice(0, IV_LEN);
  const ct = blob.slice(IV_LEN);
  const pt = new Uint8Array(
    await crypto.subtle.decrypt({ name: "AES-GCM", iv }, deviceKey, ct),
  );
  return pt;
}

// ----- Public CRUD ----------------------------------------------

/**
 * Persist (or replace) the message log for an account.
 *
 * Best-effort: an IDB / WebCrypto failure surfaces as a
 * rejected promise; callers (the chat surface) log + swallow.
 * The in-memory state stays good and the next mutation
 * retries the write.
 */
export async function saveMessageLog(
  accountIdB64: string,
  deviceKey: CryptoKey,
  payload: MessageLogPayload,
): Promise<void> {
  const plaintext = new TextEncoder().encode(JSON.stringify(payload));
  const ciphertext = await encryptBlob(deviceKey, plaintext);
  const row: PersistedRow = { accountId: accountIdB64, ciphertext };
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).put(row);
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
  // G-8: notify any unread badge (in this or another tab/route) that the log
  // changed, so it can reload instead of waiting for its next mount.
  publishMessageLogChanged(accountIdB64);
}

/**
 * Read + decrypt the message log for `accountIdB64`. Returns
 * `null` when:
 *
 *   * no row on disk for this account,
 *   * AES-GCM auth failure (device key rotated since the row
 *     was written, blob tampered, …),
 *   * JSON parse failure (corrupt blob — caller starts with
 *     an empty timeline and re-bootstraps from the DS cursor).
 */
export async function loadMessageLog(
  accountIdB64: string,
  deviceKey: CryptoKey,
): Promise<MessageLogPayload | null> {
  const db = await openDb();
  let row: PersistedRow | undefined;
  try {
    const tx = db.transaction(STORE, "readonly");
    const req = tx.objectStore(STORE).get(accountIdB64);
    row = await new Promise<PersistedRow | undefined>((resolve, reject) => {
      req.onsuccess = (): void => resolve(req.result as PersistedRow | undefined);
      req.onerror = (): void => reject(req.error ?? new Error("idb get failed"));
    });
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
  if (row === undefined) return null;
  let plaintext: Uint8Array;
  try {
    plaintext = await decryptBlob(deviceKey, row.ciphertext);
  } catch {
    return null;
  }
  try {
    return JSON.parse(new TextDecoder().decode(plaintext)) as MessageLogPayload;
  } catch {
    return null;
  }
}

/**
 * Erase the message log for `accountIdB64`. No-op when
 * absent. Called from `forgetDevice`. The device root key
 * itself lives in a different IDB store and gets wiped by
 * the auth layer; this call drops only the per-account
 * message-log row.
 */
export async function deleteMessageLog(accountIdB64: string): Promise<void> {
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).delete(accountIdB64);
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
}

/**
 * ADR-0045 UX §6.2.4 — the pure half of a per-group history delete.
 *
 * No client had one before: {@link deleteMessageLog} and its `mls-store`
 * sibling are whole-ACCOUNT operations. But the log is a per-account blob
 * keyed `{ groupIdHex -> PersistedGroup }` internally, so dropping one group's
 * entry is a read-modify-write on a store that already exists rather than a
 * new one.
 *
 * Kept separate from the IO so it is testable: `vitest` runs in plain `node`
 * with no IndexedDB, so {@link deleteGroupFromMessageLog} cannot be, and this
 * is where the rule that could actually be wrong lives.
 *
 * Returns `null` when the group was **absent**, so the caller can skip the
 * re-encrypt + write entirely. That is not just an optimisation: a rewrite
 * publishes a `message-log-changed` event to every tab, and firing one for a
 * no-op would have unread badges reload for nothing.
 *
 * Never mutates its input.
 */
export function withoutGroup(
  payload: MessageLogPayload,
  groupIdHex: string,
): MessageLogPayload | null {
  if (!Object.prototype.hasOwnProperty.call(payload.groups, groupIdHex)) {
    return null;
  }
  const groups: { [k: string]: PersistedGroup } = {};
  for (const [k, v] of Object.entries(payload.groups)) {
    if (k !== groupIdHex) groups[k] = v;
  }
  return { version: payload.version, groups };
}

/**
 * ADR-0045 UX §6.2.4 — drop one group's transcript from this device.
 *
 * Resolves `false` when there was nothing to drop (no row, an undecryptable
 * row, or no entry for this group), so the caller can tell "deleted" from
 * "there was nothing there" without a second read.
 *
 * **What this does NOT remove, stated because the confirmation copy is exact
 * rather than sweeping:** the inactive MLS group stays in the provider
 * snapshot as inert bytes. `Group` exposes no delete, so a `Group::delete` is
 * a real (small) core follow-up rather than something this rides free. The
 * remnant is inert in the strong sense — it cannot send, decrypt, export or
 * process — but it is not nothing.
 */
export async function deleteGroupFromMessageLog(
  accountIdB64: string,
  deviceKey: CryptoKey,
  groupIdHex: string,
): Promise<boolean> {
  const payload = await loadMessageLog(accountIdB64, deviceKey);
  if (payload === null) return false;
  const next = withoutGroup(payload, groupIdHex);
  if (next === null) return false;
  await saveMessageLog(accountIdB64, deviceKey, next);
  return true;
}
