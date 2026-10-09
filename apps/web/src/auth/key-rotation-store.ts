// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a device keeps, sealed under its device key, for a key rotation
 * (ADR-0066 part 3, Stage A):
 *
 *   - `predecessorSeeds`: the 64-byte seeds of the account identities this
 *     device held before each PROVEN rotation, oldest first. The
 *     conversation leaves that exist stay bound to the oldest one until
 *     Stage B moves them, so the device needs it to keep its own standing in
 *     its groups and to read the mailbox those leaves name.
 *   - `pending`: `key-rotation` envelopes received from the account's own
 *     DeviceGroup and verified, held until the server side shows their key
 *     as the account's (a kept device).
 *   - `prepared`: on the rotating device, the new entropy and the proof it
 *     prepared, kept from the envelope until the commit has its answer.
 *
 * One AES-GCM row per account in its own IndexedDB database. Never in the
 * recall record's keep-list: both wipes delete it.
 */
import type { PendingRotation } from "../devices/key-rotation";
import { base64Decode, base64Encode } from "./account-id";

const DB_NAME = "lakko-key-rotation";
const DB_VERSION = 1;
const STORE = "state";

/** What the rotating device prepared (step 3 of ADR 3.2). */
export interface PreparedRotation {
  /** The removed device this rotation belongs to (URL-safe base64). */
  readonly deviceIdB64: string;
  /** The 16-byte entropy of the NEW phrase, standard base64. */
  readonly entropyB64: string;
  /** The 145-byte supersession proof, standard base64. */
  readonly proofB64: string;
  /** The new account key, standard base64. */
  readonly newSignaturePubkeyB64: string;
  readonly preparedAtMs: number;
  /** When the proof stops being accepted, unix seconds. */
  readonly proofExpiresAtSecs: number;
}

export interface KeyRotationState {
  readonly predecessorSeedsB64: readonly string[];
  readonly pending: readonly PendingRotation[];
  readonly prepared: PreparedRotation | null;
}

export const EMPTY_KEY_ROTATION_STATE: KeyRotationState = {
  predecessorSeedsB64: [],
  pending: [],
  prepared: null,
};

interface Row {
  accountId: string;
  ciphertext: Uint8Array;
}

function isPending(value: unknown): value is PendingRotation {
  if (typeof value !== "object" || value === null) return false;
  const p = value as Partial<PendingRotation>;
  return (
    typeof p.newSignaturePubkeyB64 === "string" &&
    typeof p.envelopeB64 === "string" &&
    typeof p.preparedAtMs === "number" &&
    Number.isFinite(p.preparedAtMs)
  );
}

function isPrepared(value: unknown): value is PreparedRotation {
  if (typeof value !== "object" || value === null) return false;
  const p = value as Partial<PreparedRotation>;
  return (
    typeof p.deviceIdB64 === "string" &&
    typeof p.entropyB64 === "string" &&
    typeof p.proofB64 === "string" &&
    typeof p.newSignaturePubkeyB64 === "string" &&
    typeof p.preparedAtMs === "number" &&
    typeof p.proofExpiresAtSecs === "number"
  );
}

/** Parse the sealed plaintext. Anything that is not a state reads as the
 *  empty one, field by field: a damaged pending list must not cost the
 *  predecessor seed. Exported for the test. */
export function decodeKeyRotationState(json: string): KeyRotationState {
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return EMPTY_KEY_ROTATION_STATE;
  }
  if (typeof parsed !== "object" || parsed === null) return EMPTY_KEY_ROTATION_STATE;
  const p = parsed as Record<string, unknown>;
  const seeds = Array.isArray(p["predecessorSeedsB64"])
    ? p["predecessorSeedsB64"].filter((s): s is string => {
        if (typeof s !== "string") return false;
        try {
          return base64Decode(s).length === 64;
        } catch {
          return false;
        }
      })
    : [];
  const pending = Array.isArray(p["pending"]) ? p["pending"].filter(isPending) : [];
  const prepared = isPrepared(p["prepared"]) ? p["prepared"] : null;
  return { predecessorSeedsB64: seeds, pending, prepared };
}

