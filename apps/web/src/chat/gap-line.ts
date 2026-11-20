// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 / UX `conversation-rejoin.md` 2.4 - where the "can't be shown"
 * line sits in a chat, and the two times it names. Pure.
 *
 * A line sits where the gap was, by time: after the last entry of the
 * timeline at or before its start, that is before the first entry that
 * happened after its start, or at the bottom when nothing did (live walk 2,
 * 2026-11-19: placed by its end, the local join, a message sent after the
 * replacement but before the join stood above the line). The first time is the first message the device could not
 * read, with its date; the second is when it came back, with the date only
 * when it is another day.
 */
import type { GapLine } from "./reentry-store";

/** Where each line goes: before the entry at an index, or at the end. */
export interface GapPlacement {
  readonly before: ReadonlyMap<number, readonly GapLine[]>;
  readonly trailing: readonly GapLine[];
}

/**
 * Place `lines` among entries whose times (unix ms, `NaN` when unknown) are
 * `entryTimesMs`, oldest first. An entry with no time takes the previous
 * one's.
 */
export function placeGapLines(entryTimesMs: readonly number[], lines: readonly GapLine[]): GapPlacement {
  const before = new Map<number, GapLine[]>();
  const trailing: GapLine[] = [];
  const times: number[] = [];
  let last = Number.NEGATIVE_INFINITY;
  for (const t of entryTimesMs) {
    if (Number.isFinite(t)) last = t;
    times.push(last);
  }
  for (const line of [...lines].sort((a, b) => a.fromMs - b.fromMs || a.toMs - b.toMs)) {
    const index = times.findIndex((t) => t > line.fromMs);
    if (index < 0) trailing.push(line);
    else before.set(index, [...(before.get(index) ?? []), line]);
  }
  return { before, trailing };
}

function sameDay(a: Date, b: Date): boolean {
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
}

/** The `{from}` and `{to}` of the line, in `locale`. */
export function gapLineTimes(line: GapLine, locale: string | undefined): { readonly from: string; readonly to: string } {
  const from = new Date(line.fromMs);
  const to = new Date(line.toMs);
  const withDate = new Intl.DateTimeFormat(locale, { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
  const timeOnly = new Intl.DateTimeFormat(locale, { hour: "2-digit", minute: "2-digit" });
  return {
    from: withDate.format(from),
    to: sameDay(from, to) ? timeOnly.format(to) : withDate.format(to),
  };
}

/**
 * Insert `incoming` rows into `existing` by DS message id: a row whose id is
 * already there is skipped, and each new row goes before the first existing
 * row with a larger id (rows with no id, a send not acknowledged yet, stay
 * last). The gap's messages arrive after newer ones are already shown.
 */
export function insertByMessageId<T>(
  existing: readonly T[],
  incoming: readonly T[],
  idOf: (row: T) => number | undefined,
): T[] {
  const have = new Set<number>();
  for (const r of existing) {
    const id = idOf(r);
    if (id !== undefined) have.add(id);
  }
  const out = [...existing];
  const fresh = incoming
    .filter((r) => {
      const id = idOf(r);
      return id !== undefined && !have.has(id);
    })
    .sort((a, b) => (idOf(a) as number) - (idOf(b) as number));
  for (const row of fresh) {
    const id = idOf(row) as number;
    have.add(id);
    let at = out.findIndex((r) => {
      const other = idOf(r);
      return other === undefined || other > id;
    });
    if (at < 0) at = out.length;
    out.splice(at, 0, row);
  }
  return out;
}
