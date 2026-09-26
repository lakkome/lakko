// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0052 decision 3 / `docs/ux/message-actions.md` section 4 — the pins
 * this device holds for each conversation, and the RECEIVE-SIDE decision
 * that guards the shared ones.
 *
 * Two kinds of pin live here, and they are different features that share a
 * word (section 4):
 *
 *   * the **shared** pin map per conversation, `messageId -> {pinned, stamp}`,
 *     which travels between members as an envelope and is what the whole
 *     room sees. An UNPIN is stored, never deleted: the stamp is what keeps
 *     a stale pin of the same message from coming back (section 4.1, "What a
 *     receiver does", step 3);
 *   * the **personal** pin set per conversation, `messageId -> pinnedAtMs`,
 *     which never leaves this device.
 *
 * Both are kept free of wasm, Solid and IndexedDB so `vitest` can pin the
 * rules that could actually be wrong:
 *
 *   * {@link pinVerdict} — the pure gate an incoming envelope passes before
 *     it may replace what is stored FOR THAT MESSAGE. It copies the shape of
 *     the group-name verdict (`group-name-store.ts`): identical first, then
 *     authority read off the LIVE role table, then the order. The one
 *     addition is the one-to-one arm: either participant may pin or unpin,
 *     whatever the role table says (ADR-0052 decision 3, "In a one-to-one").
 *   * the row codec ({@link encodePinRow} / {@link decodePinRow}) — the JSON
 *     inside the encrypted IndexedDB row `pin-idb.ts` writes.
 *   * {@link PinStore} — the in-memory maps + persistence, one row per
 *     account.
 *
 * ## Why the order is per message, not per conversation
 *
 * A stamp only orders writes about the SAME thing. Two admins pinning two
 * different messages are not in conflict, and comparing their stamps would
 * make the second pin refuse the first for no reason. So the stored stamp
 * consulted is the one for the incoming envelope's `messageId`, and a
 * message this device has never seen a pin for has nothing to order against.
 */

/** A stored or incoming shared-pin action for one message. */
export interface StoredPin {
  /** `true` = pinned for everyone; `false` = unpinned (kept for its stamp). */
  readonly pinned: boolean;
  /** Setter-clock UNIX milliseconds, as stamped in the envelope. */
  readonly updatedAtMs: bigint;
}

/** A decoded incoming envelope: the action plus the message it names. */
export interface IncomingPin extends StoredPin {
  /** The DS `messageId` the action is about. */
  readonly messageId: number;
}

/** Outcome of {@link pinVerdict}. Mirrors the group-name shape. */
export type PinVerdict =
  /** Byte-identical to what is stored for that message: no write. */
  | "accept-identical"
  /** The sender is the ADR-0022 owner and the action is newer. */
  | "accept-owner"
  /** The sender is an ADR-0022 admin and the action is newer. */
  | "accept-admin"
  /** The conversation is a one-to-one: either participant, when newer. */
  | "accept-participant"
  /** The group carries no role table (legacy / open group): any member,
   *  when newer. */
  | "accept-no-owner"
  /** A plain group member tried to pin for everyone. Dropped. */
  | "reject-not-authorized"
  /** The role table / roster could not be read, the sender could not be
   *  resolved, or the local row could not be read to order against.
   *  Dropped (fail-closed, invariant #4). */
  | "reject-unverifiable"
  /** Authorised, but not strictly newer than what is stored for that
   *  message. Dropped — a re-send is not an update, and a late pin never
   *  undoes a later unpin. */
  | "reject-stale";

/** `true` for every `accept-*` verdict. */
export function pinVerdictAccepts(v: PinVerdict): boolean {
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

/**
 * The pure receive-side decision (section 4.1, "What a receiver does").
 *
 * | conversation | role table | sender        | stored (that msg) | verdict                 |
 * |--------------|------------|---------------|-------------------|-------------------------|
 * | any          | any        | anyone        | identical         | `accept-identical`      |
 * | any          | unreadable | anyone        | any               | `reject-unverifiable`   |
 * | one-to-one   | any        | anyone        | none / older      | `accept-participant`    |
 * | group        | present    | owner         | none / older      | `accept-owner`          |
 * | group        | present    | admin         | none / older      | `accept-admin`          |
 * | group        | present    | plain member  | any               | `reject-not-authorized` |
 * | group        | present    | unresolvable  | any               | `reject-unverifiable`   |
 * | group        | absent     | anyone        | none / older      | `accept-no-owner`       |
 * | any          | any        | authorised    | newer or equal    | `reject-stale`          |
 *
 * @param isNewer MUST be the wasm `pinIsNewer` export, threaded in so the
 *   order stays defined once in the core (strictly greater; equal is not
 *   newer).
 */
export function pinVerdict(args: {
  /** `true` when the roster holds exactly two people. Either may pin. */
  oneToOne: boolean;
  /** `group.roleTableOwner()` — `undefined` = legacy / open group. */
  ownerKey: Uint8Array | undefined;
  /** `group.roleTableAdmins()` sliced into 32-byte keys. Only consulted when
   *  `ownerKey` is defined. */
  adminKeys: readonly Uint8Array[];
  /** The sender's ACCOUNT key resolved at receipt (C-68), `undefined` when
   *  the leaf could not be resolved. */
  senderKey: Uint8Array | undefined;
  /** What this device holds FOR THAT MESSAGE, `null` when nothing. */
  stored: StoredPin | null;
  /** Set when the local row could not be read. There is then nothing to
   *  order against, so a changed action is refused. */
  storedUnreadable: boolean;
  /** The decoded incoming envelope. */
  incoming: StoredPin;
  /** Set when reading the role table / roster threw. */
  verifyFailed: boolean;
  /** The canonical order from the wasm core. */
  isNewer: (incomingUpdatedAtMs: bigint, storedUpdatedAtMs: bigint) => boolean;
}): PinVerdict {
  // (1) Idempotent case first: a re-observed identical action (a growth
  // re-send of exactly what is held) changes nothing, so it needs neither
  // authority nor ordering, and it must not fail a device whose table read
  // happened to tear.
  if (
    args.stored !== null &&
    args.stored.pinned === args.incoming.pinned &&
    args.stored.updatedAtMs === args.incoming.updatedAtMs
  ) {
    return "accept-identical";
  }
  // From here the envelope would CHANGE what this device shows.
  if (args.verifyFailed) return "reject-unverifiable";
  if (args.storedUnreadable) return "reject-unverifiable";
  // (2) Authority. A one-to-one has no asymmetry to enforce; a group reads
  // the live role table.
  const authorised = ((): PinVerdict => {
    if (args.oneToOne) return "accept-participant";
    if (args.ownerKey === undefined) return "accept-no-owner";
    if (args.senderKey === undefined) return "reject-unverifiable";
    if (bytesEqual(args.senderKey, args.ownerKey)) return "accept-owner";
    if (containsKey(args.adminKeys, args.senderKey)) return "accept-admin";
    return "reject-not-authorized";
  })();
  if (!pinVerdictAccepts(authorised)) return authorised;
  // (3) Nothing stored for that message to order against.
  if (args.stored === null) return authorised;
  // (4) The order decides WHICH authorised write survives.
  return args.isNewer(args.incoming.updatedAtMs, args.stored.updatedAtMs)
    ? authorised
    : "reject-stale";
}

/**
 * The stamp a LOCAL shared-pin action carries: the setter's clock, moved
 * strictly past whatever this device holds for that message when the clock
 * lags it. Without the floor an unpin stamped by a slow clock would order
 * BEFORE the pin it undoes and every receiver — this device included, since
 * it applies its own action the way a receiver would — would drop it.
 */
export function freshPinStamp(nowMs: number, stored: StoredPin | undefined): bigint {
  const now = BigInt(nowMs);
  return stored !== undefined && stored.updatedAtMs >= now ? stored.updatedAtMs + 1n : now;
}

// ---------------------------------------------------------------------------
// Row codec
// ---------------------------------------------------------------------------

/** Schema version of the JSON payload inside the encrypted row. */
export const PIN_ROW_VERSION = 1;

/** Shared pins of one conversation: decimal messageId -> `{p, t}`. */
type SharedRowGroup = Record<string, { readonly p: 0 | 1; readonly t: string }>;
/** Personal pins of one conversation: decimal messageId -> decimal pinnedAtMs. */
type PersonalRowGroup = Record<string, string>;

/** The JSON shape inside the ciphertext. */
export interface PinRowPayload {
  readonly v: 1;
  /** base64 group id -> shared pins. */
  readonly s: Record<string, SharedRowGroup>;
  /** base64 group id -> personal pins. */
  readonly m: Record<string, PersonalRowGroup>;
}

/** Everything the row holds, in memory. */
export interface PinRows {
  readonly shared: ReadonlyMap<string, ReadonlyMap<number, StoredPin>>;
  readonly personal: ReadonlyMap<string, ReadonlyMap<number, bigint>>;
}

const EMPTY_ROWS: PinRows = { shared: new Map(), personal: new Map() };

/** Encode the rows to the payload JSON. */
export function encodePinRow(rows: PinRows): string {
  const s: Record<string, SharedRowGroup> = {};
  for (const [groupIdB64, pins] of rows.shared) {
    if (pins.size === 0) continue;
    const g: SharedRowGroup = {};
    for (const [id, v] of pins) g[String(id)] = { p: v.pinned ? 1 : 0, t: v.updatedAtMs.toString() };
    s[groupIdB64] = g;
  }
  const m: Record<string, PersonalRowGroup> = {};
  for (const [groupIdB64, pins] of rows.personal) {
    if (pins.size === 0) continue;
    const g: PersonalRowGroup = {};
    for (const [id, at] of pins) g[String(id)] = at.toString();
    m[groupIdB64] = g;
  }
  const payload: PinRowPayload = { v: PIN_ROW_VERSION, s, m };
  return JSON.stringify(payload);
}

const DECIMAL = /^\d+$/;

function parseMessageId(raw: string): number | null {
  if (!DECIMAL.test(raw)) return null;
  const n = Number(raw);
  return Number.isSafeInteger(n) ? n : null;
}

/** Decode payload JSON, dropping any malformed entry rather than trusting it.
 *  `null` for a payload that is not this schema at all. */
export function decodePinRow(json: string): PinRows | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const obj = parsed as Partial<PinRowPayload>;
  if (obj.v !== PIN_ROW_VERSION) return null;
  const shared = new Map<string, Map<number, StoredPin>>();
  if (typeof obj.s === "object" && obj.s !== null) {
    for (const [groupIdB64, rawGroup] of Object.entries(obj.s)) {
      if (typeof rawGroup !== "object" || rawGroup === null) continue;
      const pins = new Map<number, StoredPin>();
      for (const [rawId, raw] of Object.entries(rawGroup)) {
        const id = parseMessageId(rawId);
        if (id === null || typeof raw !== "object" || raw === null) continue;
        const p = (raw as { p?: unknown }).p;
        const t = (raw as { t?: unknown }).t;
        if (p !== 0 && p !== 1) continue;
        if (typeof t !== "string" || !DECIMAL.test(t)) continue;
        pins.set(id, { pinned: p === 1, updatedAtMs: BigInt(t) });
      }
      if (pins.size > 0) shared.set(groupIdB64, pins);
    }
  }
  const personal = new Map<string, Map<number, bigint>>();
  if (typeof obj.m === "object" && obj.m !== null) {
    for (const [groupIdB64, rawGroup] of Object.entries(obj.m)) {
      if (typeof rawGroup !== "object" || rawGroup === null) continue;
      const pins = new Map<number, bigint>();
      for (const [rawId, at] of Object.entries(rawGroup)) {
        const id = parseMessageId(rawId);
        if (id === null || typeof at !== "string" || !DECIMAL.test(at)) continue;
        pins.set(id, BigInt(at));
      }
      if (pins.size > 0) personal.set(groupIdB64, pins);
    }
  }
  return { shared, personal };
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/** Where the rows live between sessions. One row per account. */
export interface PinBackend {
  load(accountId: string): Promise<PinRows | null>;
  save(accountId: string, rows: PinRows): Promise<void>;
}

/** A backend that keeps nothing (tests, a device with no key). */
export const MEMORY_PIN_BACKEND: PinBackend = {
  load: async () => null,
  save: async () => undefined,
};

const NO_SHARED: ReadonlyMap<number, StoredPin> = new Map();
const NO_PERSONAL: ReadonlyMap<number, bigint> = new Map();

/**
 * The in-memory view of one account's pins, written through to the
 * backend. Decisions are the caller's ({@link pinVerdict}, or its own
 * action); this only stores what it is told to.
 */
export class PinStore {
  private rows: PinRows = EMPTY_ROWS;
  private account: string | null = null;
  private readonly listeners = new Set<() => void>();
  private chain: Promise<void> = Promise.resolve();

  constructor(
    private readonly backend: PinBackend,
    private readonly onPersistError: (err: unknown) => void = () => undefined,
  ) {}

  /** Load the account's row. Resolves `false` when the backend THREW — the
   *  caller then cannot tell "nothing stored" from "unreadable", which the
   *  verdict treats as unverifiable (fail-closed) rather than as a first
   *  observation. */
  async open(accountId: string): Promise<boolean> {
    this.account = accountId;
    let loaded: PinRows | null = null;
    let readable = true;
    try {
      loaded = await this.backend.load(accountId);
    } catch (err) {
      readable = false;
      this.onPersistError(err);
    }
    // The session may have moved on while the load was in flight.
    if (this.account !== accountId) return readable;
    // A write that landed before the load settled wins over the stored row.
    const shared = new Map<string, Map<number, StoredPin>>();
    for (const [g, pins] of loaded?.shared ?? []) shared.set(g, new Map(pins));
    for (const [g, pins] of this.rows.shared) {
      const target = shared.get(g) ?? new Map<number, StoredPin>();
      for (const [id, v] of pins) target.set(id, v);
      shared.set(g, target);
    }
    const personal = new Map<string, Map<number, bigint>>();
    for (const [g, pins] of loaded?.personal ?? []) personal.set(g, new Map(pins));
    for (const [g, pins] of this.rows.personal) {
      const target = personal.get(g) ?? new Map<number, bigint>();
      for (const [id, at] of pins) target.set(id, at);
      personal.set(g, target);
    }
    this.rows = { shared, personal };
    this.notify();
    return readable;
  }

  /** Forget everything in memory (sign-out). The row on disk stays. */
  close(): void {
    this.account = null;
    this.rows = EMPTY_ROWS;
    this.notify();
  }

  /** The shared pin map of a conversation (pins AND stored unpins). */
  shared(groupIdB64: string): ReadonlyMap<number, StoredPin> {
    return this.rows.shared.get(groupIdB64) ?? NO_SHARED;
  }

  /** The stored shared action for one message, or `undefined`. */
  getShared(groupIdB64: string, messageId: number): StoredPin | undefined {
    return this.rows.shared.get(groupIdB64)?.get(messageId);
  }

  /** The personal pins of a conversation: messageId -> pinnedAtMs. */
  personal(groupIdB64: string): ReadonlyMap<number, bigint> {
    return this.rows.personal.get(groupIdB64) ?? NO_PERSONAL;
  }

  /** Replace the shared action for one message and persist. */
  setShared(accountId: string, groupIdB64: string, messageId: number, value: StoredPin): Promise<void> {
    const shared = new Map(this.rows.shared);
    const pins = new Map(shared.get(groupIdB64) ?? []);
    pins.set(messageId, value);
    shared.set(groupIdB64, pins);
    return this.commit(accountId, { shared, personal: this.rows.personal });
  }

  /** Pin (`pinnedAtMs`) or unpin (`null`) one message for this device only. */
  setPersonal(
    accountId: string,
    groupIdB64: string,
    messageId: number,
    pinnedAtMs: bigint | null,
  ): Promise<void> {
    const personal = new Map(this.rows.personal);
    const pins = new Map(personal.get(groupIdB64) ?? []);
    if (pinnedAtMs === null) pins.delete(messageId);
    else pins.set(messageId, pinnedAtMs);
    if (pins.size === 0) personal.delete(groupIdB64);
    else personal.set(groupIdB64, pins);
    return this.commit(accountId, { shared: this.rows.shared, personal });
  }

  /** The message is gone (deleted): drop both records for it. Section 4.1 —
   *  "its shared pin record is dropped with the message". */
  dropMessage(accountId: string, groupIdB64: string, messageId: number): Promise<void> {
    const hadShared = this.rows.shared.get(groupIdB64)?.has(messageId) === true;
    const hadPersonal = this.rows.personal.get(groupIdB64)?.has(messageId) === true;
    if (!hadShared && !hadPersonal) return Promise.resolve();
    const shared = new Map(this.rows.shared);
    if (hadShared) {
      const pins = new Map(shared.get(groupIdB64));
      pins.delete(messageId);
      if (pins.size === 0) shared.delete(groupIdB64);
      else shared.set(groupIdB64, pins);
    }
    const personal = new Map(this.rows.personal);
    if (hadPersonal) {
      const pins = new Map(personal.get(groupIdB64));
      pins.delete(messageId);
      if (pins.size === 0) personal.delete(groupIdB64);
      else personal.set(groupIdB64, pins);
    }
    return this.commit(accountId, { shared, personal });
  }

  /** The conversation is gone from this device: wipe everything about it. */
  forgetGroup(accountId: string, groupIdB64: string): Promise<void> {
    if (!this.rows.shared.has(groupIdB64) && !this.rows.personal.has(groupIdB64)) {
      return Promise.resolve();
    }
    const shared = new Map(this.rows.shared);
    shared.delete(groupIdB64);
    const personal = new Map(this.rows.personal);
    personal.delete(groupIdB64);
    return this.commit(accountId, { shared, personal });
  }

  /** Called on any change. */
  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  /** Swap the rows in, notify, and write through. Resolves once the write
   *  settled; a persist failure is reported and swallowed — the in-memory
   *  value stands for the session either way. */
  private commit(accountId: string, next: PinRows): Promise<void> {
    this.rows = next;
    this.notify();
    if (this.account !== accountId) return Promise.resolve();
    const snapshot = next;
    this.chain = this.chain
      .then(() => this.backend.save(accountId, snapshot))
      .catch((err: unknown) => {
        this.onPersistError(err);
      });
    return this.chain;
  }

  private notify(): void {
    for (const fn of this.listeners) fn();
  }
}
