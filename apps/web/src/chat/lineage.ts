// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 "Phase 2 revised", R.5 - one conversation over several groups.
 *
 * A conversation that cannot be repaired in place (a one-to-one whose other
 * person reset their keys, a group whose owner did) continues in a NEW MLS
 * group. The person sees one conversation: the same row, the history above,
 * the same pin / mute / archive / folder / draft / read position. That is a
 * stable **conversation id** with a **lineage** of groups, not a re-key of
 * any store:
 *
 *   * the conversation id is the group id of the lineage's FIRST group (the
 *     root). Everything a person attaches to a conversation is keyed on it
 *     and never rewritten;
 *   * the segments are the lineage's groups in order. The message log stays
 *     per group; the timeline is the segments concatenated, ordered by
 *     `(segment index, DS message id)`, since a successor's ids restart;
 *   * everything that belongs to one MLS session stays per group (the
 *     handle, the journal cursor, push routing, shared pins, watermarks,
 *     proposals, the outbox). "The conversation's current group" is the
 *     lineage head: sends, calls and the push subscription use it.
 *
 * The lineage itself is the write-once register `successor group id -> root
 * conversation id` of the ADR-0058 synced state (`lakko-sync`), which this
 * module reads as plain values. Ids are lower-case hex, the form the synced
 * state's projection uses. Free of wasm, Solid and storage; iOS and Android
 * mirror this (R.12).
 */

/** One lineage register, as projected from the synced state. */
export interface LineageEntry {
  /** Hex of the group that continues the conversation. */
  readonly successorHex: string;
  /** Hex of the conversation id (the lineage's first group). */
  readonly rootHex: string;
  /** The HLC stamp the register was written at: the only thing that
   *  carries segment order (R.13a). */
  readonly hlc: bigint;
}

/** The lineage of every conversation of the account. */
export interface Lineage {
  /** The conversation a group belongs to: its root, or itself. */
  conversationOf(groupHex: string): string;
  /** A conversation's groups in order: the root, then its successors by
   *  stamp (ties broken on the successor id, so every device agrees). */
  segmentsOf(conversationHex: string): readonly string[];
  /** The index of a group in its conversation's segments. */
  segmentIndexOf(groupHex: string): number;
  /** Whether a group is some conversation's successor (not a root). */
  isSuccessor(groupHex: string): boolean;
}

/**
 * Build the lineage from the registers.
 *
 * A register whose root is itself a successor (a device wrote the parent
 * instead of the root) is followed to the real root, bounded, so a chain
 * still reads as one conversation; a register naming itself, and a cycle,
 * are ignored.
 */
export function buildLineage(entries: readonly LineageEntry[]): Lineage {
  const rootOfSuccessor = new Map<string, string>();
  const stampOf = new Map<string, bigint>();
  for (const e of entries) {
    if (e.successorHex === e.rootHex) continue;
    rootOfSuccessor.set(e.successorHex, e.rootHex);
    stampOf.set(e.successorHex, e.hlc);
  }
  const resolve = (groupHex: string): string => {
    let current = groupHex;
    const seen = new Set<string>([current]);
    for (;;) {
      const next = rootOfSuccessor.get(current);
      if (next === undefined) return current;
      // A cycle names no root: the group stands alone.
      if (seen.has(next)) return groupHex;
      seen.add(next);
      current = next;
    }
  };
  const segments = new Map<string, string[]>();
  for (const successorHex of rootOfSuccessor.keys()) {
    const root = resolve(successorHex);
    if (root === successorHex) continue;
    const list = segments.get(root) ?? [];
    list.push(successorHex);
    segments.set(root, list);
  }
  for (const [root, list] of segments) {
    list.sort((a, b) => {
      const sa = stampOf.get(a) ?? 0n;
      const sb = stampOf.get(b) ?? 0n;
      if (sa !== sb) return sa < sb ? -1 : 1;
      return a < b ? -1 : a > b ? 1 : 0;
    });
    segments.set(root, [root, ...list]);
  }
  const segmentsOf = (conversationHex: string): readonly string[] =>
    segments.get(conversationHex) ?? [conversationHex];
  return {
    conversationOf: resolve,
    segmentsOf,
    segmentIndexOf(groupHex) {
      const index = segmentsOf(resolve(groupHex)).indexOf(groupHex);
      return index === -1 ? 0 : index;
    },
    isSuccessor(groupHex) {
      return resolve(groupHex) !== groupHex;
    },
  };
}

/**
 * Whether the conversation continued past `groupHex`: the lineage names a
 * later segment. "Already moved" for the rules that stop acting on an old
 * segment (R.5, R.8).
 */
export function laterSegmentExists(lineage: Lineage, groupHex: string): boolean {
  const segments = lineage.segmentsOf(lineage.conversationOf(groupHex));
  const at = segments.indexOf(groupHex);
  return at !== -1 && at < segments.length - 1;
}

