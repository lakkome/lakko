// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/calls.md` §6 — the durable per-`(account, group)` call rows.
 *
 * Modelled on `chat/read-watermark-store.ts` down to the concurrency: a pure
 * in-memory view written through to a backend, a `chain` so a burst of writes
 * costs one save, and a `loading` promise so a row recorded before the load
 * settles is merged over the loaded row rather than replaced by it. That last
 * one is not theoretical here: a call can end within the first second of a
 * session (the callee declines a call that was already ringing when the tab
 * opened), which is exactly the window the load is in flight.
 *
 * ## Why a store of its own, and not the message log
 *
 * `chat/message-log-store.ts` already persists the timeline, and putting call
 * rows in it looks like the smaller change. It is the riskier one. That log is
 * the shared write target of the foreground chat and the background receive —
 * `chat/background-log.ts` merges the two — and its rows carry a DS cursor,
 * message ids and an ack state that a call has none of. A call row has no
 * message id, cannot move the cursor, and must not be able to corrupt the
 * record of what was actually received if its own shape is wrong.
 *
 * So it lives beside the message log rather than inside it, and the timeline
 * merges the two at render (`chat/call-timeline.ts`). The cost is a second
 * IndexedDB database; the benefit is that nothing here can lose a message.
 *
 * ## Bounded
 *
 * Unlike the read watermarks — one entry per sender, bounded by the group —
 * call rows accumulate forever. {@link CALL_LOG_MAX_PER_GROUP} newest rows per
 * group are kept and the rest are dropped on write, so the encrypted row
 * cannot grow without limit on a device that calls a lot.
 *
 * Free of wasm, Solid and IndexedDB so vitest can pin it. The encrypted
 * backend is `call-log-idb.ts`; the session runtime is `CallLogContext.tsx`.
 */

import type { CallLogEntry, CallOutcome, StandaloneCallEntry } from "./call-log";

/** How many rows are kept per conversation, newest first. */
export const CALL_LOG_MAX_PER_GROUP = 200;

/** How many rows of calls with no conversation (`docs/ux/navigation.md`
 *  section 5.1.1) are kept, newest first. */
export const CALL_LOG_MAX_STANDALONE = 200;

/** All groups' call rows for one account, each list oldest-first. */
export type CallLogRows = ReadonlyMap<string, readonly CallLogEntry[]>;

/**
 * Everything one account's row holds: the per-conversation rows, and the
 * calls that belong to no conversation (a `New call`, a call joined from a
 * link), oldest first, one per call id.
 */
export interface CallLogData {
  readonly groups: CallLogRows;
  readonly standalone: readonly StandaloneCallEntry[];
}

const EMPTY: readonly CallLogEntry[] = [];
const EMPTY_STANDALONE: readonly StandaloneCallEntry[] = [];

const OUTCOMES: readonly CallOutcome[] = ["outgoing", "incoming", "missed", "declined", "failed"];

// ---------------------------------------------------------------------------
// Row codec
// ---------------------------------------------------------------------------

/** Schema version of the JSON payload inside the encrypted row. Version 2
 *  added `n`; a version 1 row is not read (no data to carry over). */
export const CALL_LOG_ROW_VERSION = 2;

/** The JSON shape inside the ciphertext. */
export interface CallLogRowPayload {
  readonly v: 2;
  /** base64 group id -> the group's rows, oldest first. */
  readonly g: Record<string, readonly CallLogEntry[]>;
  /** Calls with no conversation, oldest first. */
  readonly n: readonly StandaloneCallEntry[];
}

/** Encode the rows to the payload JSON. */
export function encodeCallLogRow(data: CallLogData): string {
  const g: Record<string, readonly CallLogEntry[]> = {};
  for (const [groupIdB64, list] of data.groups) {
    if (list.length > 0) g[groupIdB64] = list;
  }
  const payload: CallLogRowPayload = { v: CALL_LOG_ROW_VERSION, g, n: data.standalone };
  return JSON.stringify(payload);
}

