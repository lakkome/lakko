// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Recent calls, across every conversation, newest first.
 *
 * `docs/ux/navigation.md` section 5. The call log is kept per conversation
 * (`call-log-store.ts`, capped at 200 each), because that is how it is
 * written and how the conversation renders it. The Calls screen wants the
 * opposite cut: one sequence in time order regardless of who the call was
 * with, since a person looking for a call remembers roughly when, not which
 * thread it belonged to.
 *
 * Pure, so the flatten and the day grouping can be run by a test - vitest
 * has no jsdom and no Solid transform here, so nothing in the components is
 * reachable.
 */

import { currentLanguage, t } from "../l10n";
import type { CallLogEntry } from "./call-log";
import type { CallLogRows } from "./call-log-store";

/** One row of the Calls screen: a call, and who it was with. */
export interface CallHistoryRow {
  readonly groupIdB64: string;
  readonly entry: CallLogEntry;
}

/** How many rows the screen keeps. Older calls stay in their conversation. */
export const CALL_HISTORY_MAX = 300;

/**
 * Flatten the per-conversation logs into one list, newest first.
 *
 * Ties break on the call id so the order is total: two calls that started in
 * the same millisecond - a pair placed by a script, or two rows restored
 * from a blob that rounded - would otherwise sit in whatever order the map
 * happened to yield, and a list that reshuffles on a refresh reads as a bug.
 */
export function callHistory(
  rows: CallLogRows,
  limit: number = CALL_HISTORY_MAX,
): readonly CallHistoryRow[] {
  const out: CallHistoryRow[] = [];
  for (const [groupIdB64, entries] of rows) {
    for (const entry of entries) out.push({ groupIdB64, entry });
  }
  out.sort((a, b) => {
    const byTime = b.entry.startedAtMs - a.entry.startedAtMs;
    if (byTime !== 0) return byTime;
    return a.entry.id < b.entry.id ? -1 : a.entry.id > b.entry.id ? 1 : 0;
  });
  return out.slice(0, limit);
}

/** A run of calls that happened on the same calendar day. */
export interface CallHistoryDay {
  /** `YYYY-MM-DD` in the viewer's own zone; the key, not the label. */
  readonly day: string;
  readonly rows: readonly CallHistoryRow[];
}

/**
 * Group an already-ordered list into days.
 *
 * The day is computed in the VIEWER's zone rather than UTC: a call at
 * 00:30 local is today to the person who made it, and a list that files it
 * under yesterday because UTC says so is wrong in a way nobody would think
 * to report.
 */
export function callHistoryByDay(
  rows: readonly CallHistoryRow[],
): readonly CallHistoryDay[] {
  const days: CallHistoryDay[] = [];
  let current: { day: string; rows: CallHistoryRow[] } | null = null;
  for (const row of rows) {
    const day = localDay(row.entry.startedAtMs);
    if (current === null || current.day !== day) {
      current = { day, rows: [] };
      days.push(current);
    }
    current.rows.push(row);
  }
  return days;
}

function localDay(ms: number): string {
  const d = new Date(ms);
  const month = String(d.getMonth() + 1).padStart(2, "0");
  const dayOfMonth = String(d.getDate()).padStart(2, "0");
  return `${d.getFullYear()}-${month}-${dayOfMonth}`;
}

/**
 * The heading for a day of calls: `Today`, `Yesterday`, or the date.
 *
 * `now` is a parameter rather than read here so the boundary can be tested
 * without waiting for midnight.
 */
export function callHistoryDayLabel(day: string, now: Date = new Date()): string {
  if (day === localDay(now.getTime())) return t("chat.day.today");
  const yesterday = new Date(now.getTime());
  yesterday.setDate(yesterday.getDate() - 1);
  if (day === localDay(yesterday.getTime())) return t("chat.day.yesterday");
  const [year, month, dayOfMonth] = day.split("-");
  if (year === undefined || month === undefined || dayOfMonth === undefined) return day;
  const parsed = new Date(Number(year), Number(month) - 1, Number(dayOfMonth));
  if (Number.isNaN(parsed.getTime())) return day;
  const sameYear = parsed.getFullYear() === now.getFullYear();
  return parsed.toLocaleDateString(currentLanguage(), {
    day: "numeric",
    month: "long",
    ...(sameYear ? {} : { year: "numeric" }),
  });
}