/** The empty lineage: every group is its own conversation. */
export const NO_LINEAGE: Lineage = buildLineage([]);

/**
 * "The conversation's current group" (R.5): the LAST segment this device is
 * an active member of. A successor the device has not joined yet is not the
 * head for it: it keeps sending where it is (R.7 case 3) or holds the send
 * (case 1). `null` when the device holds no segment at all.
 *
 * When no segment is active (every membership ended), the last segment the
 * device holds is the head, so the row and its history stay reachable.
 */
export function headSegment(
  segments: readonly string[],
  membership: (groupHex: string) => "active" | "ended" | "absent",
): string | null {
  let lastHeld: string | null = null;
  let lastActive: string | null = null;
  for (const hex of segments) {
    const m = membership(hex);
    if (m === "absent") continue;
    lastHeld = hex;
    if (m === "active") lastActive = hex;
  }
  return lastActive ?? lastHeld;
}

/** A position in a conversation's timeline. */
export interface TimelinePosition {
  readonly segment: number;
  readonly messageId: number;
}

/** Timeline order (R.5): `(segment index, DS message id)`. */
export function compareTimeline(a: TimelinePosition, b: TimelinePosition): number {
  if (a.segment !== b.segment) return a.segment - b.segment;
  return a.messageId - b.messageId;
}

// ---------------------------------------------------------------------------
// The read position, keyed on the conversation
// ---------------------------------------------------------------------------

/**
 * The cross-device read cursor of a conversation is one register keyed on
 * the conversation id. Its value has always been the 8-byte big-endian DS
 * message id; a successor's ids restart, so a position past the first
 * segment also carries the segment:
 *
 *   * segment 0: `message_id(8 BE)`: unchanged, what every build writes;
 *   * segment n > 0: `message_id(8 BE) || segment(4 BE)`.
 *
 * A reader that knows only the 8-byte form reads a position in a later
 * segment as "that message id in the first segment", which can only make it
 * show LESS as read than was read, never more than exists there.
 */
export function encodeReadPosition(position: TimelinePosition): Uint8Array {
  const withSegment = position.segment > 0;
  const out = new Uint8Array(withSegment ? 12 : 8);
  const view = new DataView(out.buffer);
  view.setBigUint64(0, BigInt(Math.max(0, Math.trunc(position.messageId))));
  if (withSegment) view.setUint32(8, position.segment);
  return out;
}

/** Decode a read cursor's bytes; `null` when shorter than a message id. */
export function decodeReadPosition(bytes: Uint8Array): TimelinePosition | null {
  if (bytes.length < 8) return null;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const id = view.getBigUint64(0);
  if (id > BigInt(Number.MAX_SAFE_INTEGER)) return null;
  const segment = bytes.length >= 12 ? view.getUint32(8) : 0;
  return { segment, messageId: Number(id) };
}

/**
 * What the read position means for ONE segment's own message ids: the id up
 * to which that segment is read.
 *
 * | the position is in ...   | this segment is read up to |
 * |--------------------------|----------------------------|
 * | an earlier segment       | nothing (`null`)           |
 * | this segment             | the position's message id  |
 * | a later segment          | everything (`"all"`)       |
 */
export function readUpToInSegment(
  position: TimelinePosition | null,
  segment: number,
): number | "all" | null {
  if (position === null) return null;
  if (position.segment < segment) return null;
  if (position.segment > segment) return "all";
  return position.messageId;
}

// ---------------------------------------------------------------------------
// The conversation list
// ---------------------------------------------------------------------------

/** A group this device holds, as the list planning reads it. */
export interface HeldGroup {
  readonly groupHex: string;
  readonly active: boolean;
}

/** One conversation of the list. */
export interface ConversationPlan {
  /** The conversation id (the root's hex): the row's identity, and the key
   *  of every person-facing store. */
  readonly conversationHex: string;
  /** The group the row opens and sends into. */
  readonly headHex: string;
  /** Every segment this device holds, in timeline order (the head included,
   *  and any successor it holds beyond the head). */
  readonly heldSegments: readonly string[];
}

/**
 * One row per conversation (R.5: "a second one-to-one group with the same
 * account never becomes a second row once it is in the lineage"). Groups the
 * device holds are folded by conversation; the order of the result follows
 * the first held group of each conversation in `held`.
 */
