// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 — IndexedDB backend for the received-profile store, encrypted under
 * the device root key.
 *
 * ## Schema
 *
 * ```text
 *   db   "lakko-profiles"  v1
 *   └── store "rows"  keyPath: "accountId"
 *         row { accountId: <b64>, ciphertext: Uint8Array  // [12-byte IV | AES-GCM ct+tag] }
 * ```
 *
 * One row per account, the same posture as the message log: names are
 * personal data, so they are never written to disk in the clear. The
 * plaintext is the JSON {@link ProfileRowPayload}.
 *
 * ## The service worker reads this row
 *
 * The background notification path (`public/sw-push-decrypt.js`) decrypts the
 * same row with the same device key to title a notification with a name it
 * already knows (UX spec §5). It only reads; it never writes (C-59). That is
 * also why the local renames are mirrored into the row: a service worker has
 * no `localStorage`, and without them the rename — which must win everywhere —
 * would lose to the profile name in exactly one place.
 */

import { accountStoresWritable } from "../auth/account-store-gate";
import { avatarFromJson } from "./avatar-model";
import {
  ACCOUNT_ID_HEX_RE,
  type ProfileBackend,
  type ProfileRow,
  type StoredProfile,
  type StoredVerification,
} from "./profile-store";

const DB_NAME = "lakko-profiles";
const DB_VERSION = 1;
const STORE = "rows";
const IV_LEN = 12;

/** Schema version of the JSON payload. */
export const PROFILE_ROW_VERSION = 1;

/** The JSON shape inside the ciphertext. Field names are short on purpose —
 *  the service worker mirror reads the same names. */
export interface ProfileRowPayload {
  readonly v: 1;
  /** keyB64 -> profile. `t` is the decimal `updated_at_ms` (a `u64` does not
   *  survive `JSON.parse` as a number). `a` is the claimed account id as 32
   *  lower-case hex characters, ABSENT when the profile carried none — rows
   *  written before the field existed read the same way, so they need no
   *  rewrite (the schema version stays 1: the field is additive). */
  readonly p: Record<
    string,
    { readonly d: string | null; readonly u: string | null; readonly t: string; readonly a?: string }
  >;
  /** `keyB64|username` -> verification. `s` is `verified` / `mismatch` /
   *  `error`, `c` the check time. */
  readonly vf: Record<string, { readonly s: string; readonly c: number }>;
  /** keyB64 -> local rename, mirrored from the contacts store for the service
   *  worker. */
  readonly r: Record<string, string>;
}

const VERIFICATION_STATES: ReadonlySet<string> = new Set(["verified", "mismatch", "error"]);

/** Encode a row (plus the rename mirror) to the payload JSON. */
export function encodeProfileRow(row: ProfileRow, renames: ReadonlyMap<string, string>): string {
  const p: Record<string, { d: string | null; u: string | null; t: string; a?: string; c?: string; av?: unknown }> = {};
  for (const [key, prof] of row.profiles) {
    p[key] = {
      d: prof.displayName,
      u: prof.username,
      t: prof.updatedAtMs.toString(),
      ...(prof.accountIdHex === null ? {} : { a: prof.accountIdHex }),
      ...(prof.usernameCertificateB64 === undefined || prof.usernameCertificateB64 === null
        ? {}
        : { c: prof.usernameCertificateB64 }),
      // ADR-0084 — the photo references (blob keys inside: this payload is
      // only ever written encrypted). Absent for no photo.
      ...(prof.avatar === undefined || prof.avatar === null ? {} : { av: prof.avatar }),
    };
  }
  const vf: Record<string, { s: string; c: number }> = {};
  for (const [key, v] of row.verifications) {
    vf[key] = { s: v.state, c: v.checkedAtMs };
  }
  const r: Record<string, string> = {};
  for (const [key, name] of renames) {
    if (name.trim().length > 0) r[key] = name;
  }
  const payload: ProfileRowPayload = { v: PROFILE_ROW_VERSION, p, vf, r };
  return JSON.stringify(payload);
}

/** Decode payload JSON, dropping any malformed entry rather than trusting it.
 *  `null` for a payload that is not this schema at all. */
