// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * IndexedDB-backed encrypted store for MLS provider snapshots
 * (Phase 1.16e6c1).
 *
 * The Rust core (1.16e6a) gives us
 * `Identity.snapshotStorage()` / `Identity.restoreStorage(...)`
 * — opaque bytes that round-trip the provider's
 * `HashMap<Vec<u8>, Vec<u8>>` containing every piece of MLS
 * state OpenMLS has written (signature keypair, ratchet trees,
 * epoch keys, queued proposals, …). The bytes are sensitive —
 * they let any reader continue a live MLS session — so we
 * **MUST** wrap them under the device root key (1.16c3c2's
 * non-extractable AES-256-GCM `CryptoKey`) before they touch
 * disk.
 *
 * Schema (one row per `accountIdB64`):
 *
 * ```text
 *   db   "lakko-mls"  v2
 *   ├── store "snapshots"      keyPath: "accountId"  (string)
 *   │     row {
 *   │       accountId:        <b64-uuidv7>,
 *   │       ciphertext:       Uint8Array,    // [12-byte IV | AES-GCM ciphertext+tag]
 *   │       knownGroupIds:    string[]       // base64 of MLS group_id bytes
 *   │     }
 *   └── store "commit-outbox"  keyPath: "id"  (string)   // ADR-0040 (G-35)
 *         durable pending-commit entries — see commit-outbox.ts. Co-located in
 *         THIS db (not a separate one) so an entry + the N+1 snapshot commit in
 *         ONE atomic `transaction([snapshots, commit-outbox], "readwrite")` via
 *         `saveSnapshotWithCommitEntry`, making the forbidden "snapshot at N+1
 *         with no outbox entry" state impossible by construction.
 * ```
 *
 * The DB version was bumped v1 -> v2 to add the `commit-outbox` store. All
 * `lakko-mls` opens funnel through `openMlsDb` here, so the bump is global; the
 * `onupgradeneeded` guard creates each store only when missing (fresh install
 * creates both; an existing v1 gains only `commit-outbox`).
 *
 * Why one blob per account rather than per group: the OpenMLS
 * provider serialises **all** groups + signing keys + key
 * package state into a single HashMap. There's no clean
 * per-group split inside the storage. Per-group state can be
 * pulled out with `Group::load(identity, group_id)` after
 * the whole HashMap has been restored. We additionally
 * record the list of `groupIdB64`'s separately so the
 * rehydration path knows which `Group::load` calls to make
 * (the snapshot itself is opaque to us — we can't iterate it
 * without parsing the Rust-side format).
 *
 * The known-groups list is **not** wrapped under the device
 * key: it's just opaque base64 strings of public MLS
 * `group_id` bytes (already shared with the DS as a routing
 * address). Encrypting them would buy nothing and cost a
 * little code.
 */

import type { PendingCommitEntry } from "./commit-outbox";

const DB_NAME = "lakko-mls";
/** v2 adds the ADR-0040 `commit-outbox` store alongside `snapshots`. */
const DB_VERSION = 2;
const STORE = "snapshots";
/** ADR-0040 (G-35) — the durable pending-commit outbox store (keyPath `id`). */
export const OUTBOX_STORE = "commit-outbox";

/** Length of the AES-GCM IV we prepend to each ciphertext. */
const IV_LEN = 12;

/** Result returned by `loadMlsSnapshot` — the decrypted MLS
 *  provider snapshot ready to feed straight into
 *  `Identity.restoreStorage(...)`, plus the public list of
 *  group_ids the rehydrate path should drive through
 *  `Group.load`. */
export interface MlsSnapshotRecord {
  /** Decrypted MLS provider snapshot bytes — opaque payload,
   *  ready to hand to `Identity.restoreStorage(...)`. */
  snapshotBytes: Uint8Array;
  /** Base64-encoded `group_id`s for every group OpenMLS state
   *  the snapshot carries. Used by the rehydration path to
   *  drive `Group.load` per id. */
  knownGroupIds: string[];
}

/** Internal on-disk row shape. The `ciphertext` here actually
 *  contains `[12-byte IV | AES-GCM ciphertext+tag]` — distinct
 *  from the decrypted-result type returned by `loadMlsSnapshot`. */
