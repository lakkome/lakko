// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The timeline's date separators and message runs (docs/ux/client-redesign.md
 * section 5.2): a centred "Today" / "Yesterday" / "16 September" pill where
 * the day changes, and consecutive messages from one sender within five
 * minutes grouped into a run that names the sender and shows the avatar once.
 *
 * Pure: the rows come in as plain records (when, who, which side), the clock
 * and the locale are parameters, and the answer is one layout record per row
 * for the renderer to consume.
 */

/** How long after a message the same sender's next one still joins its run. */
export const RUN_WINDOW_MS = 5 * 60_000;

/** What the layout needs to know about one row. */
export interface TimelineItem {
  /** The DS row's `created_at`, or `undefined` for an un-acked outgoing
   *  bubble (it stays with the row before it). */
  readonly createdAt: string | undefined;
  /** The person the row is attributed to: a sender key for an incoming row,
   *  the reader's own key for an outgoing one. `undefined` never joins a
   *  run. */
  readonly senderKey: string | undefined;
  /** Which side of the timeline the row is on. A system row is neither and
   *  breaks every run. */
  readonly side: "incoming" | "outgoing" | "system";
}

/** One row's placement. */
export interface TimelineLayout {
  /** The separator pill to render above this row, or `null`. */
  readonly dayLabel: string | null;
  /** First row of a run: names the sender, shows the avatar. */
  readonly startsRun: boolean;
  /** Last row of a run: the tail radius and the normal gap after it. */
  readonly endsRun: boolean;
}

/** `17:21` in the viewer's locale; "" for an unparseable stamp. */
export function shortTime(iso: string | undefined, locale?: string): string {
  if (iso === undefined) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return new Intl.DateTimeFormat(locale, { hour: "2-digit", minute: "2-digit" }).format(d);
}

function startOfDay(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

/** "Today" / "Yesterday" / "16 September" (with the year once it differs). */
export function dayLabelOf(iso: string, now: Date = new Date(), locale?: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const days = Math.round((startOfDay(now) - startOfDay(d)) / 86_400_000);
  if (days === 0) return "Today";
  if (days === 1) return "Yesterday";
  return new Intl.DateTimeFormat(locale, {
    day: "numeric",
    month: "long",
    ...(d.getFullYear() === now.getFullYear() ? {} : { year: "numeric" }),
  }).format(d);
}

/** Whether two stamps fall on the same local calendar day. */
export function sameDay(a: string | undefined, b: string | undefined): boolean {
  if (a === undefined || b === undefined) return false;
  const x = new Date(a);
  const y = new Date(b);
  if (Number.isNaN(x.getTime()) || Number.isNaN(y.getTime())) return false;
  return (
    x.getFullYear() === y.getFullYear() && x.getMonth() === y.getMonth() && x.getDate() === y.getDate()
  );
}

function millis(iso: string | undefined): number | null {
  if (iso === undefined) return null;
  const t = new Date(iso).getTime();
  return Number.isNaN(t) ? null : t;
}

/** Whether `next` continues the run `prev` is in: same side, same sender,
 *  neither a system row, no day change, within the window. An un-stamped
 *  row (a message still sending) continues its predecessor's run. */
export function continuesRun(prev: TimelineItem, next: TimelineItem, dayChanged: boolean): boolean {
  if (prev.side === "system" || next.side === "system") return false;
  if (prev.side !== next.side) return false;
  if (prev.senderKey === undefined || prev.senderKey !== next.senderKey) return false;
  if (dayChanged) return false;
  const a = millis(prev.createdAt);
  const b = millis(next.createdAt);
  if (b === null) return true;
  if (a === null) return false;
  return b - a >= 0 && b - a <= RUN_WINDOW_MS;
}

/** The layout of every row, in order. */
export function layoutTimeline(
  items: readonly TimelineItem[],
  now: Date = new Date(),
  locale?: string,
): TimelineLayout[] {
  // The day each row is shown under: its own stamp, or the last known one
  // for a row that has none yet.
  const shownDay: (string | undefined)[] = [];
  let lastStamp: string | undefined;
  for (const it of items) {
    if (it.createdAt !== undefined) lastStamp = it.createdAt;
    shownDay.push(it.createdAt ?? lastStamp);
  }
  const dayChanged: boolean[] = items.map((_, i) => {
    const cur = shownDay[i];
    if (cur === undefined) return false;
    const prev = i === 0 ? undefined : shownDay[i - 1];
    return !sameDay(prev, cur);
  });
  return items.map((it, i) => {
    const prev = i === 0 ? undefined : items[i - 1];
    const next = items[i + 1];
    const startsRun = prev === undefined || !continuesRun(prev, it, dayChanged[i] === true);
    const endsRun = next === undefined || !continuesRun(it, next, dayChanged[i + 1] === true);
    const day = shownDay[i];
    const dayLabel = dayChanged[i] === true && day !== undefined ? dayLabelOf(day, now, locale) : null;
    return { dayLabel, startsRun, endsRun };
  });
}
