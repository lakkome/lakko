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
 *   `{ v: 1, cursor, pending: [{ messageId, payloadB64, createdAt, link? }] }`
 *
 * `link` (ADR-0079, reworked 2026-10-17) is present only for a request that
 * answers one of this account's invite links: `"undecided"` while the device
 * cannot decide it yet (the synced link records are not loaded, or its claim
 * is in progress or won but not yet built) - the row is kept, not listed.
 * A request whose link does not count is refused and removed; it is never
 * listed (ADR-0079 amendment of 2026-10-19). The row keeps the time the DS
 * stamped on it (`createdAt`), which decides the link's expiry.
 *
 * `pending` holds the mailbox rows' request bodies, not decoded requests.
 * ADR-0078: a mailbox row is sealed to the account's mailbox key; the payload
 * kept here is the OPENED body, a signed ContactRequest whose intro and
 * profile are still sealed to this account's X25519 key. So nothing here
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

/** ADR-0079 - where a request answering one of this account's invite links
 *  stands. */
export type LinkRowState = "undecided";

/** One mailbox row as stored. */
export interface StoredMailboxRow {
  readonly messageId: number;
  readonly payloadB64: string;
  readonly createdAt: string;
  /** ADR-0079: absent for every request that answers no link. */
  readonly link?: LinkRowState;
  /** `docs/ux/who-can-find-me.md` section 6.4 - the group the request
   *  arrived inside (standard base64 of its id); absent for the inbox. */
  readonly viaGroupIdB64?: string;
}

/** What the store holds for one account. */
export interface PersistedInbox {
  readonly cursor: number;
  readonly pending: readonly StoredMailboxRow[];
  /** ADR-0073 R.7 case 1 / UX 16.6 — requests from accounts that are
   *  ALREADY contacts whose identity check had no verdict yet. Never listed;
   *  kept as raw rows like `pending`, because the mailbox cursor has moved
   *  past them and nothing else would bring them back after a reload. */
  readonly waiting?: readonly StoredMailboxRow[];
}

/** How many unshown waiting requests are kept. Chosen: a bound, not a
 *  measurement; past it a new one is not kept (the scheduled identity check
 *  still finds the person). */
export const MAX_WAITING_REQUESTS = 32;

/**
 * The waiting list after one more request is held: appended once (by
 * `messageId`), never past {@link MAX_WAITING_REQUESTS}. Pure.
 */
export function withWaiting(
  waiting: readonly StoredMailboxRow[],
  row: StoredMailboxRow,
): { readonly waiting: readonly StoredMailboxRow[]; readonly kept: boolean } {
  if (waiting.some((r) => r.messageId === row.messageId)) return { waiting, kept: true };
  if (waiting.length >= MAX_WAITING_REQUESTS) return { waiting, kept: false };
  return { waiting: [...waiting, row], kept: true };
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
  const link = (r as StoredMailboxRow | null)?.link;
  const via = (r as StoredMailboxRow | null)?.viaGroupIdB64;
  return (
    (via === undefined || typeof via === "string") &&
    typeof r === "object" &&
    r !== null &&
    typeof (r as StoredMailboxRow).messageId === "number" &&
    Number.isFinite((r as StoredMailboxRow).messageId) &&
    typeof (r as StoredMailboxRow).payloadB64 === "string" &&
    typeof (r as StoredMailboxRow).createdAt === "string" &&
    (link === undefined || link === "undecided")
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
      const rawWaiting = (parsed as { waiting?: unknown }).waiting;
      const waiting = Array.isArray(rawWaiting)
        ? rawWaiting.filter(isStoredRow).slice(0, MAX_WAITING_REQUESTS)
        : [];
      return {
        cursor: parsed.cursor,
        pending: parsed.pending.filter(isStoredRow),
        ...(waiting.length > 0 ? { waiting } : {}),
      };
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
    JSON.stringify({
      v: 1,
      cursor: inbox.cursor,
      pending: inbox.pending,
      ...(inbox.waiting !== undefined && inbox.waiting.length > 0 ? { waiting: inbox.waiting } : {}),
    }),
  );
}

/** Forget device: drop `accountIdB64`'s inbox row (cursor + pending list).
 *  Log out keeps it, so a re-login does not re-poll requests already
 *  declined; another account's row on this browser is left alone. */
export function clearInboxForAccount(
  accountIdB64: string,
  storage: StorageLike | null = defaultStorage(),
): void {
  if (storage === null) return;
  try {
    storage.removeItem(storageKey(accountIdB64));
  } catch {
    // best-effort, like the rest of the forget-device wipe
  }
}

/** Drop every account's inbox row. Not used by Forget device, which drops
 *  only the forgotten account's ({@link clearInboxForAccount}). */
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

