// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * IndexedDB-backed encrypted session-pair cache (Phase 1.13c; the pair
 * since ADR-0073).
 *
 * Every session landing of `lakko-svc-auth` returns a PAIR: a short-lived
 * access token (the bearer every service verifies statelessly, 15 minutes
 * by default) and a refresh credential only `POST /auth/session/refresh`
 * accepts (30 days, sliding), plus the key generation both were issued
 * under. The pair is persisted here, encrypted under the device root key, so
 * a returning user with an unchanged device-key chain resumes the session
 * without re-typing the password: a reload refreshes with the credential,
 * and an expired access token beside a live credential still resumes.
 *
 * ## Threat model
 *
 * The refresh credential grants what a 30-day session token granted before
 * ADR-0073: access to every bearer route for the account until it expires
 * or the account's keys are reset (which supersedes every credential of the
 * old generation at once). It grants NO key material: the MLS keys and the
 * AccountIdentity seed live under the device key in their own stores, and
 * nothing here can decrypt a message or sign on the person's behalf. The
 * envelope is wrapped under the device root key like every device-bound
 * store (`AES-256-GCM` / non-extractable `CryptoKey`, 1.16c3c2).
 *
 * ## Schema
 *
 * ```text
 *   db   "lakko-session-tokens"  v1
 *   ├── store "tokens"  keyPath: "accountId"  (string)
 *         row {
 *           accountId:        <b64-uuidv7>,
 *           ciphertext:       Uint8Array,   // [12-byte IV | AES-GCM ciphertext+tag]
 *           expiresAtMs:      number        // the REFRESH expiry, unencrypted for cheap eviction
 *         }
 * ```
 *
 * The plaintext is a small JSON blob `{ t, e, r, re, g }` (access token and
 * expiry, refresh credential and expiry, key generation as a decimal
 * string). `expiresAtMs` is duplicated outside the ciphertext so the load
 * path can drop a row whose credential is dead without the AES decrypt;
 * the plaintext carries the same expiry (an attacker who swapped the column
 * to a future value gains nothing: the server-side HMAC binds the real
 * one). Flag day: a row of the single-token shape reads as absent.
 */
import type { SessionPair } from "./session-pair";

import { accountSessionWritable } from "./account-store-gate";

const DB_NAME = "lakko-session-tokens";
const DB_VERSION = 1;
const STORE = "tokens";

/** Stored payload — opaque to anyone without the device key. */
interface TokenRecord {
  accountId: string;
  /** `[12-byte IV | AES-GCM ciphertext+tag]` over the JSON plaintext. */
  ciphertext: Uint8Array;
  /** Unencrypted refresh expiry, used for cheap eviction. */
  expiresAtMs: number;
}

/** The plaintext layout. */
interface Plain {
  t: string;
  e: number;
  r: string;
  re: number;
  g: string;
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
 * Encrypt `pair` under `deviceKey` and write the envelope keyed by
 * `accountIdB64`. Overwrites any prior row (a refresh replaces the pair).
 */
export async function putSessionPair(
  accountIdB64: string,
  pair: SessionPair,
  deviceKey: CryptoKey,
): Promise<void> {
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const plain: Plain = {
    t: pair.sessionToken,
    e: pair.sessionExpiresAtMs,
    r: pair.refreshToken,
    re: pair.refreshExpiresAtMs,
    g: pair.keyGeneration.toString(),
  };
  const plaintext = new TextEncoder().encode(JSON.stringify(plain));
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
        expiresAtMs: pair.refreshExpiresAtMs,
      } satisfies TokenRecord);
    });
  } finally {
    db.close();
  }
}

/**
 * Read + decrypt the persisted pair for `accountIdB64`.
 *
 * Returns `null` when no row exists, when the refresh credential has
 * expired (the row is dropped as part of the read), or when the row is of
 * a shape this build does not read. The ACCESS token may be expired: the
 * caller refreshes with the credential.
 *
 * Throws on a decrypt failure (device key rotated, ciphertext tampered) so
 * a device-key mismatch never silently continues.
 */
export async function getSessionPair(
  accountIdB64: string,
  deviceKey: CryptoKey,
): Promise<SessionPair | null> {
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

    if (row.expiresAtMs <= Date.now()) {
      void deleteSessionToken(accountIdB64);
      return null;
    }

    const iv = row.ciphertext.slice(0, 12);
    const ct = row.ciphertext.slice(12);
    const plaintext = new Uint8Array(
      await crypto.subtle.decrypt({ name: "AES-GCM", iv }, deviceKey, ct),
    );
    const decoded = JSON.parse(new TextDecoder().decode(plaintext)) as Partial<Plain>;
    if (
      typeof decoded.t !== "string" ||
      typeof decoded.e !== "number" ||
      typeof decoded.r !== "string" ||
      typeof decoded.re !== "number" ||
      typeof decoded.g !== "string" ||
      !/^[0-9]+$/.test(decoded.g)
    ) {
      // A row of the pre-ADR-0073 single-token shape: not a session this
      // build can resume. Dropped so the next landing writes the pair.
      void deleteSessionToken(accountIdB64);
      return null;
    }
    // Belt-and-suspenders: plaintext expiry beats the IDB column.
    if (decoded.re <= Date.now()) {
      void deleteSessionToken(accountIdB64);
      return null;
    }
    return {
      sessionToken: decoded.t,
      sessionExpiresAtMs: decoded.e,
      refreshToken: decoded.r,
      refreshExpiresAtMs: decoded.re,
      keyGeneration: BigInt(decoded.g),
    };
  } finally {
    db.close();
  }
}

/**
 * Erase the persisted pair for `accountIdB64`. No-op when the row is
 * absent — used by `logout()` (drop session) and the account wipe.
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
