// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Where the anonymous-token pool keeps its tokens between launches
 * (ADR-0054 S5).
 *
 * A token is valid in the epoch it was issued for and the next one, so a
 * pool that survives a reload saves the account's daily allowance and the
 * wait for a refill on the first lookup after a restart. The pool loads its
 * rows when a session is installed, drops tokens whose epoch is dead, and
 * writes a context back after every take and every refill.
 *
 * ## Schema
 *
 * ```text
 *   db   "lakko-anon-tokens"  v1
 *   └── store "pools"  keyPath: ["account", "context"]
 *         row { account, context, tokens: [{ epoch: "<decimal>", headerValue }] }
 * ```
 *
 * One row per (account, context). `account` is the account part of the
 * session token (base64url of the account id), the same key the in-memory
 * pool tells accounts apart by.
 *
 * ## Handling
 *
 * The tokens are anonymous bearer credentials. They are never logged, and
 * they live only in this origin's IndexedDB: nothing here is part of a
 * backup, and nothing syncs them to another device. The pool clears the
 * whole store wherever it clears itself (sign-out, account switch, forget
 * device). They are not wrapped under the device key: a token names no
 * account and grants one anonymous request of a rationed kind, and the pool
 * holds no device key.
 */
import type { AnonTokenContext } from "./anon-token-pool";

/** One stored token. `epoch` is a decimal string (a bigint in memory). */
export interface PersistedAnonToken {
  readonly epoch: string;
  readonly headerValue: string;
}

/** Persistence the pool writes through. */
export interface AnonTokenStore {
  /** Every stored context of `account`. */
  load(account: string): Promise<Map<AnonTokenContext, PersistedAnonToken[]>>;
  /** Replace one (account, context) row; an empty list deletes it. */
  save(account: string, context: AnonTokenContext, tokens: readonly PersistedAnonToken[]): Promise<void>;
  /** Drop every row of every account. */
  clear(): Promise<void>;
}

const DB_NAME = "lakko-anon-tokens";
const DB_VERSION = 1;
const STORE = "pools";

interface Row {
  account: string;
  context: AnonTokenContext;
  tokens: PersistedAnonToken[];
}

function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB_NAME, DB_VERSION);
    req.onupgradeneeded = (): void => {
      const db = req.result;
      if (!db.objectStoreNames.contains(STORE)) {
        db.createObjectStore(STORE, { keyPath: ["account", "context"] });
      }
    };
    req.onsuccess = (): void => resolve(req.result);
    req.onerror = (): void => reject(req.error ?? new Error("indexedDB.open failed"));
  });
}

function wrap<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    req.onsuccess = (): void => resolve(req.result);
    req.onerror = (): void => reject(req.error ?? new Error("indexedDB request failed"));
  });
}

function done(tx: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = (): void => resolve();
    tx.onabort = (): void => reject(tx.error ?? new Error("indexedDB transaction aborted"));
    tx.onerror = (): void => reject(tx.error ?? new Error("indexedDB transaction failed"));
  });
}

function isPersistedToken(value: unknown): value is PersistedAnonToken {
  if (typeof value !== "object" || value === null) return false;
  const v = value as { epoch?: unknown; headerValue?: unknown };
  return typeof v.epoch === "string" && /^\d+$/.test(v.epoch) && typeof v.headerValue === "string";
}

/** The IndexedDB store. */
export function indexedDbAnonTokenStore(): AnonTokenStore {
  let db: Promise<IDBDatabase> | null = null;
  const open = (): Promise<IDBDatabase> => {
    if (db === null) {
      db = openDb();
      db.catch(() => {
        db = null;
      });
    }
    return db;
  };
  return {
    async load(account) {
      const conn = await open();
      const tx = conn.transaction(STORE, "readonly");
      const range = IDBKeyRange.bound([account, ""], [account, "￿"]);
      const rows = (await wrap(tx.objectStore(STORE).getAll(range))) as Row[];
      const out = new Map<AnonTokenContext, PersistedAnonToken[]>();
      for (const row of rows) {
        if (!Array.isArray(row.tokens)) continue;
        out.set(row.context, row.tokens.filter(isPersistedToken));
      }
      return out;
    },
    async save(account, context, tokens) {
      const conn = await open();
      const tx = conn.transaction(STORE, "readwrite");
      const store = tx.objectStore(STORE);
      if (tokens.length === 0) store.delete([account, context]);
      else store.put({ account, context, tokens: tokens.map((t) => ({ ...t })) } satisfies Row);
      await done(tx);
    },
    async clear() {
      const conn = await open();
      const tx = conn.transaction(STORE, "readwrite");
      tx.objectStore(STORE).clear();
      await done(tx);
    },
  };
}

/** The app's store: IndexedDB where the environment has it, else none (the
 *  pool then lives in memory only, as before). */
export function defaultAnonTokenStore(): AnonTokenStore | null {
  return typeof indexedDB === "undefined" ? null : indexedDbAnonTokenStore();
}

/** An in-memory store with the same contract (tests: a "relaunch" is a new
 *  pool over the same store). */
export function memoryAnonTokenStore(): AnonTokenStore & {
  readonly rows: Map<string, PersistedAnonToken[]>;
} {
  const rows = new Map<string, PersistedAnonToken[]>();
  const key = (account: string, context: string): string => `${account}\u0000${context}`;
  return {
    rows,
    async load(account) {
      const out = new Map<AnonTokenContext, PersistedAnonToken[]>();
      for (const [k, v] of rows) {
        const [a, c] = k.split("\u0000");
        if (a === account && c !== undefined) out.set(c as AnonTokenContext, v.map((t) => ({ ...t })));
      }
      return out;
    },
    async save(account, context, tokens) {
      if (tokens.length === 0) rows.delete(key(account, context));
      else rows.set(key(account, context), tokens.map((t) => ({ ...t })));
    },
    async clear() {
      rows.clear();
    },
  };
}
