// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Times in the panel are UTC, like the days the service reports, so two
 * moderators in different time zones read the same date for one report.
 */

export function formatTime(ms: number): string {
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "unknown time";
  const iso = d.toISOString();
  return `${iso.slice(0, 10)} ${iso.slice(11, 16)} UTC`;
}

export function formatDay(ms: number): string {
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "unknown day";
  return d.toISOString().slice(0, 10);
}

/** A suspension's end: "until 2026-11-26 10:00 UTC" or "with no end date". */
export function untilPhrase(untilMs: number | null): string {
  return untilMs === null || untilMs === 0 ? "with no end date" : `until ${formatTime(untilMs)}`;
}

/** The appeal's warning when the moderator decided the appealed suspension (warn, never block). */
export function appealWarning(decidedByYou: boolean): string | null {
  return decidedByYou
    ? "You decided this suspension. Another moderator should decide the appeal, if one is available."
    : null;
}
