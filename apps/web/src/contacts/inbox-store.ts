// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The persisted half of the account mailbox: the DS cursor and the contact
 * requests still waiting for Accept or Decline, written together.
 *
 * Why it exists (seen live 2026-09-30): the pending list lived only in a Solid
 * signal while the cursor was persisted, so a poll consumed the request row,
 * the cursor was saved past it, and a reload lost the request for good. The
 * sender still saw it under "Sent". iOS closed the same class on 2026-09-16
 * (`ContactsInboxStore.swift`); Android has the same store.
 *
 * ## Shape
 *
 * One `localStorage` row per account, key `lakko.contacts.inbox.v1.<account>`:
 *
 *   `{ v: 1, cursor, pending: [{ messageId, payloadB64, createdAt }] }`
 *
 * `pending` holds the RAW mailbox rows, not decoded requests. A row's payload
 * is the envelope exactly as the DS served it: a signed ContactRequest whose
 * intro and profile are sealed to this account's X25519 key. So nothing here
 * is plaintext the network did not already carry; the decoded intro never
 * reaches `localStorage`. The clear parts of a request (the sender's public
 * key, account id and pseudonym) are the same class of data the accepted
 * contacts row next to it holds. On load the rows go back through the same
 * verify-and-open path the poller runs (`decodePendingRequestRows`).
 *
 * Why `localStorage` and not the device-key IndexedDB pattern of
 * `devices/sync/sync-state-store.ts`: the poller writes the cursor
 * synchronously after a dispatch, and the rule this store exists for is that
 * the cursor and the pending set land in ONE write. A synchronous single
 * `setItem` gives that directly; an async IDB write would reopen the window
 * between "cursor advanced in memory" and "row durable". The rows need no
 * second layer of encryption for the reason above.
 *
 * ## Rules
 *
 *   * The cursor never runs ahead of a persisted pending row: new rows are
 *     written (with the old cursor) during the dispatch, and a failed write
 *     throws, which holds the cursor in `mailbox-poller.ts`. A failed cursor
 *     write keeps the in-memory cursor where it was too, so the next poll
 *     re-fetches and the `messageId` dedup absorbs the repeat.
 *   * Accept and Decline remove the row. The cursor is already past it, so
 *     it is never fetched again.
 *   * A request from someone already in the accepted list is not added, and a
 *     restored one is dropped: covers a remove whose write failed, and a
 *     sender re-sending after being accepted.
 *   * Kept across sign-out (so is the cursor); dropped by forget-device.
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import type { IncomingContactRequest, MailboxRow } from "./client";

const KEY_PREFIX = "lakko.contacts.inbox.v1.";

/** One mailbox row as stored. */
export interface StoredMailboxRow {
  readonly messageId: number;
  readonly payloadB64: string;
  readonly createdAt: string;
}

/** What the store holds for one account. */
export interface PersistedInbox {
  readonly cursor: number;
  readonly pending: readonly StoredMailboxRow[];
}

export const EMPTY_INBOX: PersistedInbox = { cursor: 0, pending: [] };

type StorageLike = Pick<Storage, "getItem" | "setItem" | "removeItem" | "key" | "length">;

