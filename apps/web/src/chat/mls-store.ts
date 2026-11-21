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

import type { DeviceCipher } from "../auth/device-cipher";
import { requireAccountStoresWritable } from "../auth/account-store-gate";
import { requireTabActive } from "../tab/tab-state";
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
  /** ADR-0050 — base64 of the leaf signature key of the identity the
   *  snapshot was taken under. A snapshot taken under another identity (a
   *  pre-ADR-0050 account-key snapshot, or another device's seed) must not
   *  be restored into this one; `undefined` on a row written before the
   *  field existed, which the rehydrate path treats as a mismatch. */
  leafKeyB64?: string;
  /** When this device created or joined each group, ms since epoch, by
   *  base64 `group_id`. Joining counts as activity in the conversation list
   *  and has to outlast a reload, when no message may say so yet. */
  joinedAtMs?: Readonly<Record<string, number>>;
  /** docs/ux/one-tab.md backstop — the row's write generation (0 on a row
   *  written before the field existed). A realm adopts it when it restores
   *  the row and may only write over that same generation. */
  generation: number;
}

/** Internal on-disk row shape. The `ciphertext` here actually
 *  contains `[12-byte IV | AES-GCM ciphertext+tag]` — distinct
 *  from the decrypted-result type returned by `loadMlsSnapshot`. */
interface PersistedRow {
  accountId: string;
  ciphertext: Uint8Array;
  knownGroupIds: string[];
  leafKeyB64?: string;
  joinedAtMs?: Readonly<Record<string, number>>;
  /** Bumped by every write (see {@link SnapshotWriter}). */
  generation?: number;
  /** The epoch of every conversation group the provider held at the write,
   *  decimal, by base64 `group_id` (see {@link SnapshotWriter}). */
  groupEpochs?: Readonly<Record<string, string>>;
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
  deviceKey: DeviceCipher,
  plaintext: Uint8Array,
): Promise<Uint8Array> {
  // 12 bytes is the AES-GCM-recommended IV length and matches
  // WebCrypto's default. Fresh per call — never re-use an IV
  // under the same key.
  const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
  // `.slice()` to guarantee an `ArrayBuffer` backing for
  // strict-mode TS (matches the pattern in device-key.ts).
  const ct = new Uint8Array(
    await deviceKey.encrypt(iv, plaintext),
  );
  // Concat: [12-byte IV | ciphertext+tag].
  const out = new Uint8Array(IV_LEN + ct.length);
  out.set(iv, 0);
  out.set(ct, IV_LEN);
  return out;
}

async function decryptSnapshot(
  deviceKey: DeviceCipher,
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
    await deviceKey.decrypt(iv, ct),
  );
  return pt;
}

// ---------------------------------------------------------------
// Public CRUD
// ---------------------------------------------------------------

/**
 * A write refused because the row on disk is newer than what this realm holds
 * (docs/ux/one-tab.md section 6). Another tab of the browser wrote the row
 * after this realm restored it, or this realm's copy of a group is at an
 * epoch below the one on disk. Nothing was written; the realm must reload
 * from disk rather than write again.
 */
export class StaleProviderError extends Error {
  override name = "StaleProviderError";
  constructor(reason: "generation" | "epoch") {
    super(`the stored MLS snapshot is newer than this page's copy (${reason})`);
  }
}

/** What one write captures, at its turn in the write chain. */
export interface SnapshotCapture {
  /** `Identity.snapshotStorage()`, taken INSIDE the capture call. */
  readonly snapshotBytes: Uint8Array;
  readonly knownGroupIds: readonly string[];
  readonly leafKeyB64?: string;
  readonly joinedAtMs?: Readonly<Record<string, number>>;
  /** The epoch of every conversation group the provider holds, by base64
   *  `group_id`, read in the same synchronous turn as the snapshot. */
  readonly groupEpochs?: Readonly<Record<string, bigint>>;
}

