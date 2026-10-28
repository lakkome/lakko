// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Encrypted recovery-phrase cache for the auto-backup flow
 * (Phase 2a1c).
 *
 * ## Why cache the phrase at all
 *
 * The Phase 2a1 backup envelope is keyed by an Argon2id-derived
 * KEK from the BIP39 recovery phrase. For backup to fire
 * automatically (the only way the user actually gets the safety
 * benefit), the client needs the phrase at every backup time —
 * but the phrase is only on screen at signup or recovery.
 *
 * We store the phrase locally, encrypted under the device root
 * key. The device root key is a non-extractable AES-GCM
 * `CryptoKey` (`device-key.ts`) — its bytes never leave the
 * WebCrypto sandbox, so an attacker with read-only access to
 * IndexedDB cannot recover the phrase without also breaking
 * the WebCrypto isolation boundary.
 *
 * ## Threat model
 *
 * - **Phrase confidentiality vs. plaintext-on-disk**: the phrase
 *   never lands as plaintext on disk. The envelope here is AEAD
 *   ciphertext + AES-GCM authentication tag, keyed by the
 *   non-extractable device-root key.
 * - **Phrase confidentiality vs. memory inspection**: the phrase
 *   bytes appear briefly in JS heap during encrypt / decrypt
 *   operations. Same threat profile as the OPAQUE export-key
 *   bytes in `AuthContext` — both fundamental to the design,
 *   both documented.
 * - **Recovery on a different device**: phrase cache is local
 *   per device. A new device starts without the cache and
 *   cannot auto-backup until the user goes through the recovery
 *   flow (which is when the phrase becomes available again).
 *   This is the intended boundary.
 * - **Forget-device**: [`deletePhrase`] wipes the cache. Called
 *   from the existing `forgetDevice` path so the same gesture
 *   that drops the session token also drops the cached phrase.
 *
 * ## Wire shape (IDB row)
 *
 * - `accountId` (string) — base64 of the 16-byte UUIDv7, used
 *   as the IDB primary key.
 * - `ciphertext` (Uint8Array) — `[iv(12) | aes-gcm(phrase_bytes, tag)]`.
 * - `cachedAtMs` (number) — wall-clock when the cache was last
 *   written; carried as a plaintext column for cheap staleness
 *   probes.
 *
 * The plaintext sealed inside `ciphertext` is the canonical
 * recovery phrase as UTF-8 bytes (12 words, NFKD-normalised
 * lowercase, single-space separated — same shape
 * `lakko-mnemonic` emits).
 */

import type { DeviceCipher } from "../auth/device-cipher";
import {
  accountStoresWritable,
  accountStoresWritableSince,
  type WipeMark,
} from "../auth/account-store-gate";

const DB_NAME = "lakko.backup.phrase";
const DB_VERSION = 1;
const STORE = "phrases";

/** Wire-level row shape stored in IDB. */
interface PhraseRecord {
  accountId: string;
  ciphertext: Uint8Array;
  cachedAtMs: number;
}

/**
 * Public read view of a successfully-decrypted phrase cache.
 * `phrase` is the canonical UTF-8 phrase bytes (NOT the string —
 * the caller hands it straight to `wasm.sealBackup` which
 * expects bytes).
 */
