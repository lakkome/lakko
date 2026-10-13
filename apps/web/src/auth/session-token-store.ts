// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * IndexedDB-backed encrypted session-token cache (Phase 1.13c).
 *
 * Phase 1.5b adds HMAC-SHA256 bearer tokens to every "finish"
 * endpoint (signup, login, recover, password-reset) on the auth
 * service; `apps/web` previously held them in-memory only,
 * so every page reload kicked the user back to the password form
 * for any protected `lakko-svc-discovery` write. This module
 * persists the token (encrypted with the device root key) so a
 * returning user with an unchanged device-key chain can re-attach
 * to their existing session without re-typing the password.
 *
 * ## Threat model
 *
 * The token is roughly as sensitive as the OPAQUE export key in
 * terms of what it grants:
 *
 *   - **Granted**: write access to `services/discovery` for this
 *     account (register, identity upload, keypackage upload,
 *     invite upload, invite revoke), until the embedded
 *     `exp` claim's 30-day TTL.
 *   - **NOT granted**: ability to decrypt messages (MLS keys
 *     live separately under the device key), ability to sign
 *     `ContactRequest` envelopes (those need the Ed25519 seed
 *     derived from the OPAQUE export key, which we do NOT
 *     persist), ability to read the DS inbox bytes (DS is
 *     pseudonym-keyed).
 *
 * So a leaked token is materially less damaging than a leaked
 * export key. Still: we wrap it under the device root key just
 * like the MLS snapshot blob — same `AES-256-GCM` /
 * non-extractable `CryptoKey` chain (1.16c3c2) the device-bound
 * material already uses.
 *
 * ## Schema
 *
 * ```text
 *   db   "lakko-session-tokens"  v1
 *   ├── store "tokens"  keyPath: "accountId"  (string)
 *         row {
 *           accountId:        <b64-uuidv7>,
 *           ciphertext:       Uint8Array,   // [12-byte IV | AES-GCM ciphertext+tag]
 *           expiresAtMs:      number        // surfaced unencrypted for cheap eviction
 *         }
 * ```
 *
 * `expiresAtMs` is duplicated outside the ciphertext so the load
 * path can drop stale rows without spending the AES decrypt cost.
 * The plaintext also carries it (defence in depth: an attacker
 * who swapped the row to a future expiry can't extend an actual
 * expired token because the embedded JWT-style HMAC binds the
 * server-side `exp`).
 */

import { accountSessionWritable } from "./account-store-gate";

const DB_NAME = "lakko-session-tokens";
const DB_VERSION = 1;
const STORE = "tokens";

/** Stored payload — opaque to anyone without the device key. */
interface TokenRecord {
  accountId: string;
  /** `[12-byte IV | AES-GCM ciphertext+tag]`. The plaintext is the
   *  UTF-8 bytes of the `session_token` string. */
  ciphertext: Uint8Array;
  /** Unencrypted expiry — server-stated, used for cheap eviction
   *  + the safety check on load. Plaintext also carries it. */
  expiresAtMs: number;
}

/** Decrypted output. Same shape the in-memory `IdentitySecrets`
 *  carries, minus the two byte fields the device-key chain takes
 *  care of separately. */
export interface PersistedSessionToken {
  sessionToken: string;
  sessionExpiresAtMs: number;
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
 * Encrypt `token` (with `expiresAtMs`) under `deviceKey` and write
 * the resulting envelope to IDB keyed by `accountIdB64`. Overwrites
 * any prior row.
 *
 * The plaintext layout is a tiny JSON blob `{ "t": <token>, "e":
 * <expiresAtMs> }`. We choose JSON over a fixed binary frame because
 * the inner JWT-style token has variable length and the row is
 * small (~256 bytes) — readability wins over a few bytes saved.
 */
export async function putSessionToken(
  accountIdB64: string,
  token: string,
  expiresAtMs: number,
  deviceKey: CryptoKey,
): Promise<void> {
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const plaintext = new TextEncoder().encode(
    JSON.stringify({ t: token, e: expiresAtMs }),
  );
  const ct = new Uint8Array(
    await crypto.subtle.encrypt({ name: "AES-GCM", iv }, deviceKey, plaintext),
  );
  const envelope = new Uint8Array(iv.length + ct.length);
  envelope.set(iv, 0);
  envelope.set(ct, iv.length);

  const db = await openDb();
  try {
    if (!accountSessionWritable(accountIdB64)) return;
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE, "readwrite");
      tx.oncomplete = (): void => resolve();
      tx.onerror = (): void =>
        reject(tx.error ?? new Error("IDB put tx error"));
      tx.objectStore(STORE).put({
        accountId: accountIdB64,
        ciphertext: envelope,
        expiresAtMs,
      } satisfies TokenRecord);
    });
  } finally {
    db.close();
  }
}

/**
 * Read + decrypt the persisted session token for `accountIdB64`.
 *
 * Returns:
 *   - `null` when no row exists (fresh device, post-logout, ...).
 *   - `null` when the unencrypted `expiresAtMs` is in the past
 *     (we silently drop the row as part of the read). Saves the
 *     AES decrypt cost on every stale-row hit + keeps the IDB
 *     clean across days/weeks of dormant sessions.
 *   - the decrypted `{ sessionToken, sessionExpiresAtMs }` pair
 *     otherwise.
 *
 * Throws on Decrypt failure (e.g. device-key rotated, ciphertext
 * tampered) — caller surfaces as "please re-enter password" so a
 * device-key mismatch doesn't silently allow continuation.
 */
export async function getSessionToken(
  accountIdB64: string,
  deviceKey: CryptoKey,
): Promise<PersistedSessionToken | null> {
  const db = await openDb();
  try {
    const row: TokenRecord | undefined = await new Promise(
      (resolve, reject) => {
        const tx = db.transaction(STORE, "readonly");
        const req = tx.objectStore(STORE).get(accountIdB64);
        req.onsuccess = (): void =>
          resolve(req.result as TokenRecord | undefined);
        req.onerror = (): void =>
          reject(req.error ?? new Error("IDB get error"));
      },
    );
    if (row === undefined) return null;

    // Cheap unencrypted expiry probe. A skewed clock could miss
    // the eviction; the plaintext carries the same field so the
    // post-decrypt safety check below catches that case.
    if (row.expiresAtMs <= Date.now()) {
      void deleteSessionToken(accountIdB64);
      return null;
    }

    const iv = row.ciphertext.slice(0, 12);
    const ct = row.ciphertext.slice(12);
    const plaintext = new Uint8Array(
      await crypto.subtle.decrypt(
        { name: "AES-GCM", iv },
        deviceKey,
        ct,
      ),
    );
    const decoded = JSON.parse(new TextDecoder().decode(plaintext)) as {
      t: string;
      e: number;
    };
    // Belt-and-suspenders: plaintext expiry beats the IDB column.
    if (decoded.e <= Date.now()) {
      void deleteSessionToken(accountIdB64);
      return null;
    }
    return {
      sessionToken: decoded.t,
      sessionExpiresAtMs: decoded.e,
    };
  } finally {
    db.close();
  }
}

/**
 * Erase the persisted token for `accountIdB64`. No-op when the
 * row is absent — used by both `logout()` (drop session) and
 * `forgetDevice()` (full wipe).
 */
export async function deleteSessionToken(accountIdB64: string): Promise<void> {
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
