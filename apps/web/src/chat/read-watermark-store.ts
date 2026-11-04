// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0024 — the durable per-`(account, group)` read watermarks.
 *
 * A read receipt says "member M has read everything up to message id N". The
 * chat view used to fold them into route-local state, which lost them on every
 * reload, and a receipt the background receive applied while another chat was
 * open never reached a view at all. The DS poll resumes past receipts it has
 * already consumed, so nothing replays them: a message both other members had
 * read kept showing "Sent".
 *
 * This store is the one place a receipt lands, from the foreground chat view
 * and the background receive alike, whether or not the chat is open:
 *
 * ```text
 *   groupIdB64 -> { senderSignatureKeyB64 -> readUpTo }
 * ```
 *
 * Keyed on the sender's signature key, never a leaf index (C-68): the value is
 * monotone and never reset on a membership change, so a leaf key would hand a
 * departed member's read history to whoever takes the slot. Monotone: every
 * write goes through {@link foldReadWatermark}, including the merge of a row
 * loaded from disk with receipts that arrived before the load settled, so a
 * lower value never lowers a higher one.
 *
 * Free of wasm, Solid and IndexedDB so vitest can pin it. The encrypted
 * IndexedDB backend is `read-watermark-idb.ts`; the session runtime is
 * `ReadWatermarkContext.tsx`.
 *
 * Size is bounded by construction (one entry per sender per group), so there
 * is no pruning.
 */

import type { MemberIdentity } from "./author-identity";
import { foldReadWatermark, type ReadWatermarkMap } from "./presence-ops";

/** All groups' watermarks for one account. */
export type ReadWatermarkRows = ReadonlyMap<string, ReadWatermarkMap>;

// ---------------------------------------------------------------------------
// Row codec
// ---------------------------------------------------------------------------

/** Schema version of the JSON payload inside the encrypted row. */
export const READ_WATERMARK_ROW_VERSION = 1;

/** The JSON shape inside the ciphertext. */
export interface ReadWatermarkRowPayload {
  readonly v: 1;
  /** base64 group id -> (sender signature key b64 -> readUpTo). */
  readonly g: Record<string, Record<string, number>>;
}

/** Encode the rows to the payload JSON. */
export function encodeReadWatermarkRow(rows: ReadWatermarkRows): string {
  const g: Record<string, Record<string, number>> = {};
  for (const [groupIdB64, map] of rows) {
    const members: Record<string, number> = {};
    for (const [member, upTo] of map) members[member] = upTo;
    g[groupIdB64] = members;
  }
  const payload: ReadWatermarkRowPayload = { v: READ_WATERMARK_ROW_VERSION, g };
  return JSON.stringify(payload);
}

/** Decode payload JSON, dropping any malformed entry rather than trusting it.
 *  `null` for a payload that is not this schema at all. */
export function decodeReadWatermarkRow(json: string): Map<string, ReadWatermarkMap> | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const obj = parsed as Partial<ReadWatermarkRowPayload>;
  if (obj.v !== READ_WATERMARK_ROW_VERSION) return null;
  const rows = new Map<string, ReadWatermarkMap>();
  if (typeof obj.g !== "object" || obj.g === null) return rows;
  for (const [groupIdB64, raw] of Object.entries(obj.g)) {
    if (typeof raw !== "object" || raw === null) continue;
    const map: ReadWatermarkMap = new Map();
    for (const [member, upTo] of Object.entries(raw as Record<string, unknown>)) {
      if (member.length === 0) continue;
      if (typeof upTo !== "number" || !Number.isSafeInteger(upTo) || upTo < 0) continue;
      map.set(member, upTo);
    }
    if (map.size > 0) rows.set(groupIdB64, map);
  }
  return rows;
}

/** Fold every entry of `from` into `into` monotonically, returning a NEW map
 *  (or `into` itself when nothing advanced). */
export function mergeReadWatermarks(into: ReadWatermarkMap, from: ReadWatermarkMap): ReadWatermarkMap {
  let out = into;
  for (const [member, upTo] of from) out = foldReadWatermark(out, member, upTo);
  return out;
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/** Where the rows live between sessions. One row per account. */
export interface ReadWatermarkBackend {
  load(accountId: string): Promise<ReadWatermarkRows | null>;
  save(accountId: string, rows: ReadWatermarkRows): Promise<void>;
}

/** A backend that keeps nothing (tests, a device with no key). */
export const MEMORY_READ_WATERMARK_BACKEND: ReadWatermarkBackend = {
  load: async () => null,
  save: async () => undefined,
};

const EMPTY: ReadWatermarkMap = new Map();

/**
 * The in-memory view of one account's read watermarks, written through to
 * the backend. Every write is a monotone fold.
 */
export class ReadWatermarkStore {
  private rows = new Map<string, ReadWatermarkMap>();
  private account: string | null = null;
  private readonly listeners = new Set<() => void>();
  private chain: Promise<void> = Promise.resolve();
  /** A save is queued and has not started yet: a further change rides it. */
  private saveQueued = false;
  /** The in-flight load. A save waits for it, so a receipt that arrives
   *  during the load can never overwrite the stored row with a partial one. */
  private loading: Promise<void> = Promise.resolve();

  constructor(
    private readonly backend: ReadWatermarkBackend,
    private readonly onPersistError: (err: unknown) => void = () => undefined,
  ) {}

  /** Bind to an account and load its row. Receipts that landed before the
   *  load settled are folded over the loaded row, never replaced by it. */
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
    let loaded: ReadWatermarkRows | null = null;
    try {
      loaded = await this.backend.load(accountId);
    } catch (err) {
      this.onPersistError(err);
    }
    // The session may have moved on while the load was in flight.
    if (this.account !== accountId || loaded === null) return;
    const next = new Map<string, ReadWatermarkMap>(loaded);
    for (const [groupIdB64, map] of this.rows) {
      next.set(groupIdB64, mergeReadWatermarks(next.get(groupIdB64) ?? EMPTY, map));
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

  /** The watermarks for a group (an empty map when none). Never mutate it. */
  of(groupIdB64: string): ReadWatermarkMap {
    return this.rows.get(groupIdB64) ?? EMPTY;
  }

  /**
   * Fold one receipt. Returns `true` when the stored value advanced (and a
   * save was scheduled). A receipt for an account the store is not bound to
   * is dropped — it must never land in another account's row.
   */
  ingest(accountId: string, groupIdB64: string, sender: MemberIdentity, upTo: number): boolean {
    if (this.account !== accountId) return false;
    if (sender.length === 0 || !Number.isSafeInteger(upTo) || upTo < 0) return false;
    const prev = this.of(groupIdB64);
    const next = foldReadWatermark(prev, sender, upTo);
    if (next === prev) return false;
    this.rows = new Map(this.rows).set(groupIdB64, next);
    this.notify();
    this.scheduleSave(accountId);
    return true;
  }

  /** Drop a group's watermarks (ADR-0087 section 9: the group is gone from
   *  this device) and schedule the save. */
  forget(accountId: string, groupIdB64: string): void {
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
        // Snapshot at run time so a burst of receipts costs one write.
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