/**
 * The one writer of a realm's MLS provider snapshot (docs/ux/one-tab.md
 * backstops; the 2026-11-21 stranded-owner defect).
 *
 * Three rules, each closing one way an older copy of the provider reached disk
 * after a newer one:
 *
 * 1. **One chain per realm.** Writes run one at a time in call order, and each
 *    takes `snapshotStorage()` inside its turn ({@link SnapshotCapture} is a
 *    callback). Two persists of one page used to capture, then encrypt and
 *    open the database concurrently, so the older capture could land last.
 * 2. **Generation check.** Every write bumps the row's generation. A realm
 *    adopts the generation of the row it restored ({@link adoptLoaded}) and a
 *    write finds, in the same readwrite transaction, that the row is still
 *    at that generation; otherwise another tab wrote since and the write is
 *    refused with {@link StaleProviderError}. A realm that never restored the
 *    row (no device key at the time) is not bound by it.
 * 3. **Epoch check.** A write may not put any conversation group at an epoch
 *    below the one the row holds for it.
 *
 * A refusal writes nothing, throws (so a fail-closed persist stops its
 * publish) and calls the stale listener (`main.tsx` freezes the page and
 * reloads). Holds a frozen page to {@link requireTabActive} too.
 *
 * It also remembers the epoch of each group as of the last write that landed
 * ({@link durableEpoch}): the cursor epoch is recorded from it, never from
 * the in-memory group (`chat/client.ts`, ADR-0091 phase 1b).
 */
export class SnapshotWriter {
  private chain: Promise<unknown> = Promise.resolve();
  private readonly expected = new Map<string, number>();
  private readonly durable = new Map<string, bigint>();
  private readonly staleListeners = new Set<() => void>();

  /** Observe a refused stale write. */
  onStale(fn: () => void): () => void {
    this.staleListeners.add(fn);
    return () => {
      this.staleListeners.delete(fn);
    };
  }

  /** This realm restored `accountIdB64`'s row at `generation` (0 when there
   *  was no row), with these group epochs. */
  adoptLoaded(
    accountIdB64: string,
    generation: number,
    groupEpochs: Readonly<Record<string, bigint>> = {},
  ): void {
    this.expected.set(accountIdB64, generation);
    for (const [gid, epoch] of Object.entries(groupEpochs)) this.durable.set(gid, epoch);
  }

  /** A group loaded from the snapshot this realm restored is at `epoch` on
   *  disk. */
  noteRestoredEpoch(groupIdB64: string, epoch: bigint): void {
    this.durable.set(groupIdB64, epoch);
  }

  /** The epoch `groupIdB64` was at in the last snapshot this realm restored
   *  or wrote, `undefined` when it has none. */
  durableEpoch(groupIdB64: string): bigint | undefined {
    return this.durable.get(groupIdB64);
  }

  /** The row was deleted (Forget device, a recover that starts over). */
  forgetAccount(accountIdB64: string): void {
    this.expected.delete(accountIdB64);
  }

  /** Sign-out: forget everything this realm knew. */
  reset(): void {
    this.expected.clear();
    this.durable.clear();
  }

  /**
   * Write the provider snapshot (and, with `entry`, the ADR-0040 outbox entry
   * in the same transaction). Resolves once on disk; rejects on any failure,
   * {@link StaleProviderError} when refused as stale.
   */
  write(
    accountIdB64: string,
    deviceKey: DeviceCipher,
    capture: () => SnapshotCapture,
    entry?: PendingCommitEntry,
  ): Promise<void> {
    const run = (): Promise<void> => this.runWrite(accountIdB64, deviceKey, capture, entry);
    const result = this.chain.then(run, run);
    this.chain = result.catch(() => undefined);
    return result;
  }

  private isStale(
    accountIdB64: string,
    prior: PersistedRow | undefined,
    epochs: Readonly<Record<string, bigint>>,
  ): "generation" | "epoch" | null {
    if (prior === undefined) return null;
    const expected = this.expected.get(accountIdB64);
    if (expected !== undefined && (prior.generation ?? 0) !== expected) return "generation";
    const stored = prior.groupEpochs ?? {};
    for (const [gid, epoch] of Object.entries(epochs)) {
      const onDisk = stored[gid];
      if (onDisk !== undefined && /^\d{1,20}$/.test(onDisk) && BigInt(onDisk) > epoch) return "epoch";
    }
    return null;
  }

