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
 * - **Join / Leave** update the record's `members` only for a record whose
 *   `call_id` matches (C-12): a body naming an unknown call is dropped, so a
 *   member cannot poison a live call's roster with a fabricated id.
 * - **Members are keyed on the sender's signature key, never a leaf index.**
 *   The key is resolved from the MLS-authenticated sender leaf AT RECEIPT,
 *   against the roster of the epoch the body was sealed at (C-68): OpenMLS
 *   hands a vacated leaf to the next member added, so a leaf-keyed roster let
 *   a newcomer inherit a departed member's place in a call. A body whose
 *   sender cannot be resolved is dropped.
 * - **Heartbeat.** A device that is in a group call and has published its
 *   `GroupCallJoin` re-publishes the same Join body every
 *   {@link GROUP_CALL_HEARTBEAT_MS} until it leaves, ends, or its session is
 *   torn down. No new wire body.
 * - A Join sets or advances the member's `lastSeenMs` (the DS row timestamp of
 *   its newest Join / heartbeat) and never lowers it; a Leave removes the
 *   member; **End** for a `call_id` clears that record.
 * - A member is **fresh** while `now - lastSeenMs <=`
 *   {@link GROUP_CALL_MEMBER_FRESH_MS} (two heartbeats plus a worst-case poll).
 *   A participant whose app was killed never publishes a Leave; its entry
 *   simply stops being fresh, so nobody keeps announcing a call nobody is in.
 *   Every count and every "is somebody in the call" check uses fresh members
 *   only.
 * - A record is **live** when it started within the last
 *   {@link GROUP_CALL_MAX_AGE_MS} AND (it has a fresh member OR it started
 *   within the last {@link GROUP_CALL_EMPTY_GRACE_MS}). The grace covers the
 *   moment between the initiator's Start and its own Join.
 * - When several records are live, **the** live call is the one with the
 *   lowest `(created_at_ms, call_id hex)` — so two members who started calls
 *   at the same moment converge on one on every device.
 *
 * `startedAtMs` (liveness) is the initiator's `created_at_ms` bounded by the
 * DS row timestamp, so a sender's clock cannot keep a call "recent" forever.
 * The tie-break uses the raw `created_at_ms` as the contract states; the
 * DS-bounded value is a per-row quantity other clients may bound differently.
 *
 * Web keys `members` on the leaf's own (device) signature key and carries the
 * leaf's person key (ADR-0050 account key) beside it, so two devices of one
 * person are two entries — one device leaving does not remove the other — yet
 * count as one person in the call.
 *
 * **This device's own entry.** A device that is NOT in a call (no active
 * group-call slot for that `call_id`) ignores its own device key in liveness
 * and in the people count: after a reload its own dead session must never
 * read as "1 person in the call". While in the call it counts as any member.
 * Only the device key is ignored — another device of the same person still
 * counts. See {@link GroupCallViewer}.
 *
 * Pure: no clock reads, no I/O. The caller supplies every timestamp.
 */

/** A call older than this is never live, whatever its roster says. */
export const GROUP_CALL_MAX_AGE_MS = 4 * 60 * 60 * 1000;

/** A call with nobody joined stays live for this long after it started. */
export const GROUP_CALL_EMPTY_GRACE_MS = 60_000;

/** How often a device in a call re-publishes its `GroupCallJoin`. */
export const GROUP_CALL_HEARTBEAT_MS = 60_000;

/** A joined member counts while its newest Join is at most this old:
 *  two heartbeats plus a 30 s worst-case poll. */
export const GROUP_CALL_MEMBER_FRESH_MS = 2 * GROUP_CALL_HEARTBEAT_MS + 30_000;

/** How far BEFORE the DS row timestamp an initiator's `created_at_ms` may
 *  claim to be and still be believed. Nothing may claim to be after it. */
export const GROUP_CALL_START_SKEW_MS = 5 * 60_000;

/** Upper bound on retained records, so a member spamming starts cannot grow
 *  the map without limit. The newest are kept. */
export const GROUP_CALL_MAX_RECORDS = 16;

/** One joined member of a call. */
export interface GroupCallMember {
  /** DS timestamp of this member's newest Join / heartbeat. */
  readonly lastSeenMs: number;
  /** The person the device belongs to (base64), for counting people. */
  readonly personKey: string;
}

