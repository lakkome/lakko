// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/message-actions.md` section 4 / 4.1 — what the pinned bar under
 * the header shows, in what order, and where a tap takes it next.
 *
 * The bar is DERIVED from three things and never stored: the shared pin map,
 * the personal pin set, and the messages this device holds. This module is
 * that derivation, pure, so a test can run it:
 *
 *   * {@link pinBarItems} — the ordered list. Shared pins come before
 *     personal ones ("a shared pin outranks a personal one"), each group
 *     newest first (the shared stamp, the personal pin time), a message
 *     pinned in both scopes appears ONCE, as the shared one. A pinned
 *     message this device does not hold is skipped — "not shown in the bar
 *     until it is; the pin itself is kept" — and a deleted one leaves the
 *     bar rather than showing as a tombstone.
 *   * {@link nextPinIndex} — the tap: advance to the next OLDER pin, wrap
 *     past the oldest to the newest.
 *   * {@link clampPinIndex} — where the bar stands after the list changed
 *     under it (a pin arrived, a message was deleted).
 *
 * Nothing here names an author or builds an excerpt; those come from the
 * timeline's own resolvers, and the bar draws what it is handed.
 */

import { t } from "../l10n";
import type { StoredPin } from "./pin-store";

/** One entry of the bar, before any text is resolved. */
export interface PinBarItem {
  readonly messageId: number;
  /** Which scope puts it in the bar. `shared` wins when both do. */
  readonly scope: "shared" | "personal";
}

/** What the bar needs to know about a held message. */
export interface HeldForPin {
  /** A deleted message leaves the bar (section 4). */
  readonly deleted: boolean;
}

/**
 * Section 4 — the order: shared first, personal after; within each, newest
 * first; a stamp tie broken by the higher message id (the later message).
 *
 * @param held `null` when this device does not hold the message.
 */
export function pinBarItems(args: {
  readonly shared: ReadonlyMap<number, StoredPin>;
  readonly personal: ReadonlyMap<number, bigint>;
  readonly held: (messageId: number) => HeldForPin | null;
}): PinBarItem[] {
  const visible = (messageId: number): boolean => {
    const h = args.held(messageId);
    return h !== null && !h.deleted;
  };
  const shared: { id: number; at: bigint }[] = [];
  for (const [id, v] of args.shared) {
    if (v.pinned && visible(id)) shared.push({ id, at: v.updatedAtMs });
  }
  const sharedIds = new Set(shared.map((s) => s.id));
  const personal: { id: number; at: bigint }[] = [];
  for (const [id, at] of args.personal) {
    if (!sharedIds.has(id) && visible(id)) personal.push({ id, at });
  }
  const newestFirst = (a: { id: number; at: bigint }, b: { id: number; at: bigint }): number =>
    a.at === b.at ? b.id - a.id : a.at > b.at ? -1 : 1;
  shared.sort(newestFirst);
  personal.sort(newestFirst);
  return [
    ...shared.map((s): PinBarItem => ({ messageId: s.id, scope: "shared" })),
    ...personal.map((p): PinBarItem => ({ messageId: p.id, scope: "personal" })),
  ];
}

/** The pins of ONE segment (group) of a conversation. */
export interface SegmentPins {
  /** How many segments this one is before the conversation's current group
   *  (`0` = the current group). */
  readonly distanceFromHead: number;
  readonly shared: ReadonlyMap<number, StoredPin>;
  readonly personal: ReadonlyMap<number, bigint>;
}

/**
 * ADR-0065 R.5 — a conversation's pins span its segments. The pin store
 * stays keyed on `(group, message id)` and is never rewritten when a
 * conversation continues in a successor group, so a pin of an older segment
 * is still held under the older group. This folds every held segment's pins
 * into ONE pair of maps keyed on the ON-SCREEN row id: the message id itself
 * for the current group, `rowId(messageId, distance)` for an older one
 * (`lineage.ts` `historyRowId`), which is the id the timeline draws that row
 * under and the id a tap jumps to. So a pin of an older segment stays in the
 * bar and opens where it was.
 *
 * Pure; `pinBarItems` then orders the result exactly as before.
 */
export function pinsAcrossSegments(
  segments: readonly SegmentPins[],
  rowId: (messageId: number, distanceFromHead: number) => number,
): { readonly shared: Map<number, StoredPin>; readonly personal: Map<number, bigint> } {
  const shared = new Map<number, StoredPin>();
  const personal = new Map<number, bigint>();
  for (const segment of segments) {
    const d = Math.max(0, Math.trunc(segment.distanceFromHead));
    const key = (messageId: number): number => (d === 0 ? messageId : rowId(messageId, d));
    for (const [id, pin] of segment.shared) shared.set(key(id), pin);
    for (const [id, at] of segment.personal) personal.set(key(id), at);
  }
  return { shared, personal };
}