export function decodeProfileRow(json: string): ProfileRow | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const obj = parsed as Partial<ProfileRowPayload>;
  if (obj.v !== PROFILE_ROW_VERSION) return null;
  const profiles = new Map<string, StoredProfile>();
  if (typeof obj.p === "object" && obj.p !== null) {
    for (const [key, raw] of Object.entries(obj.p)) {
      if (typeof raw !== "object" || raw === null) continue;
      const d = (raw as { d?: unknown }).d;
      const u = (raw as { u?: unknown }).u;
      const t = (raw as { t?: unknown }).t;
      if (d !== null && typeof d !== "string") continue;
      if (u !== null && typeof u !== "string") continue;
      if (typeof t !== "string" || !/^\d+$/.test(t)) continue;
      // A malformed `a` drops the FIELD, not the profile: the name is still
      // good, only the tag derivation has nothing to work from.
      const a = (raw as { a?: unknown }).a;
      const accountIdHex = typeof a === "string" && ACCOUNT_ID_HEX_RE.test(a) ? a : null;
      // ADR-0060 — the username certificate; a malformed one drops the field.
      const c = (raw as { c?: unknown }).c;
      const usernameCertificateB64 = typeof c === "string" && /^[A-Za-z0-9+/=]+$/.test(c) ? c : null;
      // ADR-0084 — a malformed photo drops the field, not the profile.
      const avatar = avatarFromJson((raw as { av?: unknown }).av);
      profiles.set(key, {
        displayName: d,
        username: u,
        updatedAtMs: BigInt(t),
        accountIdHex,
        usernameCertificateB64,
        ...(avatar === null ? {} : { avatar }),
      });
    }
  }
  const verifications = new Map<string, StoredVerification>();
  if (typeof obj.vf === "object" && obj.vf !== null) {
    for (const [key, raw] of Object.entries(obj.vf)) {
      if (typeof raw !== "object" || raw === null) continue;
      const s = (raw as { s?: unknown }).s;
      const c = (raw as { c?: unknown }).c;
      if (typeof s !== "string" || !VERIFICATION_STATES.has(s)) continue;
      if (typeof c !== "number" || !Number.isFinite(c)) continue;
      verifications.set(key, { state: s as StoredVerification["state"], checkedAtMs: c });
    }
  }
  return { profiles, verifications };
}

// ---------------------------------------------------------------------------
// IndexedDB + WebCrypto
// ---------------------------------------------------------------------------

interface PersistedRow {
  accountId: string;
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
    req.onerror = (): void => reject(req.error ?? new Error("indexedDB.open failed"));
  });
}

function awaitTx(tx: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = (): void => resolve();
    tx.onabort = (): void => reject(tx.error ?? new Error("idb tx aborted"));
    tx.onerror = (): void => reject(tx.error ?? new Error("idb tx errored"));
  });
}

async function encryptBlob(deviceKey: CryptoKey, plaintext: Uint8Array): Promise<Uint8Array> {
  const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
  const ct = new Uint8Array(
    await crypto.subtle.encrypt({ name: "AES-GCM", iv: iv.slice() }, deviceKey, plaintext.slice()),
  );
  const out = new Uint8Array(IV_LEN + ct.length);
  out.set(iv, 0);
  out.set(ct, IV_LEN);
  return out;
}

async function decryptBlob(deviceKey: CryptoKey, blob: Uint8Array): Promise<Uint8Array> {
  if (blob.length < IV_LEN + 16) throw new Error("encrypted profile row is too short");
  return new Uint8Array(
    await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: blob.slice(0, IV_LEN) },
      deviceKey,
      blob.slice(IV_LEN),
    ),
  );
}

/**
 * The production backend: one encrypted row per account.
 *
 * @param deviceKeyFor  the account's device root key, or `null` when this
 *                      device holds none (the store then stays in memory)
 * @param renamesFor    the local renames to mirror into the row
 */
export class IdbProfileBackend implements ProfileBackend {
  constructor(
    private readonly deviceKeyFor: (accountId: string) => Promise<CryptoKey | null>,
    private readonly renamesFor: () => ReadonlyMap<string, string>,
  ) {}

  async load(accountId: string): Promise<ProfileRow | null> {
    const deviceKey = await this.deviceKeyFor(accountId);
    if (deviceKey === null) return null;
    const db = await openDb();
    let row: PersistedRow | undefined;
    try {
      const tx = db.transaction(STORE, "readonly");
      const req = tx.objectStore(STORE).get(accountId);
      row = await new Promise<PersistedRow | undefined>((resolve, reject) => {
        req.onsuccess = (): void => resolve(req.result as PersistedRow | undefined);
        req.onerror = (): void => reject(req.error ?? new Error("idb get failed"));
      });
      await awaitTx(tx);
    } finally {
      db.close();
    }
    if (row === undefined) return null;
    try {
      const plain = await decryptBlob(deviceKey, row.ciphertext);
      return decodeProfileRow(new TextDecoder().decode(plain));
    } catch {
      // A row this key cannot open (the device key changed) is unusable, not
      // an error to surface: the store starts empty and rewrites it.
      return null;
    }
  }

  async save(accountId: string, row: ProfileRow): Promise<void> {
    const deviceKey = await this.deviceKeyFor(accountId);
    if (deviceKey === null) return;
    const json = encodeProfileRow(row, this.renamesFor());
    const ciphertext = await encryptBlob(deviceKey, new TextEncoder().encode(json));
    const db = await openDb();
    try {
      if (!accountStoresWritable(accountId)) return;
      const tx = db.transaction(STORE, "readwrite");
      tx.objectStore(STORE).put({ accountId, ciphertext } satisfies PersistedRow);
      await awaitTx(tx);
    } finally {
      db.close();
    }
  }
}

/** Erase the row for an account (forget device). No-op when absent. */
export async function deleteProfileRow(accountId: string): Promise<void> {
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).delete(accountId);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}
