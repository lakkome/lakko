// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * IndexedDB-backed encrypted OPAQUE export-key cache
 * (Phase 2a2c2).
 *
 * The OPAQUE export key is the seed every identity-bearing
 * operation deterministically derives from:
 *
 *   - Ed25519 + X25519 keypairs via `Identity.fromSeed`
 *   - The device root key via `deriveDeviceRootKey`
 *   - The MLS ratchet state via OpenMLS's keypair-on-storage
 *
 * Until 2a2c2 we held it in memory only — every page reload
 * kicked the user back to the password form. With WebAuthn-
 * driven sign-in (2a2c2) we want the user to tap a passkey and
 * land in a working session, which requires the export key to
 * survive between sessions on a device that already saw it once.
 *
 * ## Threat model
 *
 * The cached export key is **the** seed for the account's
 * private material on this device. Leaking it = full identity
 * compromise for as long as the account exists (until the user
 * rotates via recovery phrase).
 *
 * We therefore wrap it under the **device root key** — the same
 * non-extractable AES-256-GCM `CryptoKey` that already protects
 * the MLS snapshot blob (1.16c3c2) and the session token
 * (1.13c). The device root key itself lives in IDB as a
 * non-extractable `CryptoKey`, so an attacker reading raw disk
 * gets ciphertext only.
 *
 * Compared to the session-token cache:
 *
 *   - Session token grants 30 days of authenticated writes;
 *     it's automatically rotated by every OPAQUE login.
 *   - Export key grants ALL operations indefinitely (until
 *     password rotation); only revocable by the recovery
 *     phrase.
 *
 * Caching the export key is a deliberate trade-off: it widens
 * the attack surface (device key compromise = full identity)
 * to unlock the sign-in-with-passkey UX. Mitigations:
 *
 *   - `forgetDevice` wipes the cache (Phase 1.16c3c).
 *   - The cache is per-account; multi-account future-proof.
 *   - A user paranoid about device key compromise can disable
 *     the passkey-only mode and stick to OPAQUE password login,
 *     which never persists the export key past the session.
 *
 * ## Schema
 *
 * ```text
 *   db   "lakko-export-keys"  v1
 *   └── store "keys"  keyPath: "accountId"  (string)
 *         row {
 *           accountId:  <b64-uuidv7>,
 *           ciphertext: Uint8Array,   // [12-byte IV | AES-GCM ciphertext+tag]
 *         }
 * ```
 *
 * The plaintext is the raw 64 bytes of the export key — no JSON
 * wrapper. Length is fixed by the OPAQUE ciphersuite
 * (`export-key-length.ts`), so a length-prefix is wasteful.
 */

import type { DeviceCipher } from "./device-cipher";
import { accountStoresWritable } from "./account-store-gate";
import {
  assertExportKeyLength,
  EXPORT_KEY_ENVELOPE_LEN,
} from "./export-key-length";

const DB_NAME = "lakko-export-keys";
const DB_VERSION = 1;
const STORE = "keys";

interface ExportKeyRecord {
  accountId: string;
  /** `[12-byte IV | AES-GCM ciphertext+tag]`. The plaintext is
   *  the raw 64-byte export-key bytes. */
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
 * Encrypt `exportKey` (64 bytes) under `deviceKey` and write the
 * resulting envelope to IDB keyed by `accountIdB64`. Overwrites
 * any prior row.
 *
 * Caller passes the raw 64 bytes; the function does not retain a
 * reference past the encrypt call. A key of any other length is
 * refused before anything is written (see `export-key-length.ts`
 * for why this check used to refuse every real key).
 */
export async function putExportKey(
  accountIdB64: string,
  exportKey: Uint8Array,
  deviceKey: DeviceCipher,
): Promise<void> {
  assertExportKeyLength(exportKey, "export key");
  const iv = crypto.getRandomValues(new Uint8Array(12));
  // `.slice()` narrows the buffer-type from `ArrayBufferLike` to
  // `ArrayBuffer` so `crypto.subtle.encrypt`'s `BufferSource`
  // parameter accepts it — TS 5.7+ rejects the wider type. Cheap
  // 64-byte copy.
  const ct = new Uint8Array(
    await deviceKey.encrypt(iv, exportKey),
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
      } satisfies ExportKeyRecord);
    });
  } finally {
    db.close();
  }
}

/**
 * Read + decrypt the persisted export key for `accountIdB64`.
 *
 * Returns:
 *   - `null` when no row exists (fresh device, post-`forgetDevice`,
 *     never-logged-in account, etc.).
 *   - the raw 64-byte export key otherwise.
 *
 * Throws on decrypt failure (device-key rotated, ciphertext
 * tampered, ciphertext truncated). Caller folds this into a
 * "passkey can't unlock this device — please use password or
 * recovery" UX rather than letting the error bubble up.
 */
export async function getExportKey(
  accountIdB64: string,
  deviceKey: DeviceCipher,
): Promise<Uint8Array | null> {
  const db = await openDb();
  try {
    const row: ExportKeyRecord | undefined = await new Promise(
      (resolve, reject) => {
        const tx = db.transaction(STORE, "readonly");
        const req = tx.objectStore(STORE).get(accountIdB64);
        req.onsuccess = (): void =>
          resolve(req.result as ExportKeyRecord | undefined);
        req.onerror = (): void =>
          reject(req.error ?? new Error("IDB get error"));
      },
    );
    if (row === undefined) return null;
    // Envelope: 12-byte IV || ciphertext+tag (16-byte tag for
    // AES-GCM). AES-GCM adds no padding, so a well-formed envelope
    // is exactly 12 + 64 + 16 bytes; anything shorter cannot hold a key.
    if (row.ciphertext.length < EXPORT_KEY_ENVELOPE_LEN) {
      throw new Error(
        `export-key envelope too short (${row.ciphertext.length} bytes)`,
      );
    }
    // `.slice()` over `.subarray()` so the resulting buffer is
    // typed as `ArrayBuffer`-backed (TS 5.7+ rejects the wider
    // `ArrayBufferLike` for `BufferSource`).
    const iv = row.ciphertext.slice(0, 12);
    const ct = row.ciphertext.slice(12);
    const plain = new Uint8Array(
      await deviceKey.decrypt(iv, ct),
    );
    assertExportKeyLength(plain, "decrypted export key");
    return plain;
  } finally {
    db.close();
  }
}

/**
 * Erase the cached export key for `accountIdB64`. No-op when
 * absent. Called from `forgetDevice` so the full-wipe contract
 * holds.
 */
export async function deleteExportKey(accountIdB64: string): Promise<void> {
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
