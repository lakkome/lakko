// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Per-chat record of ongoing group calls, folded from the group-control
 * signaling bodies (`GroupCallStart` / `GroupCallJoin` / `GroupCallLeave`,
 * with `CallEnd` reused as the terminal event).
 *
 * ## The contract (shared with the iOS and Android clients)
 *
 * - **Start** creates a record for its `call_id`. A repeat of a start for a
 *   record that already exists changes nothing.
 * - **Join / Leave** update `joinedLeaves` only for a record whose `call_id`
 *   matches (C-12): a body naming an unknown call is dropped, so a member
 *   cannot poison a live call's roster with a fabricated id.
 * - **End** for a `call_id` clears that record.
 * - A record is **live** when it started within the last
 *   {@link GROUP_CALL_MAX_AGE_MS} AND (somebody is joined OR it started within
 *   the last {@link GROUP_CALL_EMPTY_GRACE_MS}). The grace covers the moment
 *   between the initiator's Start and its own Join.
 * - When several records are live, **the** live call is the one with the
 *   lowest `(created_at_ms, call_id hex)` — so two members who started calls
 *   at the same moment converge on one on every device.
 *
 * `startedAtMs` (liveness) is the initiator's `created_at_ms` bounded by the
 * DS row timestamp, so a sender's clock cannot keep a call "recent" forever.
 * The tie-break uses the raw `created_at_ms` as the contract states; the
 * DS-bounded value is a per-row quantity other clients may bound differently.
 *
 * Pure: no clock reads, no I/O. The caller supplies every timestamp.
 */

/** A call older than this is never live, whatever its roster says. */
export const GROUP_CALL_MAX_AGE_MS = 4 * 60 * 60 * 1000;

/** A call with nobody joined stays live for this long after it started. */
export const GROUP_CALL_EMPTY_GRACE_MS = 60_000;

/** How far BEFORE the DS row timestamp an initiator's `created_at_ms` may
 *  claim to be and still be believed. Nothing may claim to be after it. */
export const GROUP_CALL_START_SKEW_MS = 5 * 60_000;

/** Upper bound on retained records, so a member spamming starts cannot grow
 *  the map without limit. The newest are kept. */
export const GROUP_CALL_MAX_RECORDS = 16;

/** One call this chat has heard about. */
export interface GroupCallRecord {
  /** Lowercase hex of the 16-byte `call_id` — the record key. */
  readonly callIdHex: string;
  readonly mediaAudio: boolean;
  readonly mediaVideo: boolean;
  readonly initiatorLeaf: number;
  /** `created_at_ms` exactly as the initiator stamped it (tie-break). */
  readonly createdAtMs: number;
  /** DS-bounded start time (liveness). */
  readonly startedAtMs: number;
  readonly joinedLeaves: ReadonlySet<number>;
  /** Newest DS timestamp of any body that touched the record. */
  readonly lastActivityMs: number;
}

/** Records keyed by `callIdHex`. */
export type GroupCallRecords = ReadonlyMap<string, GroupCallRecord>;

/** A group-control body, reduced to what the record needs. `observedAtMs` is
 *  the DS row timestamp (or the receipt time when that cannot be read). */
export type GroupCallEvent =
  | {
      readonly kind: "start";
      readonly callIdHex: string;
      readonly initiatorLeaf: number;
      readonly createdAtMs: number;
      readonly mediaAudio: boolean;
      readonly mediaVideo: boolean;
      readonly observedAtMs: number;
    }
  | {
      readonly kind: "join" | "leave";
      readonly callIdHex: string;
      readonly leafIndex: number;
      readonly observedAtMs: number;
    }
  | { readonly kind: "end"; readonly callIdHex: string };

/** The empty record set. */
export const NO_GROUP_CALLS: GroupCallRecords = new Map();

/**
 * Bound an initiator-claimed start time by the DS row timestamp: believed
 * within `[observedAt - skew, observedAt]`, otherwise the row timestamp
 * stands. A zero / non-finite claim is not a claim.
 */
export function boundStartedAt(createdAtMs: number, observedAtMs: number): number {
  if (!Number.isFinite(createdAtMs) || createdAtMs <= 0) return observedAtMs;
  if (createdAtMs > observedAtMs) return observedAtMs;
  if (createdAtMs < observedAtMs - GROUP_CALL_START_SKEW_MS) return observedAtMs;
  return createdAtMs;
}

/**
 * Fold one event. Returns the SAME map instance when nothing changed, so a
 * caller can skip a signal write on an ignored body.
 */
