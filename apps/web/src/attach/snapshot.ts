// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 section 3.11 — a poll or checklist restored from a history bundle
 * renders from the state the bundle carried; live ops continue on top of it.
 * Pure helpers, tested directly.
 */
import type {
  ChecklistAddRecord,
  ChecklistBody,
  ChecklistItemRef,
  ChecklistStateItem,
  ChecklistTickRecord,
  ContentState,
} from "../../wasm/lakko_ffi_wasm";
import type { PollEntry } from "./content-store";
import type { TallyLike } from "./poll-model";

/**
 * The tally a poll entry shows: the full fold, or — for an entry filed from a
 * history bundle and not yet replaced by a hand-over — the bundle's totals
 * alone. The bundle's totals already include some votes and no voter is
 * known, so a live vote is never added to them (it would count twice); it is
 * kept and counted once the hand-over gives the per-voter state.
 */
export function shownTally(live: TallyLike | null, entry: Pick<PollEntry, "baseline" | "fromSnapshot">): TallyLike | null {
  if (entry.fromSnapshot === true && entry.baseline !== undefined) {
    return { counts: entry.baseline.counts, voters: entry.baseline.voters, closed: entry.baseline.closed || (live?.closed ?? false) };
  }
  return live;
}

/** The bundle's checklist state as stored records (privileged: the sending
 *  device's fold already applied the flags). */
export function checklistRecordsFromSnapshot(
  author: string,
  checklistMessageId: number,
  snap: Extract<ContentState, { kind: "checklist" }>,
): { adds: ChecklistAddRecord[]; ticks: ChecklistTickRecord[]; closes: { messageId: number; closer: string }[] } {
  const adds = snap.added.map((a) => ({ messageId: a.messageId, adder: author, privileged: true, text: a.text }));
  // A tick must come after the item it names (an added item's row), so the
  // stand-in ids start after every id the snapshot carries.
  const after = Math.max(checklistMessageId, ...snap.added.map((a) => a.messageId));
  const ticks = snap.done.map((d, k) => ({
    messageId: after + 1 + k,
    ticker: d.by,
    privileged: true,
    item: d.item,
    done: true,
  }));
  // The close stays after the stand-in ticks, so the fold keeps them.
  const closes =
    snap.closedBy == null ? [] : [{ messageId: Math.max(snap.closedBy, after + snap.done.length + 1), closer: author }];
  return { adds, ticks, closes };
}

/** The checklist state to draw from a snapshot alone (author unknown here). */
export function checklistStateFromSnapshot(
  body: ChecklistBody,
  snap: Extract<ContentState, { kind: "checklist" }>,
): { title: string; othersAdd: boolean; othersMark: boolean; items: ChecklistStateItem[]; closedBy?: number | null } {
  const doneBy = (ref: ChecklistItemRef): string | null =>
    snap.done.find((d) => JSON.stringify(d.item) === JSON.stringify(ref))?.by ?? null;
  const items: ChecklistStateItem[] = [
    ...body.items.map((i) => ({ item: { kind: "author" as const, id: i.id }, text: i.text, doneBy: doneBy({ kind: "author", id: i.id }) })),
    ...snap.added.map((a) => ({ item: { kind: "added" as const, messageId: a.messageId }, text: a.text, doneBy: doneBy({ kind: "added", messageId: a.messageId }) })),
  ];
  return { title: body.title, othersAdd: body.othersAdd, othersMark: body.othersMark, items, closedBy: snap.closedBy ?? null };
}