export function planConversations(
  held: readonly HeldGroup[],
  lineage: Lineage,
): ConversationPlan[] {
  const byHex = new Map(held.map((g) => [g.groupHex, g]));
  const out: ConversationPlan[] = [];
  const seen = new Set<string>();
  for (const g of held) {
    const conversationHex = lineage.conversationOf(g.groupHex);
    if (seen.has(conversationHex)) continue;
    seen.add(conversationHex);
    const segments = lineage.segmentsOf(conversationHex);
    const heldSegments = segments.filter((hex) => byHex.has(hex));
    const headHex = headSegment(segments, (hex) => {
      const entry = byHex.get(hex);
      return entry === undefined ? "absent" : entry.active ? "active" : "ended";
    });
    if (headHex === null) continue;
    out.push({ conversationHex, headHex, heldSegments });
  }
  return out;
}

/**
 * R.5 "the old segment after the move": whether an old segment's journal is
 * still polled.
 *
 * | the group is ...                                   | polled |
 * |----------------------------------------------------|--------|
 * | the conversation's head                            | yes    |
 * | an older segment of a ONE-TO-ONE                   | no     |
 * | an older segment of a group, inside the retention  | yes    |
 * | an older segment of a group, past the retention    | no     |
 *
 * A one-to-one's old journal has no sender left but the superseded identity;
 * a group's still carries members who have not moved yet (R.7 case 3), for
 * as long as the DS keeps the journal.
 */
export function oldSegmentPolled(input: {
  readonly isHead: boolean;
  readonly oneToOne: boolean;
  /** When this device learned the conversation moved, UNIX ms; `null` when
   *  unknown (then the window has not started). */
  readonly movedAtMs: number | null;
  readonly nowMs: number;
}): boolean {
  if (input.isHead) return true;
  if (input.oneToOne) return false;
  if (input.movedAtMs === null) return true;
  return input.nowMs - input.movedAtMs < OLD_SEGMENT_POLL_WINDOW_MS;
}

/**
 * R.5 "push routing ... the old group's subscription is retired": whether
 * this device retires its push subscription for one group now.
 *
 * | the group is ...                                             | retires |
 * |--------------------------------------------------------------|---------|
 * | its own conversation, or the conversation's current group    | no      |
 * | an older segment, and the current group's membership is live | yes     |
 * | an older segment, the current group not live here            | no      |
 * | an older segment already retired                             | no      |
 *
 * "The current group" is the head as THIS device holds it
 * ({@link headSegment}): a device that has not joined the successor yet
 * still has the old group as its head and keeps its subscription, since
 * that is where it still sends and is woken (R.7 case 3).
 */
export function retiresOldSegmentPush(input: {
  readonly groupHex: string;
  /** The head of the group's conversation on this device, or `null`. */
  readonly headHex: string | null;
  readonly headActive: boolean;
  readonly alreadyRetired: boolean;
}): boolean {
  if (input.alreadyRetired) return false;
  if (input.headHex === null || input.headHex === input.groupHex) return false;
  return input.headActive;
}

/** The DS journal retention (30 days today): how long an old group's journal
 *  is still read after the move. Chosen for parity with the DS, not measured. */
export const OLD_SEGMENT_POLL_WINDOW_MS = 30 * 24 * 60 * 60 * 1000;

// ---------------------------------------------------------------------------
// The timeline of an open conversation
// ---------------------------------------------------------------------------

/**
 * A successor's DS message ids restart, so the rows of an OLDER segment
 * cannot share the head's id space on screen (the list keys, dedups and
 * jumps by message id). An older segment's row is shown under
 *
 *     id - distance * HISTORY_ID_SPAN        (distance >= 1 from the head)
 *
 * which is negative, unique per `(segment, message id)`, ascending inside a
 * segment and ascending across segments: the rule `(segment index, message
 * id)` as one number. Never stored and never sent: a history row is
 * read-only (no edit, delete, reaction, reply or pin can name it).
 */
export const HISTORY_ID_SPAN = 2 ** 40;

/** The on-screen id of an older segment's row. */
export function historyRowId(messageId: number, distanceFromHead: number): number {
  return messageId - Math.max(1, Math.trunc(distanceFromHead)) * HISTORY_ID_SPAN;
}

/**
 * The inverse of {@link historyRowId}: the message id and the segment
 * distance an on-screen history id stands for, or `null` for an id that is
 * not one (a head row, or a number no history row can have).
 */
export function historyRowOrigin(
  rowId: number,
): { readonly messageId: number; readonly distanceFromHead: number } | null {
  if (!Number.isSafeInteger(rowId) || rowId >= 0) return null;
  const distanceFromHead = Math.ceil(-rowId / HISTORY_ID_SPAN);
  const messageId = rowId + distanceFromHead * HISTORY_ID_SPAN;
  if (messageId <= 0 || messageId >= HISTORY_ID_SPAN) return null;
  return { messageId, distanceFromHead };
}

/** Whether an on-screen id belongs to an older segment (read-only). */
export function isHistoryRowId(id: number | undefined): boolean {
  return id !== undefined && id < 0;
}
