// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Backup-head version-floor store (ADR-0037 §6, G-11).
 *
 * The monotonic rollback floor: the highest `backup_version` this device has
 * ever minted or restored, plus the envelope hash it committed. On a later
 * restore a **returning** client refuses any head below this floor outright
 * (hard, no freshness window) — the auth-assigned version never regresses, so
 * the floor is trustworthy against a storage-tier replay of an older head.
 *
 * ## Why device-key-encrypted (not plain localStorage)
 *
 * The floor is integrity-critical: an attacker who could lower it would defeat
 * the returning-client rollback defence. Sealing it under the non-extractable
 * AES-GCM device root key (`device-key.ts`) means a read/write-IDB attacker who
 * has not also broken the WebCrypto isolation boundary can neither read nor
 * tamper the floor (the AES-GCM tag authenticates it). Same substrate + threat
 * model as the recovery-phrase cache (`phrase-store.ts`).
 *
 * ## Wire shape (IDB row)
 *
 * - `accountId` (string) — base64 of the 16-byte UUIDv7 (the IDB primary key).
 * - `ciphertext` (Uint8Array) — `[iv(12) | aes-gcm(json_bytes, tag)]` where the
 *   sealed plaintext is the JSON `{ v: <decimal version>, h: <b64 hash> }`.
 * - `updatedAtMs` (number) — plaintext write time for cheap diagnostics.
 */

import type { DeviceCipher } from "../auth/device-cipher";
import { accountStoresWritable } from "../auth/account-store-gate";
import { base64Decode, base64Encode } from "../auth/account-id";

const DB_NAME = "lakko.backup.floor";
const DB_VERSION = 1;
const STORE = "floors";

/** Wire-level row shape stored in IDB. */
interface FloorRecord {
  accountId: string;
  ciphertext: Uint8Array;
  updatedAtMs: number;
}

/** The decrypted floor a returning client compares against. */
export interface BackupFloor {
  /** Highest `backup_version` this device has minted / restored. */
  readonly version: bigint;
  /** The 32-byte `BLAKE3` envelope hash the floor's head committed. */
  readonly envelopeHash: Uint8Array;
}

/**
 * The outcome of reading the floor (F5a). Distinguishes a genuinely absent
 * anchor (fresh device) from an anchor that EXISTS but is untrustworthy (a
 * device-key rotation or an attacker who tampered the IDB row). The restore
 * gate must never silently demote a corrupt anchor to the weaker fresh-install
 * path — it routes it to `stale-override` instead.
 */
export type FloorRead =
  /** No IDB entry — a first-ever restore on this device (zero-anchor path). */
  | { readonly kind: "absent" }
  /** A valid, decrypted floor. */
  | { readonly kind: "present"; readonly floor: BackupFloor }
  /** An IDB entry exists but AES-GCM decrypt / JSON parse / shape-check failed. */
  | { readonly kind: "corrupt" };

/** Sealed-plaintext JSON shape. `version` is a decimal string so a u64 never
 *  loses precision through JSON. */
interface FloorJson {
  readonly v: string;
  readonly h: string;
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
 * Persist (or advance) the version floor for `accountIdB64`, sealed under
 * `deviceKey`.
 *
 * **Monotone.** If a higher (or equal) floor is already stored, this is a no-op
 * — the floor never regresses, mirroring the auth-side counter. A deliberate
 * stale-override restore therefore never lowers a returning client's protection.
 */
export async function putFloor(
  accountIdB64: string,
  floor: BackupFloor,
  deviceKey: DeviceCipher,
): Promise<void> {
  // Never lower an existing (readable) floor. A corrupt / absent existing floor
  // is overwritten by this fresh, valid one — the write heals the anchor.
  const existing = await getFloor(accountIdB64, deviceKey);
  if (existing.kind === "present" && existing.floor.version >= floor.version) {
    return;
  }

  const json: FloorJson = {
    v: floor.version.toString(),
    h: base64Encode(floor.envelopeHash),
  };
  const plaintext = new TextEncoder().encode(JSON.stringify(json));
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ct = new Uint8Array(
    await deviceKey.encrypt(iv, plaintext),
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
        reject(tx.error ?? new Error("IDB floor put tx error"));
      tx.objectStore(STORE).put({
        accountId: accountIdB64,
        ciphertext: envelope,
        updatedAtMs: Date.now(),
      } satisfies FloorRecord);
    });
  } finally {
    db.close();
  }
}

/**
 * Read + decrypt the version floor for `accountIdB64`.
 *
 * Returns a discriminated [`FloorRead`] (F5a):
 * - `{ kind: "absent" }` — no IDB entry (fresh device — the zero-anchor path).
 * - `{ kind: "present", floor }` — a valid, decrypted floor.
 * - `{ kind: "corrupt" }` — an IDB entry EXISTS but AES-GCM decrypt / JSON parse
 *   / shape-check failed (a device-key rotation or an attacker who tampered the
 *   row). Distinct from "absent": the anchor once existed and is now
 *   untrustworthy, so the restore gate must NOT silently fall to the weaker
 *   freshness path — it routes a corrupt anchor to `stale-override`.
 *
 * Genuine absence stays fail-safe: a first-ever restore (no row) is `absent`,
 * never `corrupt`. Only a present-but-undecryptable row is `corrupt`.
 */
export async function getFloor(
  accountIdB64: string,
  deviceKey: DeviceCipher,
): Promise<FloorRead> {
  const db = await openDb();
  try {
    const row: FloorRecord | undefined = await new Promise((resolve, reject) => {
      const tx = db.transaction(STORE, "readonly");
      const req = tx.objectStore(STORE).get(accountIdB64);
      req.onsuccess = (): void =>
        resolve(req.result as FloorRecord | undefined);
      req.onerror = (): void =>
        reject(req.error ?? new Error("IDB floor get error"));
    });
    if (row === undefined) return { kind: "absent" };

    try {
      const iv = row.ciphertext.slice(0, 12);
      const ct = row.ciphertext.slice(12);
      const plaintext = new Uint8Array(
        await deviceKey.decrypt(iv, ct),
      );
      const json = JSON.parse(new TextDecoder().decode(plaintext)) as FloorJson;
      if (typeof json.v !== "string" || typeof json.h !== "string") {
        return { kind: "corrupt" };
      }
      return {
        kind: "present",
        floor: { version: BigInt(json.v), envelopeHash: base64Decode(json.h) },
      };
    } catch {
      // Entry exists but the wrap is broken (device-key rotation or tampering).
      // NOT absent — the anchor once existed, so surface it as corrupt so the
      // gate fails closed (stale-override) rather than demoting to fresh.
      return { kind: "corrupt" };
    }
  } finally {
    db.close();
  }
}

/**
 * Erase the floor for `accountIdB64`. No-op when absent — used by
 * `forgetDevice` cleanup alongside `deletePhrase`.
 */
export async function deleteFloor(accountIdB64: string): Promise<void> {
  const db = await openDb();
  try {
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE, "readwrite");
      tx.oncomplete = (): void => resolve();
      tx.onerror = (): void =>
        reject(tx.error ?? new Error("IDB floor delete tx error"));
      tx.objectStore(STORE).delete(accountIdB64);
    });
  } finally {
    db.close();
  }
}