export function reduceGroupCalls(
  records: GroupCallRecords,
  event: GroupCallEvent,
): GroupCallRecords {
  switch (event.kind) {
    case "start": {
      if (records.has(event.callIdHex)) return records;
      const next = new Map(records);
      next.set(event.callIdHex, {
        callIdHex: event.callIdHex,
        mediaAudio: event.mediaAudio,
        mediaVideo: event.mediaVideo,
        initiatorLeaf: event.initiatorLeaf,
        createdAtMs: event.createdAtMs,
        startedAtMs: boundStartedAt(event.createdAtMs, event.observedAtMs),
        joinedLeaves: new Set<number>(),
        lastActivityMs: event.observedAtMs,
      });
      return capRecords(next);
    }
    case "join":
    case "leave": {
      const rec = records.get(event.callIdHex);
      // C-12: a body for a call this chat never saw start is dropped.
      if (rec === undefined) return records;
      const has = rec.joinedLeaves.has(event.leafIndex);
      if ((event.kind === "join") === has) {
        // Already in the state the body asks for; only activity moves.
        if (event.observedAtMs <= rec.lastActivityMs) return records;
        const next = new Map(records);
        next.set(rec.callIdHex, { ...rec, lastActivityMs: event.observedAtMs });
        return next;
      }
      const leaves = new Set(rec.joinedLeaves);
      if (event.kind === "join") leaves.add(event.leafIndex);
      else leaves.delete(event.leafIndex);
      const next = new Map(records);
      next.set(rec.callIdHex, {
        ...rec,
        joinedLeaves: leaves,
        lastActivityMs: Math.max(rec.lastActivityMs, event.observedAtMs),
      });
      return next;
    }
    case "end": {
      if (!records.has(event.callIdHex)) return records;
      const next = new Map(records);
      next.delete(event.callIdHex);
      return next;
    }
  }
}

/** The liveness rule — see the module header. */
export function isGroupCallLive(rec: GroupCallRecord, nowMs: number): boolean {
  const age = nowMs - rec.startedAtMs;
  if (age > GROUP_CALL_MAX_AGE_MS) return false;
  return rec.joinedLeaves.size > 0 || age <= GROUP_CALL_EMPTY_GRACE_MS;
}

/** The one live call: lowest `(createdAtMs, callIdHex)` among live records. */
export function selectLiveGroupCall(
  records: GroupCallRecords,
  nowMs: number,
): GroupCallRecord | null {
  let best: GroupCallRecord | null = null;
  for (const rec of records.values()) {
    if (!isGroupCallLive(rec, nowMs)) continue;
    if (
      best === null ||
      rec.createdAtMs < best.createdAtMs ||
      (rec.createdAtMs === best.createdAtMs && rec.callIdHex < best.callIdHex)
    ) {
      best = rec;
    }
  }
  return best;
}

/** Drop records past the maximum age. Same instance when nothing dropped. */
export function pruneGroupCalls(
  records: GroupCallRecords,
  nowMs: number,
): GroupCallRecords {
  let next: Map<string, GroupCallRecord> | null = null;
  for (const [key, rec] of records) {
    if (nowMs - rec.startedAtMs > GROUP_CALL_MAX_AGE_MS) {
      next ??= new Map(records);
      next.delete(key);
    }
  }
  return next ?? records;
}

function capRecords(records: Map<string, GroupCallRecord>): GroupCallRecords {
  if (records.size <= GROUP_CALL_MAX_RECORDS) return records;
  const newest = [...records.values()]
    .sort((a, b) => b.startedAtMs - a.startedAtMs)
    .slice(0, GROUP_CALL_MAX_RECORDS);
  return new Map(newest.map((r) => [r.callIdHex, r]));
}

/** Hex string → bytes. `null` for anything that is not an even-length hex
 *  string. */
export function hexToBytes(hex: string): Uint8Array | null {
  if (hex.length % 2 !== 0 || !/^[0-9a-f]*$/.test(hex)) return null;
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

/** Serialise for tab-scoped storage, so a reload does not forget a call whose
 *  Start the persisted cursor has already moved past. */
export function serializeGroupCalls(records: GroupCallRecords): string {
  return JSON.stringify(
    [...records.values()].map((r) => ({ ...r, joinedLeaves: [...r.joinedLeaves] })),
  );
}

/** Inverse of {@link serializeGroupCalls}. Anything malformed is skipped, and
 *  input that is not a JSON array yields the empty set. */
export function deserializeGroupCalls(raw: string | null): GroupCallRecords {
  if (raw === null) return NO_GROUP_CALLS;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return NO_GROUP_CALLS;
  }
  if (!Array.isArray(parsed)) return NO_GROUP_CALLS;
  const out = new Map<string, GroupCallRecord>();
  for (const item of parsed as unknown[]) {
    if (typeof item !== "object" || item === null) continue;
    const r = item as Record<string, unknown>;
    const hex = r["callIdHex"];
    const leaves = r["joinedLeaves"];
    const nums = ["initiatorLeaf", "createdAtMs", "startedAtMs", "lastActivityMs"];
    if (
      typeof hex !== "string" ||
      hexToBytes(hex)?.length !== 16 ||
      typeof r["mediaAudio"] !== "boolean" ||
      typeof r["mediaVideo"] !== "boolean" ||
      !nums.every((k) => typeof r[k] === "number" && Number.isFinite(r[k])) ||
      !Array.isArray(leaves) ||
      !leaves.every((l) => Number.isInteger(l) && (l as number) >= 0)
    ) {
      continue;
    }
    out.set(hex, {
      callIdHex: hex,
      mediaAudio: r["mediaAudio"] as boolean,
      mediaVideo: r["mediaVideo"] as boolean,
      initiatorLeaf: r["initiatorLeaf"] as number,
      createdAtMs: r["createdAtMs"] as number,
      startedAtMs: r["startedAtMs"] as number,
      joinedLeaves: new Set(leaves as number[]),
      lastActivityMs: r["lastActivityMs"] as number,
    });
  }
  return capRecords(out);
}