/** One call this chat has heard about. */
export interface GroupCallRecord {
  /** Lowercase hex of the 16-byte `call_id` — the record key. */
  readonly callIdHex: string;
  readonly mediaAudio: boolean;
  readonly mediaVideo: boolean;
  readonly initiatorLeaf: number;
  /** The person (base64 account key) whose device announced the call,
   *  resolved from the MLS sender at receipt (C-68); `null` when it could
   *  not be resolved. Only this person may end the call for everyone
   *  (`docs/ux/calls.md` section 4.1.2). */
  readonly initiatorPersonKey: string | null;
  /** `created_at_ms` exactly as the initiator stamped it (tie-break). */
  readonly createdAtMs: number;
  /** DS-bounded start time (liveness). */
  readonly startedAtMs: number;
  /** Joined members keyed on their signature key (base64), resolved at
   *  receipt. Includes stale entries; read through {@link freshMemberKeys}. */
  readonly members: ReadonlyMap<string, GroupCallMember>;
  /** Newest DS timestamp of any body that touched the record. */
  readonly lastActivityMs: number;
}

/** Records keyed by `callIdHex`. */
export type GroupCallRecords = ReadonlyMap<string, GroupCallRecord>;

/** A group-control body, reduced to what the record needs. `observedAtMs` is
 *  the DS row timestamp (or the receipt time when that cannot be read).
 *  `memberKey` / `personKey` are the sender's keys resolved at receipt; the
 *  caller drops a body whose sender did not resolve before it gets here. */
