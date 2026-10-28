// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The intro text of the contact requests this device sent and has not seen
 * answered, per account, sealed under the device key.
 *
 * `sent-requests.ts` keeps who a request went to (the username and the
 * account id, which is what a re-send needs) in `localStorage` for the
 * synchronous "Sent" list. The intro is what the person wrote to a stranger,
 * so it does not go there: it lives here, AES-256-GCM under the device root
 * key, one row per account. With it a request re-sent after a rotation of
 * the account keys (ADR-0074 section 3.5a rule 5) carries the same words,
 * also after a reload.
 *
 * ```text
 *   db   "lakko-sent-request-intros"  v1
 *   └── store "rows"  keyPath: "accountId"
 *         row { accountId, ciphertext: Uint8Array  // [12-byte IV | ct+tag] }
 *         plaintext: JSON { v: 1, intros: { <recipient account id hex>: <text> } }
 * ```
 *
 * Dropped when the request is answered, when the person withdraws it, and by
 * the account wipe. Without a device key or without IndexedDB every read is
 * empty and every write a no-op: the re-send then carries no intro.
 */
import type { DeviceCipher } from "../auth/device-cipher";
import { accountStoresWritable } from "../auth/account-store-gate";
import { getDeviceKey } from "../auth/key-store";

const DB_NAME = "lakko-sent-request-intros";
const DB_VERSION = 1;
const STORE = "rows";
const IV_LEN = 12;

/** The longest intro kept, in UTF-16 units; a longer one is not stored. */
export const MAX_STORED_INTRO_CHARS = 2048;
/** The most intros kept per account; the oldest entries go first. */
export const MAX_STORED_INTROS = 256;

/** recipient account id (lower-case hex) -> intro text. */
export type SentIntros = Readonly<Record<string, string>>;

interface PersistedRow {
  accountId: string;
  ciphertext: Uint8Array;
}

/** Parse the sealed plaintext; anything else reads as no intros. Pure. */
export function decodeSentIntros(json: string): SentIntros {
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return {};
  }
  if (typeof parsed !== "object" || parsed === null) return {};
  const intros = (parsed as { intros?: unknown }).intros;
  if (typeof intros !== "object" || intros === null || Array.isArray(intros)) return {};
  const out: Record<string, string> = {};
  for (const [hex, text] of Object.entries(intros as Record<string, unknown>)) {
    if (!/^[0-9a-f]{32}$/.test(hex)) continue;
    if (typeof text !== "string" || text.length === 0 || text.length > MAX_STORED_INTRO_CHARS) continue;
    out[hex] = text;
  }
  return out;
}

export function encodeSentIntros(intros: SentIntros): string {
  return JSON.stringify({ v: 1, intros });
}

/** `intros` with the intro of one request set. An empty or over-long intro
 *  removes the entry (the request then re-sends without one). Pure. */
export function withSentIntro(intros: SentIntros, recipientAccountIdHex: string, intro: string): SentIntros {
  const hex = recipientAccountIdHex.toLowerCase();
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(intros)) if (k !== hex) out[k] = v;
  if (/^[0-9a-f]{32}$/.test(hex) && intro.length > 0 && intro.length <= MAX_STORED_INTRO_CHARS) {
    out[hex] = intro;
  }
  const keys = Object.keys(out);
  for (let i = 0; i < keys.length - MAX_STORED_INTROS; i += 1) {
    const oldest = keys[i];
    if (oldest !== undefined) delete out[oldest];
  }
  return out;
}

/** `intros` without one request's. Pure. */
export function withoutSentIntro(intros: SentIntros, recipientAccountIdHex: string): SentIntros {
  const hex = recipientAccountIdHex.toLowerCase();
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(intros)) if (k !== hex) out[k] = v;
  return out;
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
      if (!db.objectStoreNames.contains(STORE)) db.createObjectStore(STORE, { keyPath: "accountId" });
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

async function deviceKeyOf(accountIdB64: string): Promise<DeviceCipher | null> {
  try {
    return await getDeviceKey(accountIdB64);
  } catch {
    return null;
  }
}