/**
 * One entry from untrusted JSON, or `null`.
 *
 * Strict field by field rather than a cast: this is read back from disk, and a
 * row written by a future version — or half-written by a crash — must produce
 * no row rather than a row that renders as `undefined` in the middle of a
 * conversation.
 */
function decodeEntry(raw: unknown): CallLogEntry | null {
  if (typeof raw !== "object" || raw === null) return null;
  const o = raw as Record<string, unknown>;
  if (typeof o.id !== "string" || o.id.length === 0) return null;
  if (typeof o.outcome !== "string" || !OUTCOMES.includes(o.outcome as CallOutcome)) return null;
  if (typeof o.video !== "boolean") return null;
  if (typeof o.startedAtMs !== "number" || !Number.isFinite(o.startedAtMs)) return null;
  // A call that never connected stores `null`, which is not "zero seconds".
  const duration = o.durationMs;
  if (duration !== null && (typeof duration !== "number" || !Number.isFinite(duration) || duration < 0)) {
    return null;
  }
  return {
    id: o.id,
    outcome: o.outcome as CallOutcome,
    video: o.video,
    startedAtMs: o.startedAtMs,
    durationMs: duration,
  };
}

/** One call with no conversation from untrusted JSON, or `null`. Only the two
 *  outcomes such a call can have. */
function decodeStandaloneEntry(raw: unknown): StandaloneCallEntry | null {
  const base = decodeEntry(raw);
  if (base === null) return null;
  if (base.outcome !== "outgoing" && base.outcome !== "incoming") return null;
  return { ...base, outcome: base.outcome };
}

/** Decode payload JSON, dropping any malformed entry rather than trusting it.
 *  `null` for a payload that is not this schema at all. */
export function decodeCallLogRow(json: string): CallLogData | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const obj = parsed as Partial<CallLogRowPayload>;
  if (obj.v !== CALL_LOG_ROW_VERSION) return null;
  const groups = new Map<string, readonly CallLogEntry[]>();
  if (typeof obj.g === "object" && obj.g !== null) {
    for (const [groupIdB64, raw] of Object.entries(obj.g)) {
      if (groupIdB64.length === 0 || !Array.isArray(raw)) continue;
      const list: CallLogEntry[] = [];
      for (const item of raw) {
        const entry = decodeEntry(item);
        if (entry !== null) list.push(entry);
      }
      if (list.length > 0) groups.set(groupIdB64, sortAndCap(list, CALL_LOG_MAX_PER_GROUP));
    }
  }
  const standalone: StandaloneCallEntry[] = [];
  if (Array.isArray(obj.n)) {
    for (const item of obj.n as readonly unknown[]) {
      const entry = decodeStandaloneEntry(item);
      if (entry !== null) standalone.push(entry);
    }
  }
  return { groups, standalone: sortAndCap(standalone, CALL_LOG_MAX_STANDALONE) };
}

// ---------------------------------------------------------------------------
// Folding
// ---------------------------------------------------------------------------

/** Oldest first, newest kept when the cap bites, and one row per call id. */
function sortAndCap<T extends CallLogEntry>(list: readonly T[], cap: number): readonly T[] {
  const byId = new Map<string, T>();
  for (const entry of list) byId.set(entry.id, entry);
  const sorted = [...byId.values()].sort((a, b) => a.startedAtMs - b.startedAtMs);
  return sorted.length <= cap ? sorted : sorted.slice(sorted.length - cap);
}

function sameEntry(a: CallLogEntry, b: CallLogEntry): boolean {
  return (
    a.outcome === b.outcome &&
    a.video === b.video &&
    a.startedAtMs === b.startedAtMs &&
    a.durationMs === b.durationMs
  );
}