export type GroupCallEvent =
  | {
      readonly kind: "start";
      readonly callIdHex: string;
      readonly initiatorLeaf: number;
      readonly initiatorPersonKey: string | null;
      readonly createdAtMs: number;
      readonly mediaAudio: boolean;
      readonly mediaVideo: boolean;
      readonly observedAtMs: number;
    }
  | {
      readonly kind: "join";
      readonly callIdHex: string;
      readonly memberKey: string;
      readonly personKey: string;
      readonly observedAtMs: number;
    }
  | {
      readonly kind: "leave";
      readonly callIdHex: string;
      readonly memberKey: string;
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
        initiatorPersonKey: event.initiatorPersonKey,
        createdAtMs: event.createdAtMs,
        startedAtMs: boundStartedAt(event.createdAtMs, event.observedAtMs),
        members: new Map<string, GroupCallMember>(),
        lastActivityMs: event.observedAtMs,
      });
      return capRecords(next);
    }
    case "join": {
      const rec = records.get(event.callIdHex);
      // C-12: a body for a call this chat never saw start is dropped.
      if (rec === undefined) return records;
      if (event.memberKey.length === 0) return records;
      const prev = rec.members.get(event.memberKey);
      const lastSeenMs =
        prev === undefined ? event.observedAtMs : Math.max(prev.lastSeenMs, event.observedAtMs);
      const lastActivityMs = Math.max(rec.lastActivityMs, event.observedAtMs);
      if (
        prev !== undefined &&
        prev.lastSeenMs === lastSeenMs &&
        prev.personKey === event.personKey &&
        rec.lastActivityMs === lastActivityMs
      ) {
        return records;
      }
      const members = new Map(rec.members);
      members.set(event.memberKey, { lastSeenMs, personKey: event.personKey });
      const next = new Map(records);
      next.set(rec.callIdHex, { ...rec, members, lastActivityMs });
      return next;
    }
    case "leave": {
      const rec = records.get(event.callIdHex);
      if (rec === undefined) return records;
      const lastActivityMs = Math.max(rec.lastActivityMs, event.observedAtMs);
      if (!rec.members.has(event.memberKey)) {
        if (lastActivityMs === rec.lastActivityMs) return records;
        const next = new Map(records);
        next.set(rec.callIdHex, { ...rec, lastActivityMs });
        return next;
      }
      const members = new Map(rec.members);
      members.delete(event.memberKey);
      const next = new Map(records);
      next.set(rec.callIdHex, { ...rec, members, lastActivityMs });
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

/** Who is looking at the records: this device's own signature key and the
 *  call it is currently in (`null` when none). */
export interface GroupCallViewer {
  readonly ownMemberKey: string;
  readonly inCallIdHex: string | null;
}

/** Whether a member's newest Join is recent enough to count. */
export function isMemberFresh(member: GroupCallMember, nowMs: number): boolean {
  return nowMs - member.lastSeenMs <= GROUP_CALL_MEMBER_FRESH_MS;
}

/** Whether `key`'s entry counts for `viewer`: this device's own entry is
 *  ignored unless this device is in THIS call. */
function countsFor(rec: GroupCallRecord, key: string, viewer: GroupCallViewer | undefined): boolean {
  return (
    viewer === undefined ||
    key !== viewer.ownMemberKey ||
    viewer.inCallIdHex === rec.callIdHex
  );
}

/** The signature keys of the members that count at `nowMs`. Without a
 *  `viewer` nothing is excluded (the leave plan excludes the own device key
 *  itself). */
export function freshMemberKeys(
  rec: GroupCallRecord,
  nowMs: number,
  viewer?: GroupCallViewer,
): ReadonlySet<string> {
  const out = new Set<string>();
  for (const [key, member] of rec.members) {
    if (isMemberFresh(member, nowMs) && countsFor(rec, key, viewer)) out.add(key);
  }
  return out;
}

/** How many PEOPLE are in the call at `nowMs` (fresh members, one per
 *  person however many of their devices joined). */
export function freshParticipantCount(
  rec: GroupCallRecord,
  nowMs: number,
  viewer?: GroupCallViewer,
): number {
  const people = new Set<string>();
  for (const [key, member] of rec.members) {
    if (isMemberFresh(member, nowMs) && countsFor(rec, key, viewer)) people.add(member.personKey);
  }
  return people.size;
}

/** The liveness rule — see the module header. */
export function isGroupCallLive(
  rec: GroupCallRecord,
  nowMs: number,
  viewer?: GroupCallViewer,
): boolean {
  const age = nowMs - rec.startedAtMs;
  if (age > GROUP_CALL_MAX_AGE_MS) return false;
  if (age <= GROUP_CALL_EMPTY_GRACE_MS) return true;
  for (const [key, member] of rec.members) {
    if (isMemberFresh(member, nowMs) && countsFor(rec, key, viewer)) return true;
  }
  return false;
}

/** The one live call: lowest `(createdAtMs, callIdHex)` among live records. */
export function selectLiveGroupCall(
  records: GroupCallRecords,
  nowMs: number,
  viewer?: GroupCallViewer,
): GroupCallRecord | null {
  let best: GroupCallRecord | null = null;
  for (const rec of records.values()) {
    if (!isGroupCallLive(rec, nowMs, viewer)) continue;
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
    [...records.values()].map((r) => ({ ...r, members: [...r.members.entries()] })),
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
    const rawMembers = r["members"];
    const nums = ["initiatorLeaf", "createdAtMs", "startedAtMs", "lastActivityMs"];
    if (
      typeof hex !== "string" ||
      hexToBytes(hex)?.length !== 16 ||
      typeof r["mediaAudio"] !== "boolean" ||
      typeof r["mediaVideo"] !== "boolean" ||
      !nums.every((k) => typeof r[k] === "number" && Number.isFinite(r[k])) ||
      !Array.isArray(rawMembers)
    ) {
      continue;
    }
    const members = new Map<string, GroupCallMember>();
    let membersOk = true;
    for (const entry of rawMembers as unknown[]) {
      const pair = Array.isArray(entry) ? (entry as unknown[]) : null;
      const key = pair?.[0];
      const value = pair?.[1] as Record<string, unknown> | null | undefined;
      if (
        pair === null ||
        pair.length !== 2 ||
        typeof key !== "string" ||
        key.length === 0 ||
        typeof value !== "object" ||
        value === null ||
        typeof value["lastSeenMs"] !== "number" ||
        !Number.isFinite(value["lastSeenMs"]) ||
        typeof value["personKey"] !== "string"
      ) {
        membersOk = false;
        break;
      }
      members.set(key, {
        lastSeenMs: value["lastSeenMs"] as number,
        personKey: value["personKey"] as string,
      });
    }
    if (!membersOk) continue;
    out.set(hex, {
      callIdHex: hex,
      mediaAudio: r["mediaAudio"] as boolean,
      mediaVideo: r["mediaVideo"] as boolean,
      initiatorLeaf: r["initiatorLeaf"] as number,
      initiatorPersonKey:
        typeof r["initiatorPersonKey"] === "string" && r["initiatorPersonKey"].length > 0
          ? r["initiatorPersonKey"]
          : null,
      createdAtMs: r["createdAtMs"] as number,
      startedAtMs: r["startedAtMs"] as number,
      members,
      lastActivityMs: r["lastActivityMs"] as number,
    });
  }
  return capRecords(out);
}
