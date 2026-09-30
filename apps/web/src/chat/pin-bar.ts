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
