// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * IndexedDB-backed encrypted **AccountIdentity seed** cache
 * (ADR-0017, Part 1 — phrase-seeded identity).
 *
 * The AccountIdentity is the **account-level** Ed25519 + X25519
 * keypair that is shared across all of the user's devices and is
 * published in the discovery identity bundle / KT log / recovery
 * proofs — the key contacts pin. Per ADR-0017 it is now derived
 * from the user's stable BIP39 **recovery-phrase seed**
 * (`RecoveryMnemonic.toSeed()`), NOT the password-derived OPAQUE
 * `exportKey`, so a routine recover / password-change preserves the
 * identity (the phrase does not change → the seed does not change →
 * the pubkey does not churn).
 *
 * This store is the parallel of `export-key-store.ts` /
 * `device-identity-store.ts`: it holds the BIP39 phrase seed wrapped
 * under the **same** non-extractable hardware-backed device root key
 * (`deriveDeviceRootKey(exportKey, accountId)`). The wrapping key
 * stays password-derived — only the wrapped *payload* changes —
 * preserving invariant #7 (hardware-backed device keys).
 *
 * ## What's stored
 *
 * A single **64-byte** seed. The runtime AccountIdentity is rebuilt
 * deterministically by `wasm.Identity.fromSeed(seed)` — the same
 * shape as the `Identity::fromSeed(exportKey)` path the clients used
 * before this ADR, just with the BIP39 PBKDF2 seed bytes instead of
 * the OPAQUE-derived ones. `Identity::fromSeed` HKDF-expands
 * arbitrary input bytes under a pinned salt, so it is seed-agnostic
 * (ADR-0017 "Scope" section) — no `lakko-mls` / Rust-core change.
 *
 * ## Threat model
 *
 * This is the **highest-sensitivity** on-device secret: leaking the
 * AccountIdentity seed = full account-identity compromise (it is the
 * recovery factor itself). It wraps under the non-extractable
 * AES-256-GCM device root key, same as the export key and the
 * DeviceIdentity seed, and is wiped on `forgetDevice` alongside the
 * others.
 *
 * Compared to the export-key cache: the export key can be rotated by
 * a password change; this seed only changes if the user changes
 * their recovery phrase (a deliberate "rotate my identity" action,
 * out of scope for Part 1).
 *
 * ## Schema
 *
 * ```text
 *   db   "lakko-account-identity-seed"  v1
 *   └── store "seeds"  keyPath: "accountId"  (string)
 *         row {
 *           accountId:  <b64-uuidv7>,
 *           ciphertext: Uint8Array,   // [12-byte IV | AES-GCM ciphertext+tag]
 *         }
 * ```
 *
 * Plaintext is the raw 64 bytes — fixed by the BIP39 PBKDF2 seed
 * length, no length prefix needed.
 */

import { accountStoresWritable } from "./account-store-gate";

const DB_NAME = "lakko-account-identity-seed";
const DB_VERSION = 1;
const STORE = "seeds";

/** Fixed seed length — pinned by the BIP39 PBKDF2 seed contract
 *  (`RecoveryMnemonic::toSeed` returns 64 bytes). */
export const ACCOUNT_IDENTITY_SEED_LENGTH = 64;

interface AccountIdentitySeedRecord {
  accountId: string;
  /** `[12-byte IV | AES-GCM ciphertext+tag]`. The plaintext is
   *  the raw 64-byte AccountIdentity (BIP39) seed. */
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

/**
 * Encrypt the raw 64-byte `seed` under `deviceKey` and write the
 * resulting envelope to IDB keyed by `accountIdB64`. Overwrites any
 * prior row.
 */
export async function putAccountIdentitySeed(
  accountIdB64: string,
  seed: Uint8Array,
  deviceKey: CryptoKey,
): Promise<void> {
  if (seed.length !== ACCOUNT_IDENTITY_SEED_LENGTH) {
    throw new Error(
      `AccountIdentity seed length ${seed.length} != ${ACCOUNT_IDENTITY_SEED_LENGTH}`,
    );
  }
  const iv = crypto.getRandomValues(new Uint8Array(12));
  // `.slice()` narrows the buffer-type from `ArrayBufferLike` to
  // `ArrayBuffer` so `crypto.subtle.encrypt`'s `BufferSource`
  // parameter accepts it (TS 5.7+ rejects the wider type).
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
      } satisfies AccountIdentitySeedRecord);
    });
  } finally {
    db.close();
  }
}

/**
 * Read + decrypt the persisted AccountIdentity seed for
 * `accountIdB64`. Returns `null` when no row exists (fresh device,
 * post-`forgetDevice`, never-seen account, or a device that logged
 * in with only a password before this seed was cached).
 *
 * Throws on decrypt failure (device key rotated, ciphertext
 * tampered, ciphertext truncated). The caller MUST NOT silently fall
 * back to deriving the identity from the export key or minting a
 * fresh identity on a `null` / throw — per ADR-0017 the fresh-device
 * fail-safe surfaces a "needs recovery phrase" state instead.
 */
export async function getAccountIdentitySeed(
  accountIdB64: string,
  deviceKey: CryptoKey,
): Promise<Uint8Array | null> {
  const db = await openDb();
  try {
    const row: AccountIdentitySeedRecord | undefined = await new Promise(
      (resolve, reject) => {
        const tx = db.transaction(STORE, "readonly");
        const req = tx.objectStore(STORE).get(accountIdB64);
        req.onsuccess = (): void =>
          resolve(req.result as AccountIdentitySeedRecord | undefined);
        req.onerror = (): void =>
          reject(req.error ?? new Error("IDB get error"));
      },
    );
    if (row === undefined) return null;
    // Envelope: 12-byte IV || ciphertext+tag (16-byte AES-GCM tag).
    // Minimum sensible size = 12 + 16 + 64 = 92.
    if (
      row.ciphertext.length <
      12 + 16 + ACCOUNT_IDENTITY_SEED_LENGTH
    ) {
      throw new Error(
        `account-identity-seed envelope too short (${row.ciphertext.length} bytes)`,
      );
    }
    const iv = row.ciphertext.slice(0, 12);
    const ct = row.ciphertext.slice(12);
    const plain = new Uint8Array(
      await crypto.subtle.decrypt({ name: "AES-GCM", iv }, deviceKey, ct),
    );
    if (plain.length !== ACCOUNT_IDENTITY_SEED_LENGTH) {
      throw new Error(
        `decrypted AccountIdentity seed length ${plain.length} != ${ACCOUNT_IDENTITY_SEED_LENGTH}`,
      );
    }
    return plain;
  } finally {
    db.close();
  }
}

/**
 * Erase the cached AccountIdentity seed for `accountIdB64`. No-op
 * when absent. Called from `forgetDevice` so the full-wipe contract
 * holds.
 */
export async function deleteAccountIdentitySeed(
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