interface PersistedRow {
  accountId: string;
  ciphertext: Uint8Array;
  knownGroupIds: string[];
}

/**
 * Open the `lakko-mls` database (v2), creating the `snapshots` and
 * `commit-outbox` (ADR-0040) stores on first upgrade. Exported so the
 * `commit-outbox` helpers share the exact same opener + version — a single
 * canonical open path keeps the two stores in one database, which is what makes
 * the entry + snapshot single-transaction write atomic.
 */
export function openMlsDb(): Promise<IDBDatabase> {
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
      if (!db.objectStoreNames.contains(OUTBOX_STORE)) {
        db.createObjectStore(OUTBOX_STORE, { keyPath: "id" });
      }
    };
    req.onsuccess = (): void => resolve(req.result);
    req.onerror = (): void =>
      reject(req.error ?? new Error("indexedDB.open failed"));
  });
}

/** Resolve when `tx` completes; reject on abort / error. Exported for the
 *  `commit-outbox` writes (they run in the same database). */
export function awaitTx<T>(tx: IDBTransaction, value: T): Promise<T> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = (): void => resolve(value);
    tx.onabort = (): void => reject(tx.error ?? new Error("idb tx aborted"));
    tx.onerror = (): void => reject(tx.error ?? new Error("idb tx errored"));
  });
}

// ---------------------------------------------------------------
// Encryption helpers — AES-GCM with the device root key
// ---------------------------------------------------------------

async function encryptSnapshot(
  deviceKey: CryptoKey,
  plaintext: Uint8Array,
): Promise<Uint8Array> {
  // 12 bytes is the AES-GCM-recommended IV length and matches
  // WebCrypto's default. Fresh per call — never re-use an IV
  // under the same key.
  const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
  // `.slice()` to guarantee an `ArrayBuffer` backing for
  // strict-mode TS (matches the pattern in device-key.ts).
  const ct = new Uint8Array(
    await crypto.subtle.encrypt(
      { name: "AES-GCM", iv: iv.slice() },
      deviceKey,
      plaintext.slice(),
    ),
  );
  // Concat: [12-byte IV | ciphertext+tag].
  const out = new Uint8Array(IV_LEN + ct.length);
  out.set(iv, 0);
  out.set(ct, IV_LEN);
  return out;
}

async function decryptSnapshot(
  deviceKey: CryptoKey,
  blob: Uint8Array,
): Promise<Uint8Array> {
  if (blob.length < IV_LEN + 16) {
    // 16 bytes is the AES-GCM tag floor. Anything shorter than
    // IV+tag can't be a valid ciphertext.
    throw new Error("encrypted snapshot is too short to be valid");
  }
  const iv = blob.slice(0, IV_LEN);
  const ct = blob.slice(IV_LEN);
  const pt = new Uint8Array(
    await crypto.subtle.decrypt({ name: "AES-GCM", iv }, deviceKey, ct),
  );
  return pt;
}

// ---------------------------------------------------------------
// Public CRUD
// ---------------------------------------------------------------

/**
 * Persist (or replace) the MLS snapshot for an account.
 *
 * @param accountIdB64    Same string the rest of the auth
 *                        layer uses to identify the account.
 * @param deviceKey       Non-extractable AES-256-GCM key
 *                        produced by `deriveDeviceRootKey`
 *                        (1.16c3c2). The plaintext bytes
 *                        never leave the WebCrypto sandbox.
 * @param snapshotBytes   Output of
 *                        `Identity.snapshotStorage()`.
 * @param knownGroupIds   Base64-encoded `group_id`s the
 *                        rehydrate path should later drive
 *                        through `Group.load`.
 */
export async function saveMlsSnapshot(
  accountIdB64: string,
  deviceKey: CryptoKey,
  snapshotBytes: Uint8Array,
  knownGroupIds: readonly string[],
): Promise<void> {
  const ciphertext = await encryptSnapshot(deviceKey, snapshotBytes);
  const row: PersistedRow = {
    accountId: accountIdB64,
    ciphertext,
    knownGroupIds: [...knownGroupIds],
  };
  const db = await openMlsDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).put(row);
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
}

