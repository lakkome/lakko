// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A plain "how long ago" label for a status line ("Refreshed 3 hours ago").
 * Whole words, one unit, never a clock or a date: the reader wants a sense
 * of recency, not a timestamp. Pure: the clock is a parameter.
 */

const MINUTE_MS = 60_000;
const HOUR_MS = 3_600_000;
const DAY_MS = 86_400_000;

function plural(n: number, unit: string): string {
  return `${n} ${unit}${n === 1 ? "" : "s"} ago`;
}

/**
 * "just now" under a minute, then minutes, hours and days. A stamp in the
 * future (a clock skew) reads as "just now" rather than a negative number.
 */
export function relativeTimeLabel(sinceMs: number | bigint, nowMs: number = Date.now()): string {
  const since = typeof sinceMs === "bigint" ? Number(sinceMs) : sinceMs;
  if (!Number.isFinite(since)) return "just now";
  const delta = Math.max(0, nowMs - since);
  if (delta < MINUTE_MS) return "just now";
  if (delta < HOUR_MS) return plural(Math.floor(delta / MINUTE_MS), "minute");
  if (delta < DAY_MS) return plural(Math.floor(delta / HOUR_MS), "hour");
  return plural(Math.floor(delta / DAY_MS), "day");
}

/**
 * The time on a post card: the clock time for a post from today, otherwise
 * the short date with the time, the year only once it differs. `Intl` in
 * the viewer's locale, like the chat-list time.
 */
export function postTimeLabel(atMs: number, now: Date = new Date(), locale?: string): string {
  const d = new Date(atMs);
  if (Number.isNaN(d.getTime())) return "";
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate();
  const time: Intl.DateTimeFormatOptions = { hour: "numeric", minute: "2-digit" };
  if (sameDay) return new Intl.DateTimeFormat(locale, time).format(d);
  return new Intl.DateTimeFormat(locale, {
    day: "numeric",
    month: "short",
    ...(d.getFullYear() === now.getFullYear() ? {} : { year: "numeric" }),
    ...time,
  }).format(d);
}