/**
 * Fold one call into a group's rows, returning a NEW list — or the list
 * unchanged when nothing about it moved.
 *
 * **Keyed on the call id, so recording is idempotent.** A session can report
 * its terminal state more than once (a local end racing the peer's `End` on
 * the journal), and a call is one event however many times it is reported. The
 * later record wins, which is what lets a call be provisionally recorded and
 * then corrected — a row written when the call ended is replaced if the same
 * call is somehow settled again.
 */
export function foldCallLogEntry(
  list: readonly CallLogEntry[],
  entry: CallLogEntry,
): readonly CallLogEntry[] {
  const existing = list.find((e) => e.id === entry.id);
  if (existing !== undefined && sameEntry(existing, entry)) return list;
  return sortAndCap([...list.filter((e) => e.id !== entry.id), entry], CALL_LOG_MAX_PER_GROUP);
}

/** Fold every entry of `from` into `into`. */
export function mergeCallLogEntries(
  into: readonly CallLogEntry[],
  from: readonly CallLogEntry[],
): readonly CallLogEntry[] {
  let out = into;
  for (const entry of from) out = foldCallLogEntry(out, entry);
  return out;
}

/**
 * Fold one call with no conversation into that list: one row per call id
 * (`docs/ux/navigation.md` section 6.2). The FIRST row for a call stays as it
 * is: a later record for a call id already held — this device joining the
 * same call again — is dropped, and the list comes back unchanged (as on
 * Android).
 */