/**
 * ADR-0040 (G-35) — persist the MLS snapshot AND a durable pending-commit outbox
 * entry in ONE atomic IndexedDB transaction spanning both stores of the
 * `lakko-mls` database, awaited, THROWING on failure (fail-closed).
 *
 * This is the load-bearing atomicity primitive for the crash-safe commit paths:
 * a group-membership commit path calls this BEFORE publishing the commit. Because
 * IndexedDB multi-store transactions are all-or-nothing, the entry (holding the
 * commit + Welcome / rendezvous wire-bytes) and the N+1 snapshot land together or
 * not at all — so the forbidden state "durably at N+1 with no re-publishable
 * commit recorded" (which would strand the whole group) can never occur, and an
 * un-acked entry read on restart UNCONDITIONALLY means the device is at N+1 (no
 * torn-write reconciliation needed).
 *
 * A throw here MUST abort the commit path (do NOT publish): on the next launch
 * the device is at N with no entry — clean, as if the op never happened.
 *
 * @param entry  the FULL outbox entry (its `accountId` + `acked: false` already
 *               set by the caller) — stored as plain base64: opaque MLS public
 *               artefacts, no plaintext / PII (invariants #1 / #2), unlike the
 *               device-key-encrypted snapshot row.
 */
export async function saveSnapshotWithCommitEntry(
  accountIdB64: string,
  deviceKey: CryptoKey,
  snapshotBytes: Uint8Array,
  knownGroupIds: readonly string[],
  entry: PendingCommitEntry,
): Promise<void> {
  const ciphertext = await encryptSnapshot(deviceKey, snapshotBytes);
  const snapshotRow: PersistedRow = {
    accountId: accountIdB64,
    ciphertext,
    knownGroupIds: [...knownGroupIds],
  };
  const db = await openMlsDb();
  try {
    // ONE transaction over BOTH stores — atomic: both puts land on
    // `tx.oncomplete`, or neither does on any abort/error.
    const tx = db.transaction([STORE, OUTBOX_STORE], "readwrite");
    tx.objectStore(STORE).put(snapshotRow);
    tx.objectStore(OUTBOX_STORE).put(entry);
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
}

/**
 * Read the MLS snapshot for the given account, decrypt it
 * with `deviceKey`, return the bytes + the known-group-id
 * list. Returns `null` when there's no row for that account
 * yet (typical first-ever load). Throws on decrypt failure
 * (key mismatch / tampered ciphertext).
 */
export async function loadMlsSnapshot(
  accountIdB64: string,
  deviceKey: CryptoKey,
): Promise<MlsSnapshotRecord | null> {
  const db = await openMlsDb();
  let row: PersistedRow | undefined;
  try {
    const tx = db.transaction(STORE, "readonly");
    const req = tx.objectStore(STORE).get(accountIdB64);
    row = await new Promise<PersistedRow | undefined>((resolve, reject) => {
      req.onsuccess = (): void =>
        resolve(req.result as PersistedRow | undefined);
      req.onerror = (): void =>
        reject(req.error ?? new Error("idb get failed"));
    });
  } finally {
    db.close();
  }
  if (row === undefined) return null;

  const snapshotBytes = await decryptSnapshot(deviceKey, row.ciphertext);
  return {
    snapshotBytes,
    knownGroupIds: row.knownGroupIds,
  };
}

/**
 * Delete the MLS snapshot for the given account. No-op when
 * no row exists. Called from `AuthProvider.forgetDevice` so a
 * device wipe leaves no per-account residue.
 */
export async function deleteMlsSnapshot(accountIdB64: string): Promise<void> {
  const db = await openMlsDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).delete(accountIdB64);
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
}

/**
 * Wipe **every** MLS snapshot, regardless of account. Useful
 * from a global "delete all data" UX (1.16f+); the regular
 * `forgetDevice` path stays per-account.
 */
export async function clearAllMlsSnapshots(): Promise<void> {
  const db = await openMlsDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).clear();
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
}
