// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 phase 2 (UX 16.5 / 16.8) — the per-`(account, group)` CLOSED
 * state a received group-closed envelope sets on this device, and the
 * RECEIVE-SIDE decision that guards it.
 *
 * The twin of `group-name-store.ts`, and deliberately the same shape (the
 * brief: "the group-closed envelope follows the group-name gate EXACTLY"):
 *
 *   * {@link groupClosedVerdict} — the pure gate an incoming envelope passes
 *     before it may set or replace what is stored: identical first, then
 *     authority read off the LIVE role table (owner or admin resolved at
 *     receipt, C-68; a notice from a plain member is dropped), then the
 *     order (a strictly greater stamp, `groupClosedIsNewer` from the core);
 *   * the row codec — the JSON inside the encrypted IndexedDB row
 *     `group-closed-idb.ts` writes;
 *   * {@link GroupClosedStore} — the in-memory map + persistence.
 *
 * ## Why the second notice wins
 *
 * Two admins may each recreate the same group (UX 16.10): both post a
 * notice, and the later stamp wins the `Open the new group` target. There
 * is no "reopen": a group is closed once any authorised notice landed, and
 * only the successor id moves with a newer one.
 *
 * ## What is NOT here
 *
 * The one-to-one close (16.4) is this device's own decision, recorded in
 * `conversation-closed.ts`; it never rides the journal. And a one-to-one
 * cannot be closed by an envelope at all: it has no admin.
 */
import type { ConversationKind } from "./conversation-kind";

/** A stored or incoming closed notice. */
export interface StoredGroupClosed {
  /** Closer-clock UNIX milliseconds, as stamped in the envelope. */
  readonly closedAtMs: bigint;
  /** base64 of the successor group's id, or `null` when the group was
   *  closed without one. The id is the admin's CLAIM: `Open the new group`
   *  only once this device has joined it through the ordinary path. */
  readonly successorGroupIdB64: string | null;
}

/** Outcome of {@link groupClosedVerdict}. Mirrors the group-name shape. */
export type GroupClosedVerdict =
  | "accept-identical"
  | "accept-owner"
  | "accept-admin"
  /** The group carries no role table (legacy / open group): any member,
   *  when newer. Such a group has no admin to recreate it, so this arm is
   *  reachable only by a notice posted by hand; kept for the gate's
   *  symmetry with the name. */
  | "accept-no-owner"
  | "reject-not-authorized"
  | "reject-unverifiable"
  | "reject-stale"
  /** A one-to-one has no admin and cannot be closed by anyone's notice. */
  | "reject-one-to-one";

/** `true` for every `accept-*` verdict. */
export function groupClosedVerdictAccepts(v: GroupClosedVerdict): boolean {
  return v.startsWith("accept-");
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false;
  return true;
}

function containsKey(keys: readonly Uint8Array[], key: Uint8Array): boolean {
  return keys.some((k) => bytesEqual(k, key));
}

function sameNotice(a: StoredGroupClosed, b: StoredGroupClosed): boolean {
  return a.closedAtMs === b.closedAtMs && a.successorGroupIdB64 === b.successorGroupIdB64;
}

/**
 * The pure receive-side decision — the group-name table verbatim:
 *
 * | role table | sender            | stored          | verdict                 |
 * |------------|-------------------|-----------------|-------------------------|
 * | any        | anyone            | identical       | `accept-identical`      |
 * | unreadable | anyone            | any             | `reject-unverifiable`   |
 * | present    | owner             | none / older    | `accept-owner`          |
 * | present    | admin             | none / older    | `accept-admin`          |
 * | present    | plain member      | any             | `reject-not-authorized` |
 * | present    | unresolvable      | any             | `reject-unverifiable`   |
 * | absent     | anyone            | none / older    | `accept-no-owner`       |
 * | any        | authorised        | newer or equal  | `reject-stale`          |
 *
 * Ahead of the whole table: in a one-to-one every envelope is
 * `reject-one-to-one`.
 *
 * @param isNewer MUST be the wasm `groupClosedIsNewer` export (strictly
 *   greater; equal is not newer).
 */
export function groupClosedVerdict(args: {
  kind: ConversationKind;
  ownerKey: Uint8Array | undefined;
  adminKeys: readonly Uint8Array[];
  /** The sender's ACCOUNT key resolved at receipt (C-68). */
  senderKey: Uint8Array | undefined;
  stored: StoredGroupClosed | null;
  storedUnreadable: boolean;
  incoming: StoredGroupClosed;
  verifyFailed: boolean;
  isNewer: (incomingClosedAtMs: bigint, storedClosedAtMs: bigint) => boolean;
}): GroupClosedVerdict {
  if (args.kind !== "group") return "reject-one-to-one";
  if (args.stored !== null && sameNotice(args.stored, args.incoming)) return "accept-identical";
  if (args.verifyFailed) return "reject-unverifiable";
  if (args.storedUnreadable) return "reject-unverifiable";
  const authorised = ((): GroupClosedVerdict => {
    if (args.ownerKey === undefined) return "accept-no-owner";
    if (args.senderKey === undefined) return "reject-unverifiable";
    if (bytesEqual(args.senderKey, args.ownerKey)) return "accept-owner";
    if (containsKey(args.adminKeys, args.senderKey)) return "accept-admin";
    return "reject-not-authorized";
  })();
  if (!groupClosedVerdictAccepts(authorised)) return authorised;
  if (args.stored === null) return authorised;
  return args.isNewer(args.incoming.closedAtMs, args.stored.closedAtMs)
    ? authorised
    : "reject-stale";
}

