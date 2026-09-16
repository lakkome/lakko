// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The time on a chat-list row (docs/ux/client-redesign.md section 5.1):
 * today shows the clock time, the last week the short weekday, anything
 * older the short date. Every form comes from `Intl` in the viewer's locale,
 * so a 12-hour locale sees "2:05 PM" where a 24-hour one sees "14:05".
 *
 * Pure: the clock and the locale are parameters so the rule is testable.
 */

const DAY_MS = 86_400_000;

/** Local midnight of `d`, as a timestamp. */
function startOfDay(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

/** Whole local calendar days from `d` to `now` (negative when `d` is later). */
export function calendarDaysBetween(d: Date, now: Date): number {
  return Math.round((startOfDay(now) - startOfDay(d)) / DAY_MS);
}

/**
 * The row's time label for a message stamped `iso`, or "" for an unparseable
 * stamp. `locale` undefined means the browser's own.
 */
export function formatListTime(
  iso: string | null | undefined,
  now: Date = new Date(),
  locale?: string,
): string {
  if (iso === null || iso === undefined) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const days = calendarDaysBetween(d, now);
  if (days <= 0) {
    return new Intl.DateTimeFormat(locale, { hour: "numeric", minute: "2-digit" }).format(d);
  }
  if (days < 7) {
    return new Intl.DateTimeFormat(locale, { weekday: "short" }).format(d);
  }
  return new Intl.DateTimeFormat(locale, {
    day: "numeric",
    month: "short",
    ...(d.getFullYear() === now.getFullYear() ? {} : { year: "numeric" }),
  }).format(d);
}