export function toStoredRow(row: MailboxRow, link?: LinkRowState): StoredMailboxRow {
  return {
    messageId: row.messageId,
    payloadB64: base64Encode(row.payload),
    createdAt: row.createdAt,
    ...(link !== undefined ? { link } : {}),
    ...(row.viaGroupIdB64 !== undefined ? { viaGroupIdB64: row.viaGroupIdB64 } : {}),
  };
}

export function fromStoredRow(row: StoredMailboxRow): MailboxRow {
  return {
    messageId: row.messageId,
    payload: base64Decode(row.payloadB64),
    createdAt: row.createdAt,
    ...(row.viaGroupIdB64 !== undefined ? { viaGroupIdB64: row.viaGroupIdB64 } : {}),
  };
}

/** A freshly polled request with the row it came from. */
export interface FetchedRequest {
  readonly request: IncomingContactRequest;
  readonly row: MailboxRow;
  /** ADR-0074 section 3.5a rule 5 - the pending requests this one REPLACES:
   *  the same person's, sent from a key this request's key provably
   *  superseded. Removed in the same write, so the list shows one request
   *  per person and never a second row. */
  readonly replaces?: readonly number[];
  /** ADR-0079 - the request answers one of this account's invite links and
   *  is kept undecided (not listed) until the device decides it. */
  readonly undecidedLink?: boolean;
}

/** One pending request as the one-per-person rule reads it. */
export interface PendingKey {
  readonly messageId: number;
  readonly keyB64: string;
}

/**
 * ADR-0074 section 3.5a rule 5 - what a request arriving from `incomingKeyB64`
 * does to the pending list: it REPLACES every pending request whose key it
 * provably superseded, and is itself DROPPED when a pending request already
 * comes from a key that superseded its own (a stale request never puts the
 * old key back). `isSuccessor(old, new)` answers whether `new` is the proven
 * successor of `old`. Pure.
 */
export function pendingAfterArrival(
  pending: readonly PendingKey[],
  incomingKeyB64: string,
  isSuccessor: (oldKeyB64: string, newKeyB64: string) => boolean,
): { readonly replaces: number[]; readonly dropIncoming: boolean } {
  const replaces: number[] = [];
  let dropIncoming = false;
  for (const p of pending) {
    if (p.keyB64 === incomingKeyB64) continue;
    if (isSuccessor(p.keyB64, incomingKeyB64)) replaces.push(p.messageId);
    else if (isSuccessor(incomingKeyB64, p.keyB64)) dropIncoming = true;
  }
  return dropIncoming ? { replaces: [], dropIncoming } : { replaces, dropIncoming };
}

/**
 * One card per person (2026-11-23, seen live as two identical cards): the
 * message ids of the listed requests a later request under the same signature
 * key supersedes. `held` are listed already, `incoming` arrive in order after
 * them; of each key only the last request survives. Pure.
 */
export function laterRequestSupersedes(
  held: readonly Pick<IncomingContactRequest, "messageId" | "fromPubkey">[],
  incoming: readonly Pick<IncomingContactRequest, "messageId" | "fromPubkey">[],
): Set<number> {
  const last = new Map<string, number>();
  for (const r of [...held, ...incoming]) last.set(base64Encode(r.fromPubkey), r.messageId);
  const out = new Set<number>();
  for (const r of [...held, ...incoming]) {
    if (last.get(base64Encode(r.fromPubkey)) !== r.messageId) out.add(r.messageId);
  }
  return out;
}

export interface PendingInboxDeps {
  readonly accountIdB64: string;
  /** Whether a sender (base64 signature key) is already an accepted contact. */
  readonly isAccepted: (signaturePubkeyB64: string) => boolean;
  /** `docs/ux/who-can-find-me.md` section 6.4 - a request that is kept
   *  although its sender is already a contact: one that came inside a group
   *  from a contact this device holds no one-to-one with (a contact first met
   *  in a group). Absent: none is. */
  readonly keepsFromContact?: (request: IncomingContactRequest) => boolean;
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
   *  finished yet (just after a reload) is not listed until it has, nor is
   *  an undecided link request (ADR-0079). */
  pending(): readonly IncomingContactRequest[];
  /** ADR-0079 - the decoded requests that answer one of this account's
   *  invite links and are not decided yet, in arrival order. */
  undecidedLinkRequests(): readonly IncomingContactRequest[];
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
  /** Keep an UNSHOWN request of an existing contact that has no verdict yet
   *  (UX 16.6), persisted with the current cursor. Returns whether it is
   *  kept (`false` past the bound, or when the write failed). */
  holdWaiting(row: MailboxRow): boolean;
  /** The waiting rows kept on disk, in arrival order. */
  waitingRows(): readonly MailboxRow[];
  /** A waiting request got its verdict (answered, dropped, or shown). */
  dropWaiting(messageId: number): void;
  /** Stop writing: a disposed inbox (signed out, account switched) must not
   *  overwrite what a newer one for the same account wrote. */
  dispose(): void;
}

