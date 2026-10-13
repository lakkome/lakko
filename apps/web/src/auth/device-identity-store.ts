// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * IndexedDB-backed encrypted **DeviceIdentity seed** cache
 * (Phase 2a3b0, ADR-0007).
 *
 * The DeviceIdentity is a **per-device** random Ed25519 + X25519
 * keypair the user's device uses as its MLS leaf in the
 * `DeviceGroup` (2a3a) and — Phase 2b+ — in every conversation
 * group the user joins from this device. Distinct from the
 * `AccountIdentity` (deterministic from the OPAQUE export key),
 * which is shared across all of the user's devices and used for
 * the published identity bundle / KT log / recovery proofs.
 *
 * See `docs/adr/0007-multi-device-identity.md` for the full
 * rationale.
 *
 * ## What's stored
 *
 * A single 32-byte **seed**. The runtime DeviceIdentity is
 * rebuilt deterministically by
 * `wasm.Identity.fromSeed(seed)` — same shape as the existing
 * `Identity::fromSeed(exportKey)` path Phase 1 already uses,
 * just with random input bytes instead of the OPAQUE-derived
 * ones.
 *
 * ## Threat model
 *
 * Compared to the export-key cache (2a2c2):
 *
 *   - **Export key** leak = account-wide compromise (every
 *     device, every group, until the user rotates via recovery
 *     phrase).
 *   - **DeviceIdentity seed** leak = compromise of THIS device's
 *     leaf material only — the other devices' leaves stay
 *     intact, the AccountIdentity stays intact, only the local
 *     leaf needs to be surgically removed via DeviceGroup
 *     `Remove` and re-added (2a3b add-device flow).
 *
 * That scoped-blast-radius property is the whole point of the
 * two-tier identity (ADR-0007). The seed wraps under the
 * non-extractable AES-256-GCM device root key, same as every
 * other locally-cached per-account secret.
 *
 * ## Schema
 *
 * ```text
 *   db   "lakko-device-identity"  v1
 *   └── store "seeds"  keyPath: "accountId"  (string)
 *         row {
 *           accountId:  <b64-uuidv7>,
 *           ciphertext: Uint8Array,   // [12-byte IV | AES-GCM ciphertext+tag]
 *         }
 * ```
 *
 * Plaintext is the raw 32 bytes — fixed by the Ed25519 seed
 * contract, no length prefix needed.
 */

import { accountStoresWritable } from "./account-store-gate";

const DB_NAME = "lakko-device-identity";
const DB_VERSION = 1;
const STORE = "seeds";

/** Fixed seed length — pinned by `Identity::fromSeed`'s
 *  contract (32-byte Ed25519 seed). */
export const DEVICE_IDENTITY_SEED_LENGTH = 32;

interface DeviceIdentityRecord {
  accountId: string;
  /** `[12-byte IV | AES-GCM ciphertext+tag]`. The plaintext is
   *  the raw 32-byte DeviceIdentity seed. */
  ciphertext: Uint8Array;
  /** ADR-0050 device binding wire bytes (public: the account key's
   *  signature over this device's key), kept so `issued_at` stays stable
   *  across restarts. Absent until the device is first bound. */
  binding?: Uint8Array;
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

/**
 * Encrypt the raw 32-byte `seed` under `deviceKey` and write
 * the resulting envelope to IDB keyed by `accountIdB64`.
 * Overwrites any prior row.
 */
export async function putDeviceIdentitySeed(
  accountIdB64: string,
  seed: Uint8Array,
  deviceKey: CryptoKey,
): Promise<void> {
  if (seed.length !== DEVICE_IDENTITY_SEED_LENGTH) {
    throw new Error(
      `DeviceIdentity seed length ${seed.length} != ${DEVICE_IDENTITY_SEED_LENGTH}`,
    );
  }
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ct = new Uint8Array(
    await crypto.subtle.encrypt(
      { name: "AES-GCM", iv },
      deviceKey,
      seed.slice(),
    ),
  );
  const envelope = new Uint8Array(iv.length + ct.length);
  envelope.set(iv, 0);
  envelope.set(ct, iv.length);

  const db = await openDb();
  try {
    if (!accountStoresWritable(accountIdB64)) return;
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE, "readwrite");
      tx.oncomplete = (): void => resolve();
      tx.onerror = (): void =>
        reject(tx.error ?? new Error("IDB put tx error"));
      tx.objectStore(STORE).put({
        accountId: accountIdB64,
        ciphertext: envelope,
      } satisfies DeviceIdentityRecord);
    });
  } finally {
    db.close();
  }
}

/**
 * Read + decrypt the persisted DeviceIdentity seed for
 * `accountIdB64`. Returns `null` when no row exists (fresh
 * device, post-`forgetDevice`, never-seen account).
 *
 * Throws on decrypt failure (device key rotated, ciphertext
 * tampered, ciphertext truncated). Caller folds this into a
 * "re-generate" UX rather than letting the error bubble up —
 * the DeviceIdentity is a local per-device secret, so a
 * decrypt-failure path can just generate a fresh seed and
 * re-pair via QR add-device.
 */
