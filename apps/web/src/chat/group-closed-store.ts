// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 "Phase 2 revised", R.6 - the per-`(account, group)` CLAIM state
 * a received `group_closed` envelope leaves on this device.
 *
 * Since the visible flow was removed, a `group_closed` envelope has one
 * producer: a device founding the successor of a conversation that cannot
 * be repaired in place (a one-to-one whose other person reset their keys, a
 * group whose owner did). The decision that guards it is
 * `successor-claim.ts` (`claimVerdict`): the FIRST eligible claim in the
 * old journal's order wins, for good. This file is what is kept between
 * sessions:
 *
 *   * `accepted` - the winning claim: the conversation's successor;
 *   * `pending`  - claims that arrived before this device had its own
 *     verdict on the reset, kept in journal order and judged again after
 *     the identity check the claim itself triggers;
 *   * `own`      - this device's own claim, from the moment it is posted
 *     until step 3 decided it (so a restart between the post and the read
 *     neither founds a second successor nor forgets the local one).
 *
 * Nothing here is shown: the store has no reader in any view (R.0).
 */
import type { SuccessorClaim } from "./successor-claim";

/** This device's own claim, posted and not yet decided. */
export interface OwnClaim {
  /** base64 of the successor this device created locally. */
  readonly successorGroupIdB64: string;
  /** The DS message id the publish returned; `null` while the publish is
   *  in flight (a restart then re-reads the journal for its own row). */
  readonly messageId: number | null;
}

/**
 * The successor THIS device created for the old group (R.6 step 1), kept
 * from the moment it exists until its Welcome was delivered, and after that
 * as the mark "this device founded it" (the founder alone re-sends, R.7
 * case 1). The Welcome is a public MLS artefact sealed to its recipients;
 * it is kept so a restart between winning the claim and delivering it does
 * not leave a successor nobody can join.
 */
export interface PreparedSuccessor {
  readonly successorGroupIdB64: string;
  /** base64 of the Welcome of the founding commit; `null` once delivered. */
  readonly welcomeB64: string | null;
  /** base64 account keys of the people the Welcome goes to, one each. */
  readonly targetKeysB64: readonly string[];
  readonly delivered: boolean;
}

/** What is stored for one old group. */
export interface StoredGroupClosed {
  readonly accepted: SuccessorClaim | null;
  readonly pending: readonly SuccessorClaim[];
  readonly own: OwnClaim | null;
  readonly prepared?: PreparedSuccessor | null;
}

/** The empty state. */
export const NO_CLAIMS: StoredGroupClosed = { accepted: null, pending: [], own: null };

/** At most this many undecided claims are kept per group: the journal's
 *  claims arrive from members, and an unbounded list would be a way to grow
 *  this device's storage. Chosen, not measured. */
export const MAX_PENDING_CLAIMS = 16;

/** `state` with `claim` kept for a later verdict, in journal order, once. */
export function withPendingClaim(state: StoredGroupClosed, claim: SuccessorClaim): StoredGroupClosed {
  if (state.pending.some((c) => c.messageId === claim.messageId)) return state;
  const pending = [...state.pending, claim]
    .sort((a, b) => a.messageId - b.messageId)
    .slice(0, MAX_PENDING_CLAIMS);
  return { ...state, pending };
}

/** `state` with `claim` as the conversation's successor; the pending list
 *  is spent. */
export function withAcceptedClaim(state: StoredGroupClosed, claim: SuccessorClaim): StoredGroupClosed {
  return {
    accepted: claim,
    pending: [],
    own: state.own,
    ...(state.prepared === undefined ? {} : { prepared: state.prepared }),
  };
}

// ---------------------------------------------------------------------------
// Row codec
// ---------------------------------------------------------------------------

/** Schema version of the JSON payload inside the encrypted row. Version 3 is
 *  the claim state; rows of the visible flow (1, 2) are not read. */
export const GROUP_CLOSED_ROW_VERSION = 3;

interface ClaimJson {
  readonly m: number;
  readonly s: string;
  readonly k: string;
  readonly a: boolean;
  readonly t: readonly string[];
}