export function encodeKeyRotationState(state: KeyRotationState): string {
  return JSON.stringify({
    v: 1,
    predecessorSeedsB64: state.predecessorSeedsB64,
    pending: state.pending,
    prepared: state.prepared,
  });
}

/** The state with `seed` appended as the newest predecessor (once). */
export function withPredecessorSeed(state: KeyRotationState, seed: Uint8Array): KeyRotationState {
  const b64 = base64Encode(seed);
  if (state.predecessorSeedsB64.includes(b64)) return state;
  return { ...state, predecessorSeedsB64: [...state.predecessorSeedsB64, b64] };
}

/** The seed of the key the conversation leaves are bound to: the OLDEST
 *  predecessor, or `null` when the account never rotated on this device. */
export function conversationSeed(state: KeyRotationState): Uint8Array | null {
  const first = state.predecessorSeedsB64[0];
  return first === undefined ? null : base64Decode(first);
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

/** The state of `accountIdB64`; the empty state when there is no row. Throws
 *  when the row does not open under `deviceKey`. */
export async function getKeyRotationState(
  accountIdB64: string,
  deviceKey: CryptoKey,
): Promise<KeyRotationState> {
  const db = await openDb();
  let row: Row | undefined;
  try {
    row = await new Promise((resolve, reject) => {
      const tx = db.transaction(STORE, "readonly");
      const req = tx.objectStore(STORE).get(accountIdB64);
      req.onsuccess = (): void => resolve(req.result as Row | undefined);
      req.onerror = (): void => reject(req.error ?? new Error("IDB get error"));
    });
  } finally {
    db.close();
  }
  if (row === undefined) return EMPTY_KEY_ROTATION_STATE;
  const iv = row.ciphertext.slice(0, 12);
  const ct = row.ciphertext.slice(12);
  const plain = new Uint8Array(await crypto.subtle.decrypt({ name: "AES-GCM", iv }, deviceKey, ct));
  try {
    return decodeKeyRotationState(new TextDecoder().decode(plain));
  } finally {
    plain.fill(0);
  }
}

/** Seal and write the state. Overwrites the row. */
export async function putKeyRotationState(
  accountIdB64: string,
  state: KeyRotationState,
  deviceKey: CryptoKey,
): Promise<void> {
  const plain = new TextEncoder().encode(encodeKeyRotationState(state));
  const iv = crypto.getRandomValues(new Uint8Array(12));
  let ct: Uint8Array;
  try {
    ct = new Uint8Array(await crypto.subtle.encrypt({ name: "AES-GCM", iv }, deviceKey, plain));
  } finally {
    plain.fill(0);
  }
  const envelope = new Uint8Array(iv.length + ct.length);
  envelope.set(iv, 0);
  envelope.set(ct, iv.length);
  const db = await openDb();
  try {
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE, "readwrite");
      tx.oncomplete = (): void => resolve();
      tx.onerror = (): void => reject(tx.error ?? new Error("IDB put tx error"));
      tx.objectStore(STORE).put({ accountId: accountIdB64, ciphertext: envelope } satisfies Row);
    });
  } finally {
    db.close();
  }
}

/** Read, change, write. One writer per page at a time per account. */
const queues = new Map<string, Promise<unknown>>();
export function updateKeyRotationState(
  accountIdB64: string,
  deviceKey: CryptoKey,
  change: (state: KeyRotationState) => KeyRotationState,
): Promise<KeyRotationState> {
  const run = async (): Promise<KeyRotationState> => {
    const next = change(await getKeyRotationState(accountIdB64, deviceKey));
    await putKeyRotationState(accountIdB64, next, deviceKey);
    return next;
  };
  const queued = (queues.get(accountIdB64) ?? Promise.resolve()).then(run, run);
  queues.set(
    accountIdB64,
    queued.catch(() => undefined),
  );
  return queued;
}

export async function deleteKeyRotationState(accountIdB64: string): Promise<void> {
  if (typeof indexedDB === "undefined") return;
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