export function createPendingInbox(deps: PendingInboxDeps): PendingInbox {
  const storage = deps.storage === undefined ? defaultStorage() : deps.storage;
  const loaded = loadInbox(deps.accountIdB64, storage);
  let cursor = loaded.cursor;
  let rows: StoredMailboxRow[] = [...loaded.pending];
  let waiting: readonly StoredMailboxRow[] = [...(loaded.waiting ?? [])];
  const decoded = new Map<number, IncomingContactRequest>();
  let disposed = false;

  function write(nextCursor: number, nextRows: readonly StoredMailboxRow[]): void {
    if (disposed) throw new Error("contacts inbox: disposed");
    saveInbox(deps.accountIdB64, { cursor: nextCursor, pending: nextRows, waiting }, storage);
  }

  function pending(): readonly IncomingContactRequest[] {
    const out: IncomingContactRequest[] = [];
    for (const r of rows) {
      if (r.link === "undecided") continue;
      const req = decoded.get(r.messageId);
      if (req === undefined) continue;
      out.push(req);
    }
    return out;
  }

  function withLink(link: LinkRowState): readonly IncomingContactRequest[] {
    const out: IncomingContactRequest[] = [];
    for (const r of rows) {
      if (r.link !== link) continue;
      const req = decoded.get(r.messageId);
      if (req !== undefined) out.push(req);
    }
    return out;
  }

  return {
    cursor: () => cursor,
    pending,
    undecidedLinkRequests: () => withLink("undecided"),
    addFetched(fetched) {
      const ids = new Set(rows.map((r) => r.messageId));
      const added: FetchedRequest[] = [];
      for (const f of fetched) {
        if (ids.has(f.request.messageId)) continue;
        if (deps.isAccepted(base64Encode(f.request.fromPubkey)) && deps.keepsFromContact?.(f.request) !== true) {
          continue;
        }
        ids.add(f.request.messageId);
        added.push(f);
      }
      if (added.length === 0) return;
      // Rule 5: a request from the proven successor of a pending request's
      // key replaces it, in this same write.
      const replaced = new Set<number>();
      for (const f of added) for (const id of f.replaces ?? []) replaced.add(id);
      for (const f of added) replaced.delete(f.request.messageId);
      // One card per person (2026-11-23): a later listed request under the
      // same key replaces an earlier one, held or in this batch.
      const superseded = laterRequestSupersedes(
        rows.flatMap((r) => {
          const req = decoded.get(r.messageId);
          return r.link === "undecided" || req === undefined ? [] : [req];
        }),
        added.filter((f) => f.undecidedLink !== true).map((f) => f.request),
      );
      for (const id of superseded) replaced.add(id);
      const kept = added.filter((f) => !superseded.has(f.request.messageId));
      const nextRows = [
        ...rows.filter((r) => !replaced.has(r.messageId)),
        ...kept.map((f) => toStoredRow(f.row, f.undecidedLink === true ? "undecided" : undefined)),
      ];
      write(cursor, nextRows);
      rows = nextRows;
      for (const id of replaced) decoded.delete(id);
      for (const f of kept) decoded.set(f.request.messageId, f.request);
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
      const restored: IncomingContactRequest[] = [];
      toDecode.forEach((row, i) => {
        const req = results[i] ?? null;
        if (
          req === null ||
          (deps.isAccepted(base64Encode(req.fromPubkey)) && deps.keepsFromContact?.(req) !== true)
        ) {
          drop.add(row.messageId);
        } else if (rows.some((r) => r.messageId === row.messageId)) {
          decoded.set(row.messageId, req);
          if (row.link !== "undecided") restored.push(req);
        }
      });
      // One card per person: rows kept from before the fold read as one.
      const listed = rows.flatMap((r) => {
        const req = decoded.get(r.messageId);
        return r.link === "undecided" || req === undefined || restored.includes(req) ? [] : [req];
      });
      for (const id of laterRequestSupersedes(listed, restored)) {
        drop.add(id);
        decoded.delete(id);
      }
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
    holdWaiting(row) {
      const next = withWaiting(waiting, toStoredRow(row));
      if (!next.kept || next.waiting === waiting) return next.kept;
      const before = waiting;
      waiting = next.waiting;
      try {
        write(cursor, rows);
      } catch (err) {
        waiting = before;
        // eslint-disable-next-line no-console
        console.warn("contacts inbox: could not persist a waiting request", err);
        return false;
      }
      return true;
    },
    waitingRows: () => waiting.map(fromStoredRow),
    dropWaiting(messageId) {
      if (!waiting.some((r) => r.messageId === messageId)) return;
      waiting = waiting.filter((r) => r.messageId !== messageId);
      try {
        write(cursor, rows);
      } catch (err) {
        // It is judged again after a reload, and reaches the same verdict.
        // eslint-disable-next-line no-console
        console.warn("contacts inbox: could not persist a waiting removal", messageId, err);
      }
    },
    dispose() {
      disposed = true;
    },
  };
}