export function foldStandaloneCallEntry(
  list: readonly StandaloneCallEntry[],
  entry: StandaloneCallEntry,
): readonly StandaloneCallEntry[] {
  if (list.some((e) => e.id === entry.id)) return list;
  return sortAndCap([...list, entry], CALL_LOG_MAX_STANDALONE);
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/** Where the rows live between sessions. One row per account. */
export interface CallLogBackend {
  load(accountId: string): Promise<CallLogData | null>;
  save(accountId: string, data: CallLogData): Promise<void>;
}

/** A backend that keeps nothing (tests, a device with no key). */
export const MEMORY_CALL_LOG_BACKEND: CallLogBackend = {
  load: async () => null,
  save: async () => undefined,
};

/**
 * The in-memory view of one account's call rows, written through to the
 * backend.
 */
export class CallLogStore {
  private rows = new Map<string, readonly CallLogEntry[]>();
  /** Calls with no conversation, oldest first. */
  private standaloneRows: readonly StandaloneCallEntry[] = EMPTY_STANDALONE;
  private account: string | null = null;
  private readonly listeners = new Set<() => void>();
  private chain: Promise<void> = Promise.resolve();
  /** A save is queued and has not started yet: a further change rides it. */
  private saveQueued = false;
  /** The in-flight load. A save waits for it, so a call recorded during the
   *  load can never overwrite the stored row with a partial one. */
  private loading: Promise<void> = Promise.resolve();

  constructor(
    private readonly backend: CallLogBackend,
    private readonly onPersistError: (err: unknown) => void = () => undefined,
  ) {}

  /** Bind to an account and load its row. Calls recorded before the load
   *  settled are folded over the loaded row, never replaced by it. */
  open(accountId: string): Promise<void> {
    if (this.account !== accountId) {
      this.account = accountId;
      this.rows = new Map();
      this.standaloneRows = EMPTY_STANDALONE;
      this.notify();
    }
    const attempt = this.load(accountId);
    this.loading = attempt;
    return attempt;
  }

  private async load(accountId: string): Promise<void> {
    let loaded: CallLogData | null = null;
    try {
      loaded = await this.backend.load(accountId);
    } catch (err) {
      this.onPersistError(err);
    }
    // The session may have moved on while the load was in flight.
    if (this.account !== accountId || loaded === null) return;
    const next = new Map<string, readonly CallLogEntry[]>(loaded.groups);
    for (const [groupIdB64, list] of this.rows) {
      next.set(groupIdB64, mergeCallLogEntries(next.get(groupIdB64) ?? EMPTY, list));
    }
    this.rows = next;
    // The stored row was written first, so for a call id held on both sides
    // the stored one stays.
    let standalone = loaded.standalone;
    for (const entry of this.standaloneRows) standalone = foldStandaloneCallEntry(standalone, entry);
    this.standaloneRows = standalone;
    this.notify();
  }

  /** Forget everything in memory (sign-out). The row on disk stays. */
  close(): void {
    this.account = null;
    this.rows = new Map();
    this.standaloneRows = EMPTY_STANDALONE;
    this.notify();
  }

  /** The account the store is bound to, or `null`. */
  boundAccount(): string | null {
    return this.account;
  }

  /** A group's rows, oldest first (empty when none). Never mutate them. */
  of(groupIdB64: string): readonly CallLogEntry[] {
    return this.rows.get(groupIdB64) ?? EMPTY;
  }

  /**
   * Every conversation's rows, for the Calls screen
   * (`docs/ux/navigation.md` section 5), which wants the cut across
   * conversations that ``of`` deliberately does not give. Read-only: the
   * caller flattens and orders (`call-history.ts`) and never writes here.
   */
  all(): CallLogRows {
    return this.rows;
  }

  /** The calls with no conversation, oldest first, for the Calls screen
   *  (`docs/ux/navigation.md` section 5.1.1). Never mutate them. */
  standalone(): readonly StandaloneCallEntry[] {
    return this.standaloneRows;
  }

  /**
   * Record one finished call. Returns `true` when something changed (and a
   * save was scheduled). A call for an account the store is not bound to is
   * dropped — it must never land in another account's row.
   */
  record(accountId: string, groupIdB64: string, entry: CallLogEntry): boolean {
    if (this.account !== accountId) return false;
    if (groupIdB64.length === 0 || entry.id.length === 0) return false;
    const prev = this.of(groupIdB64);
    const next = foldCallLogEntry(prev, entry);
    if (next === prev) return false;
    this.rows = new Map(this.rows).set(groupIdB64, next);
    this.notify();
    this.scheduleSave(accountId);
    return true;
  }

  /**
   * Record one finished call that belongs to no conversation. One row per
   * call id, the first one kept: a record for a call id already held is
   * dropped. Returns `true` when something changed. Dropped for an account
   * the store is not bound to, like {@link record}.
   */
  recordStandalone(accountId: string, entry: StandaloneCallEntry): boolean {
    if (this.account !== accountId) return false;
    if (entry.id.length === 0) return false;
    const prev = this.standaloneRows;
    const next = foldStandaloneCallEntry(prev, entry);
    if (next === prev) return false;
    this.standaloneRows = next;
    this.notify();
    this.scheduleSave(accountId);
    return true;
  }

  /** Drop a conversation's rows (the chat was deleted or left). */
  forgetGroup(accountId: string, groupIdB64: string): void {
    if (this.account !== accountId || !this.rows.has(groupIdB64)) return;
    const next = new Map(this.rows);
    next.delete(groupIdB64);
    this.rows = next;
    this.notify();
    this.scheduleSave(accountId);
  }

  /** Resolves once every save scheduled so far has settled (tests). */
  flush(): Promise<void> {
    return this.chain;
  }

  /** Called on any change. */
  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  private scheduleSave(accountId: string): void {
    if (this.saveQueued) return;
    this.saveQueued = true;
    this.chain = this.chain
      .then(async () => {
        await this.loading;
        this.saveQueued = false;
        // Snapshot at run time so a burst of records costs one write.
        if (this.account !== accountId) return;
        await this.backend.save(accountId, { groups: this.rows, standalone: this.standaloneRows });
      })
      .catch((err: unknown) => {
        this.saveQueued = false;
        this.onPersistError(err);
      });
  }

  private notify(): void {
    for (const fn of this.listeners) fn();
  }
}