/** The JSON shape inside the ciphertext. */
export interface GroupClosedRowPayload {
  readonly v: 3;
  readonly g: Record<
    string,
    {
      readonly a: ClaimJson | null;
      readonly p: readonly ClaimJson[];
      readonly o: { readonly s: string; readonly m: number | null } | null;
      readonly r?: {
        readonly s: string;
        readonly w: string | null;
        readonly t: readonly string[];
        readonly d: boolean;
      } | null;
    }
  >;
}

function claimToJson(c: SuccessorClaim): ClaimJson {
  return {
    m: c.messageId,
    s: c.successorGroupIdB64,
    k: c.senderKeyB64,
    a: c.senderWasAdmin,
    t: [...c.adminKeysAtReceiptB64],
  };
}

function claimFromJson(raw: unknown): SuccessorClaim | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Partial<ClaimJson>;
  if (typeof r.m !== "number" || !Number.isSafeInteger(r.m) || r.m < 0) return null;
  if (typeof r.s !== "string" || typeof r.k !== "string" || typeof r.a !== "boolean") return null;
  if (!Array.isArray(r.t) || !r.t.every((x) => typeof x === "string")) return null;
  return {
    messageId: r.m,
    successorGroupIdB64: r.s,
    senderKeyB64: r.k,
    senderWasAdmin: r.a,
    adminKeysAtReceiptB64: [...r.t],
  };
}

/** Encode the rows to the payload JSON. */
export function encodeGroupClosedRow(rows: ReadonlyMap<string, StoredGroupClosed>): string {
  const g: GroupClosedRowPayload["g"] = {};
  for (const [groupIdB64, v] of rows) {
    g[groupIdB64] = {
      a: v.accepted === null ? null : claimToJson(v.accepted),
      p: v.pending.map(claimToJson),
      o: v.own === null ? null : { s: v.own.successorGroupIdB64, m: v.own.messageId },
      ...(v.prepared === undefined || v.prepared === null
        ? {}
        : {
            r: {
              s: v.prepared.successorGroupIdB64,
              w: v.prepared.welcomeB64,
              t: [...v.prepared.targetKeysB64],
              d: v.prepared.delivered,
            },
          }),
    };
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
      const a = (raw as { a?: unknown }).a;
      const p = (raw as { p?: unknown }).p;
      const o = (raw as { o?: unknown }).o;
      const accepted = a === null || a === undefined ? null : claimFromJson(a);
      if (a !== null && a !== undefined && accepted === null) continue;
      const pending: SuccessorClaim[] = [];
      if (Array.isArray(p)) {
        for (const item of p) {
          const claim = claimFromJson(item);
          if (claim !== null) pending.push(claim);
        }
      }
      let own: OwnClaim | null = null;
      if (typeof o === "object" && o !== null) {
        const os = (o as { s?: unknown }).s;
        const om = (o as { m?: unknown }).m;
        if (typeof os === "string" && (om === null || (typeof om === "number" && Number.isSafeInteger(om)))) {
          own = { successorGroupIdB64: os, messageId: om };
        }
      }
      const r = (raw as { r?: unknown }).r;
      let prepared: PreparedSuccessor | null = null;
      if (typeof r === "object" && r !== null) {
        const rs = (r as { s?: unknown }).s;
        const rw = (r as { w?: unknown }).w;
        const rt = (r as { t?: unknown }).t;
        const rd = (r as { d?: unknown }).d;
        if (
          typeof rs === "string" &&
          (rw === null || typeof rw === "string") &&
          Array.isArray(rt) &&
          rt.every((x) => typeof x === "string") &&
          typeof rd === "boolean"
        ) {
          prepared = { successorGroupIdB64: rs, welcomeB64: rw, targetKeysB64: [...rt], delivered: rd };
        }
      }
      rows.set(groupIdB64, {
        accepted,
        pending: pending.sort((x, y) => x.messageId - y.messageId).slice(0, MAX_PENDING_CLAIMS),
        own,
        ...(prepared === null ? {} : { prepared }),
      });
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
 * The in-memory view of one account's claim states, written through to the
 * backend. Decisions are the caller's (`successor-claim.ts`); this only
 * stores what it is told to.
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

  /** The claim state of an old group, or `undefined` when it has none. */
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

  /** Forget a group's claim state (the `Delete this chat` path). */
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
