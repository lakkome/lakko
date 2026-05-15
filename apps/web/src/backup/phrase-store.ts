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
 * recovery phrase as UTF-8 bytes (24 words, NFKD-normalised
 * lowercase, single-space separated — same shape
 * `lakko-mnemonic` emits).
 */

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
 * Encrypt `phraseBytes` under `deviceKey` and persist to IDB.
 * Overwrites any prior cache for the same `accountIdB64`. Caller
 * is responsible for zeroing `phraseBytes` after this resolves.
 */
export async function putPhrase(
  accountIdB64: string,
  phraseBytes: Uint8Array,
  deviceKey: CryptoKey,
): Promise<void> {
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ct = new Uint8Array(
    await crypto.subtle.encrypt(
      { name: "AES-GCM", iv },
      deviceKey,
      phraseBytes.slice(),
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
        cachedAtMs: Date.now(),
      } satisfies PhraseRecord);
    });
  } finally {
    db.close();
  }
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
  deviceKey: CryptoKey,
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
      await crypto.subtle.decrypt({ name: "AES-GCM", iv }, deviceKey, ct),
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
