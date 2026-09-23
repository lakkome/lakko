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

import type { CallLogEntry, CallOutcome } from "./call-log";

/** How many rows are kept per conversation, newest first. */
export const CALL_LOG_MAX_PER_GROUP = 200;

/** All groups' call rows for one account, each list oldest-first. */
export type CallLogRows = ReadonlyMap<string, readonly CallLogEntry[]>;

const EMPTY: readonly CallLogEntry[] = [];

const OUTCOMES: readonly CallOutcome[] = ["outgoing", "incoming", "missed", "declined"];

// ---------------------------------------------------------------------------
// Row codec
// ---------------------------------------------------------------------------

/** Schema version of the JSON payload inside the encrypted row. */
export const CALL_LOG_ROW_VERSION = 1;

/** The JSON shape inside the ciphertext. */
export interface CallLogRowPayload {
  readonly v: 1;
  /** base64 group id -> the group's rows, oldest first. */
  readonly g: Record<string, readonly CallLogEntry[]>;
}

/** Encode the rows to the payload JSON. */
export function encodeCallLogRow(rows: CallLogRows): string {
  const g: Record<string, readonly CallLogEntry[]> = {};
  for (const [groupIdB64, list] of rows) {
    if (list.length > 0) g[groupIdB64] = list;
  }
  const payload: CallLogRowPayload = { v: CALL_LOG_ROW_VERSION, g };
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

/** Decode payload JSON, dropping any malformed entry rather than trusting it.
 *  `null` for a payload that is not this schema at all. */
export function decodeCallLogRow(json: string): Map<string, readonly CallLogEntry[]> | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const obj = parsed as Partial<CallLogRowPayload>;
  if (obj.v !== CALL_LOG_ROW_VERSION) return null;
  const rows = new Map<string, readonly CallLogEntry[]>();
  if (typeof obj.g !== "object" || obj.g === null) return rows;
  for (const [groupIdB64, raw] of Object.entries(obj.g)) {
    if (groupIdB64.length === 0 || !Array.isArray(raw)) continue;
    const list: CallLogEntry[] = [];
    for (const item of raw) {
      const entry = decodeEntry(item);
      if (entry !== null) list.push(entry);
    }
    if (list.length > 0) rows.set(groupIdB64, sortAndCap(list));
  }
  return rows;
}

// ---------------------------------------------------------------------------
// Folding
// ---------------------------------------------------------------------------

/** Oldest first, newest kept when the cap bites, and one row per call id. */
function sortAndCap(list: readonly CallLogEntry[]): readonly CallLogEntry[] {
  const byId = new Map<string, CallLogEntry>();
  for (const entry of list) byId.set(entry.id, entry);
  const sorted = [...byId.values()].sort((a, b) => a.startedAtMs - b.startedAtMs);
  return sorted.length <= CALL_LOG_MAX_PER_GROUP
    ? sorted
    : sorted.slice(sorted.length - CALL_LOG_MAX_PER_GROUP);
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
  if (
    existing !== undefined &&
    existing.outcome === entry.outcome &&
    existing.video === entry.video &&
    existing.startedAtMs === entry.startedAtMs &&
    existing.durationMs === entry.durationMs
  ) {
    return list;
  }
  return sortAndCap([...list.filter((e) => e.id !== entry.id), entry]);
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

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/** Where the rows live between sessions. One row per account. */
export interface CallLogBackend {
  load(accountId: string): Promise<CallLogRows | null>;
  save(accountId: string, rows: CallLogRows): Promise<void>;
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
      this.notify();
    }
    const attempt = this.load(accountId);
    this.loading = attempt;
    return attempt;
  }

  private async load(accountId: string): Promise<void> {
    let loaded: CallLogRows | null = null;
    try {
      loaded = await this.backend.load(accountId);
    } catch (err) {
      this.onPersistError(err);
    }
    // The session may have moved on while the load was in flight.
    if (this.account !== accountId || loaded === null) return;
    const next = new Map<string, readonly CallLogEntry[]>(loaded);
    for (const [groupIdB64, list] of this.rows) {
      next.set(groupIdB64, mergeCallLogEntries(next.get(groupIdB64) ?? EMPTY, list));
    }
    this.rows = next;
    this.notify();
  }

  /** Forget everything in memory (sign-out). The row on disk stays. */
  close(): void {
    this.account = null;
    this.rows = new Map();
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
        await this.backend.save(accountId, this.rows);
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