export async function getDeviceIdentitySeed(
  accountIdB64: string,
  deviceKey: CryptoKey,
): Promise<Uint8Array | null> {
  const db = await openDb();
  try {
    const row: DeviceIdentityRecord | undefined = await new Promise(
      (resolve, reject) => {
        const tx = db.transaction(STORE, "readonly");
        const req = tx.objectStore(STORE).get(accountIdB64);
        req.onsuccess = (): void =>
          resolve(req.result as DeviceIdentityRecord | undefined);
        req.onerror = (): void =>
          reject(req.error ?? new Error("IDB get error"));
      },
    );
    if (row === undefined) return null;
    if (row.ciphertext.length < 12 + 16 + DEVICE_IDENTITY_SEED_LENGTH) {
      throw new Error(
        `device-identity envelope too short (${row.ciphertext.length} bytes)`,
      );
    }
    const iv = row.ciphertext.slice(0, 12);
    const ct = row.ciphertext.slice(12);
    const plain = new Uint8Array(
      await crypto.subtle.decrypt({ name: "AES-GCM", iv }, deviceKey, ct),
    );
    if (plain.length !== DEVICE_IDENTITY_SEED_LENGTH) {
      throw new Error(
        `decrypted DeviceIdentity seed length ${plain.length} != ${DEVICE_IDENTITY_SEED_LENGTH}`,
      );
    }
    return plain;
  } finally {
    db.close();
  }
}

/**
 * Erase the cached DeviceIdentity seed for `accountIdB64`.
 * No-op when absent. Called from `forgetDevice`.
 *
 * Note: deleting the seed alone does NOT remove the device's
 * leaf from the user's DeviceGroup — the other devices need to
 * see an MLS `Remove` commit to ratchet past the gone leaf.
 * That full revoke flow lives in `devices/remove.ts`
 * (`removeDeviceAt`, broadcast to the DeviceGroup DS journal);
 * this helper only wipes the local seed cache on `forgetDevice`.
 */
export async function deleteDeviceIdentitySeed(
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

/**
 * Convenience: read the cached seed if present, otherwise
 * generate a fresh random 32-byte seed and persist it. Returns
 * the seed bytes in both branches.
 *
 * Called from the signup / login / recover handlers as part of
 * the "make sure this device has a DeviceIdentity" hook.
 * Idempotent — same accountId + same deviceKey returns the
 * same bytes across calls.
 */
export async function ensureDeviceIdentitySeed(
  accountIdB64: string,
  deviceKey: CryptoKey,
): Promise<Uint8Array> {
  try {
    const existing = await getDeviceIdentitySeed(accountIdB64, deviceKey);
    if (existing !== null) return existing;
  } catch (err) {
    // Decrypt failed (device key rotated, tampered, …) — fall
    // through to generate a fresh seed. The previous device
    // identity becomes a leaf-in-DeviceGroup that nobody on
    // this device can sign for; 2a3c's remove-device flow
    // cleans it up later.
    // eslint-disable-next-line no-console
    console.warn("device-identity decrypt failed, regenerating", err);
  }
  const seed = crypto.getRandomValues(new Uint8Array(DEVICE_IDENTITY_SEED_LENGTH));
  await putDeviceIdentitySeed(accountIdB64, seed, deviceKey);
  return seed;
}

/** Read the persisted ADR-0050 device binding for `accountIdB64`, or
 *  `null` when the device has not been bound yet. */
export async function getDeviceBinding(
  accountIdB64: string,
): Promise<Uint8Array | null> {
  const db = await openDb();
  try {
    const row: DeviceIdentityRecord | undefined = await new Promise(
      (resolve, reject) => {
        const tx = db.transaction(STORE, "readonly");
        const req = tx.objectStore(STORE).get(accountIdB64);
        req.onsuccess = (): void =>
          resolve(req.result as DeviceIdentityRecord | undefined);
        req.onerror = (): void =>
          reject(req.error ?? new Error("IDB get failed"));
      },
    );
    return row?.binding ?? null;
  } finally {
    db.close();
  }
}

/** Persist the ADR-0050 device binding beside the seed. The seed row must
 *  already exist; the binding is public and stored as-is. */
export async function putDeviceBinding(
  accountIdB64: string,
  binding: Uint8Array,
): Promise<void> {
  const db = await openDb();
  try {
    if (!accountStoresWritable(accountIdB64)) return;
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE, "readwrite");
      const store = tx.objectStore(STORE);
      const get = store.get(accountIdB64);
      get.onsuccess = (): void => {
        const row = get.result as DeviceIdentityRecord | undefined;
        if (row === undefined) {
          reject(new Error("no DeviceIdentity seed row to bind"));
          return;
        }
        store.put({ ...row, binding });
      };
      get.onerror = (): void => reject(get.error ?? new Error("IDB get failed"));
      tx.oncomplete = (): void => resolve();
      tx.onerror = (): void => reject(tx.error ?? new Error("IDB put failed"));
    });
  } finally {
    db.close();
  }
}