/** The stored pin an on-screen row id stands for. */
export interface PinTarget {
  /** The group the pin is stored under (standard base64). */
  readonly groupIdB64: string;
  /** The message id in THAT group. */
  readonly messageId: number;
  /** Whether that group is an older segment of the conversation. */
  readonly older: boolean;
}

/**
 * ADR-0065 R.5 — the inverse of {@link pinsAcrossSegments}: which stored
 * `(group, message id)` an on-screen row id is. A pin of an older segment
 * is drawn under its history id; to UNPIN it the store has to be told the
 * older group and the original id.
 *
 * `olderGroupIdsB64` is the conversation's older segments, oldest first
 * (distance `length - index` from the head), exactly as the bar folds them.
 * `null` for a history id whose segment this device does not hold.
 */
export function pinTargetOfRow(input: {
  readonly rowId: number;
  readonly headGroupIdB64: string;
  readonly olderGroupIdsB64: readonly string[];
  readonly origin: (
    rowId: number,
  ) => { readonly messageId: number; readonly distanceFromHead: number } | null;
}): PinTarget | null {
  if (input.rowId >= 0) {
    return { groupIdB64: input.headGroupIdB64, messageId: input.rowId, older: false };
  }
  const origin = input.origin(input.rowId);
  if (origin === null) return null;
  const groupIdB64 = input.olderGroupIdsB64[input.olderGroupIdsB64.length - origin.distanceFromHead];
  if (groupIdB64 === undefined) return null;
  return { groupIdB64, messageId: origin.messageId, older: true };
}

/** How an unpin (or a pin) of one scope is carried out. */
export type PinRoute =
  /** The ordinary path: a shared change is sent to the group. */
  | "ordinary"
  /** Applied on this device only: nothing is published. */
  | "local-only"
  /** Not offered. */
  | "refused";

/**
 * ADR-0065 R.5 — a pin of an OLDER segment can be removed, and nothing
 * else can be done to it: an older segment is read-only on the wire (no op
 * can name its rows in the current group's journal, and nothing is sent
 * into a superseded session), so its shared unpin is recorded on this
 * device alone, with a fresh stamp so a late re-send of the old pin cannot
 * bring it back.
 *
 * | segment | change | scope    | route        |
 * |---------|--------|----------|--------------|
 * | current | any    | any      | `ordinary`   |
 * | older   | unpin  | personal | `ordinary`   |
 * | older   | unpin  | shared   | `local-only` |
 * | older   | pin    | any      | `refused`    |
 */
export function pinRoute(input: {
  readonly older: boolean;
  readonly pinned: boolean;
  readonly scope: "shared" | "personal";
}): PinRoute {
  if (!input.older) return "ordinary";
  if (input.pinned) return "refused";
  return input.scope === "shared" ? "local-only" : "ordinary";
}

/**
 * ADR-0065 R.5 — what the action menu of a row of an OLDER segment offers:
 * the `Unpin` row alone when the row is pinned in a way this device may
 * clear, and nothing otherwise (a history row takes no edit, delete,
 * reaction, reply, forward or new pin).
 */
export function historyRowOffersUnpin(input: {
  readonly isHistoryRow: boolean;
  readonly pinRow: "Pin" | "Unpin";
}): boolean {
  return input.isHistoryRow && input.pinRow === "Unpin";
}

/** The tap: the next older pin, wrapping past the oldest to the newest.
 *  With one pin the bar stays where it is. */
export function nextPinIndex(current: number, count: number): number {
  if (count <= 0) return 0;
  return (current + 1) % count;
}

/** After the list changed: keep the position when it still exists, else
 *  return to the newest. */
export function clampPinIndex(current: number, count: number): number {
  if (count <= 0) return 0;
  return current >= 0 && current < count ? current : 0;
}

/** Section 4.1 — the label in place of the author on a personal pin. */
export const PIN_FOR_YOU = (): string => t("chat.pinned_bar.for_you");

/** The accessible name of the bar for the pin at `index` (0-based). */
export function pinBarLabel(index: number, count: number): string {
  return count > 1
    ? t("chat.pinned_bar.label_of", { index: index + 1, count })
    : t("chat.pinned_bar.label");
}
