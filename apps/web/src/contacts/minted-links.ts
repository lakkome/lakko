// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What this device keeps about the invite links it minted, beyond the ledger
 * row: the link secret and the link's reach scope id.
 *
 * Since the 2026-09-28 security pass the ledger stores only the sealed blob,
 * and the link secret rides in the URL fragment the server never sees. The
 * owner's "Invite links" list is built from the ledger's owner query, so
 * without a local copy of the secret a listed link could not be copied or
 * shared again. The link id (ADR-0058 section 6) is what `revokeLink` needs to
 * remove exactly that link's reach key when the link is revoked.
 *
 * A link secret lets whoever holds it open the link, which is what the owner
 * hands out anyway; the row is still sealed under the device root key (the
 * posture of `reach/reach-store.ts`: AES-256-GCM, a fresh 12-byte IV per
 * write) so a copied profile directory does not carry live links in the clear.
 *
 * ```text
 *   db   "lakko-minted-links"  v1
 *   └── store "rows"  keyPath: "key"   ("<accountIdB64>|<kind>|<blobIdSlug>")
 *         row { key, ciphertext: Uint8Array  // [12-byte IV | AES-GCM ct+tag] }
 * ```
 *
 * Without a device key or without IndexedDB (the host tests) every read is
 * `null` and every write a no-op: a link minted there still works, it just
 * cannot be copied again from the list.
 */
import { getDeviceKey } from "../auth/key-store";
import { base64Decode, base64Encode } from "../auth/account-id";

const DB_NAME = "lakko-minted-links";
const DB_VERSION = 1;
const STORE = "rows";
const IV_LEN = 12;

/** Which ledger a minted link lives in. */
export type MintedLinkKind = "contact" | "call";

/** The local half of a minted link. */
export interface MintedLink {
  /** The 12-byte link secret (the URL fragment). */
  readonly linkSecret: Uint8Array;
  /** The link's 16-byte reach scope id. */
  readonly linkId: Uint8Array;
}

interface PersistedRow {
  key: string;
  ciphertext: Uint8Array;
}

function rowKey(accountIdB64: string, kind: MintedLinkKind, blobIdSlug: string): string {
  return `${accountIdB64}|${kind}|${blobIdSlug}`;
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
      if (!db.objectStoreNames.contains(STORE)) db.createObjectStore(STORE, { keyPath: "key" });
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

async function withKey(accountIdB64: string): Promise<CryptoKey | null> {
  try {
    return await getDeviceKey(accountIdB64);
  } catch {
    return null;
  }
}

/** Keep a minted link's secret and link id. Best effort: a failure is
 *  logged and the link still works, it just cannot be copied from the list
 *  again. */
export async function rememberMintedLink(
  accountIdB64: string,
  kind: MintedLinkKind,
  blobIdSlug: string,
  link: MintedLink,
): Promise<void> {
  const key = await withKey(accountIdB64);
  if (key === null) return;
  try {
    const plaintext = new TextEncoder().encode(
      JSON.stringify({ s: base64Encode(link.linkSecret), l: base64Encode(link.linkId) }),
    );
    const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
    const ct = new Uint8Array(await crypto.subtle.encrypt({ name: "AES-GCM", iv }, key, plaintext));
    const blob = new Uint8Array(IV_LEN + ct.length);
    blob.set(iv, 0);
    blob.set(ct, IV_LEN);
    const db = await openDb();
    try {
      const tx = db.transaction(STORE, "readwrite");
      const row: PersistedRow = { key: rowKey(accountIdB64, kind, blobIdSlug), ciphertext: blob };
      tx.objectStore(STORE).put(row);
      await awaitTx(tx);
    } finally {
      db.close();
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("minted-links: could not keep a minted link", err instanceof Error ? err.name : "");
  }
}

/** The local half of a minted link, or `null` when this device did not mint
 *  it (or cannot read its store). */
export async function mintedLinkFor(
  accountIdB64: string,
  kind: MintedLinkKind,
  blobIdSlug: string,
): Promise<MintedLink | null> {
  const key = await withKey(accountIdB64);
  if (key === null) return null;
  try {
    const db = await openDb();
    let row: PersistedRow | undefined;
    try {
      const tx = db.transaction(STORE, "readonly");
      const req = tx.objectStore(STORE).get(rowKey(accountIdB64, kind, blobIdSlug));
      row = await new Promise<PersistedRow | undefined>((resolve, reject) => {
        req.onsuccess = (): void => resolve(req.result as PersistedRow | undefined);
        req.onerror = (): void => reject(req.error ?? new Error("idb get failed"));
      });
    } finally {
      db.close();
    }
    if (row === undefined || row.ciphertext.length < IV_LEN + 16) return null;
    const plaintext = new Uint8Array(
      await crypto.subtle.decrypt(
        { name: "AES-GCM", iv: row.ciphertext.slice(0, IV_LEN) },
        key,
        row.ciphertext.slice(IV_LEN),
      ),
    );
    const parsed = JSON.parse(new TextDecoder().decode(plaintext)) as { s?: unknown; l?: unknown };
    if (typeof parsed.s !== "string" || typeof parsed.l !== "string") return null;
    return { linkSecret: base64Decode(parsed.s), linkId: base64Decode(parsed.l) };
  } catch {
    return null;
  }
}

/** Forget a minted link (after its revoke). Best effort. */
export async function forgetMintedLink(
  accountIdB64: string,
  kind: MintedLinkKind,
  blobIdSlug: string,
): Promise<void> {
  try {
    const db = await openDb();
    try {
      const tx = db.transaction(STORE, "readwrite");
      tx.objectStore(STORE).delete(rowKey(accountIdB64, kind, blobIdSlug));
      await awaitTx(tx);
    } finally {
      db.close();
    }
  } catch {
    /* nothing kept, nothing to forget */
  }
}