async function read(accountIdB64: string, key: DeviceCipher): Promise<SentIntros> {
  const db = await openDb();
  let row: PersistedRow | undefined;
  try {
    const tx = db.transaction(STORE, "readonly");
    const req = tx.objectStore(STORE).get(accountIdB64);
    row = await new Promise<PersistedRow | undefined>((resolve, reject) => {
      req.onsuccess = (): void => resolve(req.result as PersistedRow | undefined);
      req.onerror = (): void => reject(req.error ?? new Error("idb get failed"));
    });
  } finally {
    db.close();
  }
  if (row === undefined || row.ciphertext.length < IV_LEN + 16) return {};
  const plaintext = new Uint8Array(
    await key.decrypt(row.ciphertext.slice(0, IV_LEN), row.ciphertext.slice(IV_LEN)),
  );
  return decodeSentIntros(new TextDecoder().decode(plaintext));
}

async function write(accountIdB64: string, key: DeviceCipher, intros: SentIntros): Promise<void> {
  const db = await openDb();
  try {
    if (Object.keys(intros).length === 0) {
      const tx = db.transaction(STORE, "readwrite");
      tx.objectStore(STORE).delete(accountIdB64);
      await awaitTx(tx);
      return;
    }
    const plaintext = new TextEncoder().encode(encodeSentIntros(intros));
    const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
    const ct = new Uint8Array(await key.encrypt(iv, plaintext));
    const blob = new Uint8Array(IV_LEN + ct.length);
    blob.set(iv, 0);
    blob.set(ct, IV_LEN);
    // A wiped account takes no late write (`auth/account-store-gate.ts`).
    if (!accountStoresWritable(accountIdB64)) return;
    const tx = db.transaction(STORE, "readwrite");
    const row: PersistedRow = { accountId: accountIdB64, ciphertext: blob };
    tx.objectStore(STORE).put(row);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

// One writer per account at a time on this page.
const queues = new Map<string, Promise<unknown>>();

function update(accountIdB64: string, change: (intros: SentIntros) => SentIntros): Promise<void> {
  const run = async (): Promise<void> => {
    const key = await deviceKeyOf(accountIdB64);
    if (key === null) return;
    try {
      let held: SentIntros = {};
      try {
        held = await read(accountIdB64, key);
      } catch {
        // A row that does not open is replaced rather than kept unreadable.
        held = {};
      }
      await write(accountIdB64, key, change(held));
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("sent-request-intros: write failed", err instanceof Error ? err.name : "");
    }
  };
  const queued = (queues.get(accountIdB64) ?? Promise.resolve()).then(run, run);
  queues.set(accountIdB64, queued);
  return queued;
}

/** Keep the intro of a request just sent. Best effort, never throws. */
export function rememberSentIntro(
  accountIdB64: string,
  recipientAccountIdHex: string,
  intro: string,
): Promise<void> {
  return update(accountIdB64, (held) => withSentIntro(held, recipientAccountIdHex, intro));
}

/** Drop a request's intro (answered, or withdrawn). Never throws. */
export function forgetSentIntro(accountIdB64: string, recipientAccountIdHex: string): Promise<void> {
  return update(accountIdB64, (held) => withoutSentIntro(held, recipientAccountIdHex));
}

/** The intro kept for a request, or `""`. Never throws. */
export async function sentIntroFor(accountIdB64: string, recipientAccountIdHex: string): Promise<string> {
  // Behind any write still in flight for the account.
  await (queues.get(accountIdB64) ?? Promise.resolve()).catch(() => undefined);
  const key = await deviceKeyOf(accountIdB64);
  if (key === null) return "";
  try {
    return (await read(accountIdB64, key))[recipientAccountIdHex.toLowerCase()] ?? "";
  } catch {
    return "";
  }
}

/** Drop every intro of the account (Forget device, Delete account). Throws
 *  when the store cannot be opened; the caller logs. */
export async function deleteSentIntrosForAccount(accountIdB64: string): Promise<void> {
  if (typeof indexedDB === "undefined") return;
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).delete(accountIdB64);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}