export interface PersistedPhrase {
  phrase: Uint8Array;
  cachedAtMs: number;
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
 * A stored row as it is on disk (the ciphertext, never opened): what a
 * staged write keeps so it can put the row back exactly
 * (`device-phrase.ts`, `staged-phrase.ts`).
 */
export interface PhraseRowSnapshot {
  readonly ciphertext: Uint8Array;
  readonly cachedAtMs: number;
}

/** Whether a write of `accountIdB64`'s row may be issued now: the plain
 *  store gate, or (under `wipes`) only "not wiped since the work began". */
function writable(accountIdB64: string, wipes: WipeMark | undefined): boolean {
  return wipes === undefined
    ? accountStoresWritable(accountIdB64)
    : accountStoresWritableSince(wipes, accountIdB64);
}

/**
 * Encrypt `phraseBytes` under `deviceKey` and persist to IDB.
 * Overwrites any prior cache for the same `accountIdB64`. Caller
 * is responsible for zeroing `phraseBytes` after this resolves.
 *
 * Resolves `false` when the store gate refused the write (the account's
 * local data was wiped; under `wipes`, wiped since that mark was taken), and
 * `true` once the row is written. Throws when the write fails.
 */
export async function putPhrase(
  accountIdB64: string,
  phraseBytes: Uint8Array,
  deviceKey: DeviceCipher,
  wipes?: WipeMark,
): Promise<boolean> {
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ct = new Uint8Array(
    await deviceKey.encrypt(iv, phraseBytes),
  );
  const envelope = new Uint8Array(iv.length + ct.length);
  envelope.set(iv, 0);
  envelope.set(ct, iv.length);

  return writeRecord(
    { accountId: accountIdB64, ciphertext: envelope, cachedAtMs: Date.now() },
    wipes,
  );
}

async function writeRecord(record: PhraseRecord, wipes: WipeMark | undefined): Promise<boolean> {
  const db = await openDb();
  try {
    if (!writable(record.accountId, wipes)) return false;
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE, "readwrite");
      tx.oncomplete = (): void => resolve();
      tx.onerror = (): void =>
        reject(tx.error ?? new Error("IDB put tx error"));
      tx.objectStore(STORE).put(record);
    });
    return true;
  } finally {
    db.close();
  }
}

/**
 * The row stored for `accountIdB64` as it is on disk, or `null` when there is
 * none. Nothing is decrypted, so a row under another device key reads too.
 * Throws when the store cannot be read.
 */
export async function getPhraseRow(accountIdB64: string): Promise<PhraseRowSnapshot | null> {
  const db = await openDb();
  try {
    const row: PhraseRecord | undefined = await new Promise((resolve, reject) => {
      const tx = db.transaction(STORE, "readonly");
      const req = tx.objectStore(STORE).get(accountIdB64);
      req.onsuccess = (): void => resolve(req.result as PhraseRecord | undefined);
      req.onerror = (): void => reject(req.error ?? new Error("IDB get error"));
    });
    if (row === undefined) return null;
    return { ciphertext: row.ciphertext.slice(), cachedAtMs: row.cachedAtMs };
  } finally {
    db.close();
  }
}

/** Put back a row {@link getPhraseRow} read. `false` when the gate refused
 *  it, as {@link putPhrase}. */
export function putPhraseRow(
  accountIdB64: string,
  row: PhraseRowSnapshot,
  wipes?: WipeMark,
): Promise<boolean> {
  return writeRecord(
    { accountId: accountIdB64, ciphertext: row.ciphertext.slice(), cachedAtMs: row.cachedAtMs },
    wipes,
  );
}

/**
 * Read + decrypt the cached phrase for `accountIdB64`.
 *
 * Returns:
 *   - `null` when no row exists (fresh device / post-forget).
 *   - the decrypted `{ phrase, cachedAtMs }` otherwise.
 *
 * Throws on decrypt failure — typically a device-key rotation
 * that broke the wrap. The caller surfaces as "please go through
 * recovery to re-enable backups on this device".
 */
export async function getPhrase(
  accountIdB64: string,
  deviceKey: DeviceCipher,
): Promise<PersistedPhrase | null> {
  const db = await openDb();
  try {
    const row: PhraseRecord | undefined = await new Promise((resolve, reject) => {
      const tx = db.transaction(STORE, "readonly");
      const req = tx.objectStore(STORE).get(accountIdB64);
      req.onsuccess = (): void => resolve(req.result as PhraseRecord | undefined);
      req.onerror = (): void => reject(req.error ?? new Error("IDB get error"));
    });
    if (row === undefined) return null;

    const iv = row.ciphertext.slice(0, 12);
    const ct = row.ciphertext.slice(12);
    const plaintext = new Uint8Array(
      await deviceKey.decrypt(iv, ct),
    );
    return { phrase: plaintext, cachedAtMs: row.cachedAtMs };
  } finally {
    db.close();
  }
}

/**
 * Erase the cached phrase for `accountIdB64`. No-op when the
 * row is absent — used by `forgetDevice` / `logout` cleanup.
 */
export async function deletePhrase(accountIdB64: string): Promise<void> {
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