function defaultStorage(): StorageLike | null {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

function storageKey(accountIdB64: string): string {
  return KEY_PREFIX + accountIdB64;
}

function isStoredRow(r: unknown): r is StoredMailboxRow {
  return (
    typeof r === "object" &&
    r !== null &&
    typeof (r as StoredMailboxRow).messageId === "number" &&
    Number.isFinite((r as StoredMailboxRow).messageId) &&
    typeof (r as StoredMailboxRow).payloadB64 === "string" &&
    typeof (r as StoredMailboxRow).createdAt === "string"
  );
}

/** Read one account's inbox. Absent or malformed reads as empty (cursor 0:
 *  re-poll everything), never a throw. */
export function loadInbox(
  accountIdB64: string,
  storage: StorageLike | null = defaultStorage(),
): PersistedInbox {
  if (storage === null) return EMPTY_INBOX;
  let raw: string | null;
  try {
    raw = storage.getItem(storageKey(accountIdB64));
  } catch {
    return EMPTY_INBOX;
  }
  if (raw === null) return EMPTY_INBOX;
  try {
    const parsed = JSON.parse(raw) as { v?: unknown; cursor?: unknown; pending?: unknown };
    if (
      parsed.v === 1 &&
      typeof parsed.cursor === "number" &&
      Number.isFinite(parsed.cursor) &&
      parsed.cursor >= 0 &&
      Array.isArray(parsed.pending)
    ) {
      return { cursor: parsed.cursor, pending: parsed.pending.filter(isStoredRow) };
    }
  } catch {
    // malformed: fall through
  }
  return EMPTY_INBOX;
}

/** Write one account's inbox in a single `setItem`. THROWS when storage is
 *  unavailable or the write fails: the caller must not advance past it. */
export function saveInbox(
  accountIdB64: string,
  inbox: PersistedInbox,
  storage: StorageLike | null = defaultStorage(),
): void {
  if (storage === null) throw new Error("contacts inbox: no storage");
  storage.setItem(
    storageKey(accountIdB64),
    JSON.stringify({ v: 1, cursor: inbox.cursor, pending: inbox.pending }),
  );
}

/** Drop every account's inbox row (forget-device). */
export function clearAllInboxes(storage: StorageLike | null = defaultStorage()): void {
  if (storage === null) return;
  try {
    const keys: string[] = [];
    for (let i = 0; i < storage.length; i++) {
      const k = storage.key(i);
      if (k !== null && k.startsWith(KEY_PREFIX)) keys.push(k);
    }
    for (const k of keys) storage.removeItem(k);
  } catch {
    // best-effort, like the rest of the forget-device wipe
  }
}

export function toStoredRow(row: MailboxRow): StoredMailboxRow {
  return { messageId: row.messageId, payloadB64: base64Encode(row.payload), createdAt: row.createdAt };
}

export function fromStoredRow(row: StoredMailboxRow): MailboxRow {
  return { messageId: row.messageId, payload: base64Decode(row.payloadB64), createdAt: row.createdAt };
}

/** A freshly polled request with the row it came from. */
export interface FetchedRequest {
  readonly request: IncomingContactRequest;
  readonly row: MailboxRow;
}

export interface PendingInboxDeps {
  readonly accountIdB64: string;
  /** Whether a sender (base64 signature key) is already an accepted contact. */
  readonly isAccepted: (signaturePubkeyB64: string) => boolean;
  /** Called after every change to `pending()`. */
  readonly onChange: () => void;
  readonly storage?: StorageLike | null;
}

/**
 * One account's mailbox cursor + pending requests, in memory and on disk.
 * Framework-free so it can be tested without Solid; `MailboxContext` mirrors
 * `pending()` into a signal through `onChange`.
 */
export interface PendingInbox {
  cursor(): number;
  /** Pending requests in arrival order. A kept row whose decode has not
   *  finished yet (just after a reload) is not listed until it has. */
  pending(): readonly IncomingContactRequest[];
  /** Add freshly polled requests (deduped by `messageId` and against the
   *  accepted list) and persist them with the CURRENT cursor. Throws when the
   *  write fails, leaving memory unchanged, so the dispatch holds the cursor. */
  addFetched(fetched: readonly FetchedRequest[]): void;
  /** Persist `next` as the cursor together with the pending rows. A value not
   *  above the current cursor is ignored. Throws when the write fails, and the
   *  in-memory cursor stays where it was. */
  advanceCursor(next: number): void;
  /** Accept or Decline: drop the request for good. */
  remove(messageId: number): void;
  /** Rebuild the decoded requests for the rows kept on disk. Rows that do not
   *  decode, or whose sender is already accepted, are dropped. */
  restore(
    decode: (rows: readonly MailboxRow[]) => Promise<readonly (IncomingContactRequest | null)[]>,
  ): Promise<void>;
  /** Stop writing: a disposed inbox (signed out, account switched) must not
   *  overwrite what a newer one for the same account wrote. */
  dispose(): void;
}

export function createPendingInbox(deps: PendingInboxDeps): PendingInbox {
  const storage = deps.storage === undefined ? defaultStorage() : deps.storage;
  const loaded = loadInbox(deps.accountIdB64, storage);
  let cursor = loaded.cursor;
  let rows: StoredMailboxRow[] = [...loaded.pending];
  const decoded = new Map<number, IncomingContactRequest>();
  let disposed = false;

  function write(nextCursor: number, nextRows: readonly StoredMailboxRow[]): void {
    if (disposed) throw new Error("contacts inbox: disposed");
    saveInbox(deps.accountIdB64, { cursor: nextCursor, pending: nextRows }, storage);
  }

  function pending(): readonly IncomingContactRequest[] {
    const out: IncomingContactRequest[] = [];
    for (const r of rows) {
      const req = decoded.get(r.messageId);
      if (req !== undefined) out.push(req);
    }
    return out;
  }

  return {
    cursor: () => cursor,
    pending,
    addFetched(fetched) {
      const ids = new Set(rows.map((r) => r.messageId));
      const added: FetchedRequest[] = [];
      for (const f of fetched) {
        if (ids.has(f.request.messageId)) continue;
        if (deps.isAccepted(base64Encode(f.request.fromPubkey))) continue;
        ids.add(f.request.messageId);
        added.push(f);
      }
      if (added.length === 0) return;
      const nextRows = [...rows, ...added.map((f) => toStoredRow(f.row))];
      write(cursor, nextRows);
      rows = nextRows;
      for (const f of added) decoded.set(f.request.messageId, f.request);
      deps.onChange();
    },
    advanceCursor(next) {
      if (next <= cursor) return;
      write(next, rows);
      cursor = next;
    },
    remove(messageId) {
      const nextRows = rows.filter((r) => r.messageId !== messageId);
      const had = nextRows.length !== rows.length || decoded.has(messageId);
      rows = nextRows;
      decoded.delete(messageId);
      if (!had) return;
      try {
        write(cursor, rows);
      } catch (err) {
        // The request is gone from this session either way. An accepted one
        // cannot come back after a reload (the accepted-list rule drops it);
        // a declined one could, which is the lesser failure.
        // eslint-disable-next-line no-console
        console.warn("contacts inbox: could not persist removal", messageId, err);
      }
      deps.onChange();
    },
    async restore(decode) {
      const toDecode = rows.filter((r) => !decoded.has(r.messageId));
      if (toDecode.length === 0) return;
      const results = await decode(toDecode.map(fromStoredRow));
      if (disposed) return;
      const drop = new Set<number>();
      toDecode.forEach((row, i) => {
        const req = results[i] ?? null;
        if (req === null || deps.isAccepted(base64Encode(req.fromPubkey))) {
          drop.add(row.messageId);
        } else if (rows.some((r) => r.messageId === row.messageId)) {
          decoded.set(row.messageId, req);
        }
      });
      if (drop.size > 0) {
        rows = rows.filter((r) => !drop.has(r.messageId));
        try {
          write(cursor, rows);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("contacts inbox: could not persist dropped rows", err);
        }
      }
      deps.onChange();
    },
    dispose() {
      disposed = true;
    },
  };
}
