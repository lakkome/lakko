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
 *   db   "lakko-device-group-snapshots"  v1
 *   └── store "snapshots"  keyPath: "accountId"  (string)
 *         row {
 *           accountId:  <b64-uuidv7>,
 *           ciphertext: Uint8Array,   // [12-byte IV | AES-GCM ciphertext+tag]
 *         }
 * ```
 */

const DB_NAME = "lakko-device-group-snapshots";
const DB_VERSION = 1;
const STORE = "snapshots";

interface SnapshotRecord {
  accountId: string;
  /** `[12-byte IV | AES-GCM ciphertext+tag]`. Plaintext is
   *  the `Identity.snapshotStorage()` blob. */
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

/** Encrypt + persist the DeviceGroup MLS snapshot for
 *  `accountIdB64`. Overwrites any prior row. */
export async function putDeviceGroupSnapshot(
  accountIdB64: string,
  snapshot: Uint8Array,
  deviceKey: CryptoKey,
): Promise<void> {
  if (snapshot.length === 0) {
    throw new Error("device-group snapshot must be non-empty");
  }
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ct = new Uint8Array(
    await crypto.subtle.encrypt(
      { name: "AES-GCM", iv },
      deviceKey,
      snapshot.slice(),
    ),
  );
  const envelope = new Uint8Array(iv.length + ct.length);
  envelope.set(iv, 0);
  envelope.set(ct, iv.length);

  const db = await openDb();
  try {
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE, "readwrite");
      tx.oncomplete = (): void => resolve();
      tx.onerror = (): void =>
        reject(tx.error ?? new Error("IDB put tx error"));
      tx.objectStore(STORE).put({
        accountId: accountIdB64,
        ciphertext: envelope,
      } satisfies SnapshotRecord);
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
  deviceKey: CryptoKey,
): Promise<Uint8Array | null> {
  const db = await openDb();
  try {
    const row: SnapshotRecord | undefined = await new Promise(
      (resolve, reject) => {
        const tx = db.transaction(STORE, "readonly");
        const req = tx.objectStore(STORE).get(accountIdB64);
        req.onsuccess = (): void =>
          resolve(req.result as SnapshotRecord | undefined);
        req.onerror = (): void =>
          reject(req.error ?? new Error("IDB get error"));
      },
    );
    if (row === undefined) return null;
    if (row.ciphertext.length < 12 + 16) {
      throw new Error(
        `device-group snapshot envelope too short (${row.ciphertext.length} bytes)`,
      );
    }
    const iv = row.ciphertext.slice(0, 12);
    const ct = row.ciphertext.slice(12);
    return new Uint8Array(
      await crypto.subtle.decrypt({ name: "AES-GCM", iv }, deviceKey, ct),
    );
  } finally {
    db.close();
  }
}

/** Erase the cached DeviceGroup snapshot. No-op when absent.
 *  Called from `forgetDevice`. */
export async function deleteDeviceGroupSnapshot(
  accountIdB64: string,
): Promise<void> {
  const db = await openDb();
  try {
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE, "readwrite");
      tx.oncomplete = (): void => resolve();
      tx.onerror = (): void =>
        reject(tx.error ?? new Error("IDB delete tx error"));
      tx.objectStore(STORE).delete(accountIdB64);
    });
  } finally {
    db.close();
  }
}