  private async runWrite(
    accountIdB64: string,
    deviceKey: DeviceCipher,
    capture: () => SnapshotCapture,
    entry: PendingCommitEntry | undefined,
  ): Promise<void> {
    requireTabActive();
    const cap = capture();
    const epochs = cap.groupEpochs ?? {};
    const ciphertext = await encryptSnapshot(deviceKey, cap.snapshotBytes);
    const groupEpochs: Record<string, string> = {};
    for (const [gid, epoch] of Object.entries(epochs)) groupEpochs[gid] = epoch.toString();
    const base: PersistedRow = {
      accountId: accountIdB64,
      ciphertext,
      knownGroupIds: [...cap.knownGroupIds],
      ...(cap.leafKeyB64 === undefined ? {} : { leafKeyB64: cap.leafKeyB64 }),
      ...(cap.joinedAtMs === undefined ? {} : { joinedAtMs: cap.joinedAtMs }),
      groupEpochs,
    };
    const verdict: { stale: "generation" | "epoch" | null; generation: number } = {
      stale: null,
      generation: 0,
    };
    const put = (tx: IDBTransaction): void => {
      const store = tx.objectStore(STORE);
      const read = store.get(accountIdB64);
      read.onsuccess = (): void => {
        const prior = read.result as PersistedRow | undefined;
        verdict.stale = this.isStale(accountIdB64, prior, epochs);
        if (verdict.stale !== null) return;
        verdict.generation = (prior?.generation ?? 0) + 1;
        store.put({ ...base, generation: verdict.generation });
        if (entry !== undefined) tx.objectStore(OUTBOX_STORE).put(entry);
      };
    };
    const db = await openMlsDb();
    try {
      requireTabActive();
      if (entry === undefined) {
        requireAccountStoresWritable(accountIdB64);
        const tx = db.transaction(STORE, "readwrite");
        put(tx);
        await awaitTx(tx, undefined);
      } else {
        // ONE transaction over BOTH stores — atomic: both puts land on
        // `tx.oncomplete`, or neither does on any abort/error.
        requireAccountStoresWritable(accountIdB64);
        const tx = db.transaction([STORE, OUTBOX_STORE], "readwrite");
        put(tx);
        await awaitTx(tx, undefined);
      }
    } finally {
      db.close();
    }
    if (verdict.stale !== null) {
      for (const fn of [...this.staleListeners]) {
        try {
          fn();
        } catch {
          /* the refusal below is what the caller acts on */
        }
      }
      throw new StaleProviderError(verdict.stale);
    }
    this.expected.set(accountIdB64, verdict.generation);
    for (const [gid, epoch] of Object.entries(epochs)) this.durable.set(gid, epoch);
  }
}

/** The realm's writer: every MLS provider snapshot of this page goes through it. */
export const mlsSnapshotWriter = new SnapshotWriter();

/**
 * Persist (or replace) the MLS snapshot for an account, through
 * {@link mlsSnapshotWriter}.
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
 * @param joinedAtMs      When this device created or joined each
 *                        of them.
 */
export async function saveMlsSnapshot(
  accountIdB64: string,
  deviceKey: DeviceCipher,
  snapshotBytes: Uint8Array,
  knownGroupIds: readonly string[],
  leafKeyB64?: string,
  joinedAtMs?: Readonly<Record<string, number>>,
): Promise<void> {
  await mlsSnapshotWriter.write(accountIdB64, deviceKey, () => ({
    snapshotBytes,
    knownGroupIds,
    ...(leafKeyB64 === undefined ? {} : { leafKeyB64 }),
    ...(joinedAtMs === undefined ? {} : { joinedAtMs }),
  }));
}

/**
 * ADR-0040 (G-35) — persist the MLS snapshot AND a durable pending-commit outbox
 * entry in ONE atomic IndexedDB transaction spanning both stores of the
 * `lakko-mls` database, awaited, THROWING on failure (fail-closed). Through
 * {@link mlsSnapshotWriter}.
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
  deviceKey: DeviceCipher,
  snapshotBytes: Uint8Array,
  knownGroupIds: readonly string[],
  entry: PendingCommitEntry,
  leafKeyB64?: string,
  joinedAtMs?: Readonly<Record<string, number>>,
): Promise<void> {
  await mlsSnapshotWriter.write(
    accountIdB64,
    deviceKey,
    () => ({
      snapshotBytes,
      knownGroupIds,
      ...(leafKeyB64 === undefined ? {} : { leafKeyB64 }),
      ...(joinedAtMs === undefined ? {} : { joinedAtMs }),
    }),
    entry,
  );
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
  deviceKey: DeviceCipher,
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
    ...(row.leafKeyB64 === undefined ? {} : { leafKeyB64: row.leafKeyB64 }),
    ...(row.joinedAtMs === undefined ? {} : { joinedAtMs: row.joinedAtMs }),
    generation: row.generation ?? 0,
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
  mlsSnapshotWriter.forgetAccount(accountIdB64);
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
  mlsSnapshotWriter.reset();
}