// ---------------------------------------------------------------------------
// Row codec
// ---------------------------------------------------------------------------

/** Schema version of the JSON payload inside the encrypted row. */
export const GROUP_CLOSED_ROW_VERSION = 1;

/** The JSON shape inside the ciphertext. */
export interface GroupClosedRowPayload {
  readonly v: 1;
  /** base64 group id -> `{ t: decimal closed_at_ms, s: base64 successor | null }`. */
  readonly g: Record<string, { readonly t: string; readonly s: string | null }>;
}

/** Encode the rows to the payload JSON. */
export function encodeGroupClosedRow(rows: ReadonlyMap<string, StoredGroupClosed>): string {
  const g: Record<string, { t: string; s: string | null }> = {};
  for (const [groupIdB64, v] of rows) {
    g[groupIdB64] = { t: v.closedAtMs.toString(), s: v.successorGroupIdB64 };
  }
  const payload: GroupClosedRowPayload = { v: GROUP_CLOSED_ROW_VERSION, g };
  return JSON.stringify(payload);
}

/** Decode payload JSON, dropping any malformed entry rather than trusting it.
 *  `null` for a payload that is not this schema at all. */
export function decodeGroupClosedRow(json: string): Map<string, StoredGroupClosed> | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const obj = parsed as Partial<GroupClosedRowPayload>;
  if (obj.v !== GROUP_CLOSED_ROW_VERSION) return null;
  const rows = new Map<string, StoredGroupClosed>();
  if (typeof obj.g === "object" && obj.g !== null) {
    for (const [groupIdB64, raw] of Object.entries(obj.g)) {
      if (typeof raw !== "object" || raw === null) continue;
      const t = (raw as { t?: unknown }).t;
      const s = (raw as { s?: unknown }).s;
      if (typeof t !== "string" || !/^\d+$/.test(t)) continue;
      if (s !== null && typeof s !== "string") continue;
      rows.set(groupIdB64, { closedAtMs: BigInt(t), successorGroupIdB64: s });
    }
  }
  return rows;
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/** Where the rows live between sessions. One row per account. */
export interface GroupClosedBackend {
  load(accountId: string): Promise<ReadonlyMap<string, StoredGroupClosed> | null>;
  save(accountId: string, rows: ReadonlyMap<string, StoredGroupClosed>): Promise<void>;
}

/** A backend that keeps nothing (tests, a device with no key). */
export const MEMORY_GROUP_CLOSED_BACKEND: GroupClosedBackend = {
  load: async () => null,
  save: async () => undefined,
};

/**
 * The in-memory view of one account's closed groups, written through to
 * the backend. Decisions are the caller's ({@link groupClosedVerdict});
 * this only stores what it is told to.
 */
export class GroupClosedStore {
  private rows = new Map<string, StoredGroupClosed>();
  private account: string | null = null;
  private readonly listeners = new Set<() => void>();
  private chain: Promise<void> = Promise.resolve();

  constructor(
    private readonly backend: GroupClosedBackend,
    private readonly onPersistError: (err: unknown) => void = () => undefined,
  ) {}

  /** Load the account's row. Resolves `false` when the backend THREW. */
  async open(accountId: string): Promise<boolean> {
    this.account = accountId;
    let loaded: ReadonlyMap<string, StoredGroupClosed> | null = null;
    let readable = true;
    try {
      loaded = await this.backend.load(accountId);
    } catch (err) {
      readable = false;
      this.onPersistError(err);
    }
    if (this.account !== accountId) return readable;
    const next = new Map(loaded ?? []);
    for (const [k, v] of this.rows) next.set(k, v);
    this.rows = next;
    this.notify();
    return readable;
  }

  /** Forget everything in memory (sign-out). The row on disk stays. */
  close(): void {
    this.account = null;
    this.rows = new Map();
    this.notify();
  }

  /** The stored notice for a group, or `undefined` when it is not closed. */
  get(groupIdB64: string): StoredGroupClosed | undefined {
    return this.rows.get(groupIdB64);
  }

  /** Every stored `(groupIdB64, value)`. */
  entries(): IterableIterator<[string, StoredGroupClosed]> {
    return this.rows.entries();
  }

  /** Replace the value for a group and persist. */
  set(accountId: string, groupIdB64: string, value: StoredGroupClosed): Promise<void> {
    this.rows = new Map(this.rows).set(groupIdB64, value);
    this.notify();
    if (this.account !== accountId) return Promise.resolve();
    const snapshot: ReadonlyMap<string, StoredGroupClosed> = this.rows;
    this.chain = this.chain
      .then(() => this.backend.save(accountId, snapshot))
      .catch((err: unknown) => {
        this.onPersistError(err);
      });
    return this.chain;
  }

  /** Forget a group's notice (the `Delete this chat` path). */
  remove(accountId: string, groupIdB64: string): Promise<void> {
    if (!this.rows.has(groupIdB64)) return Promise.resolve();
    const next = new Map(this.rows);
    next.delete(groupIdB64);
    this.rows = next;
    this.notify();
    if (this.account !== accountId) return Promise.resolve();
    const snapshot: ReadonlyMap<string, StoredGroupClosed> = this.rows;
    this.chain = this.chain
      .then(() => this.backend.save(accountId, snapshot))
      .catch((err: unknown) => {
        this.onPersistError(err);
      });
    return this.chain;
  }

  /** Called on any change. */
  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  private notify(): void {
    for (const fn of this.listeners) fn();
  }
}
