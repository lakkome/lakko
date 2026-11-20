// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The queue's filters and the words of a queue row (UX section 5.2). The
 * order of the rows is the service's (checked messages first, then report
 * count); `defaultOrder` restates it so the panel never shows a row list in
 * another order, for instance after a row was updated locally.
 */

import type { CaseRow } from "../api/types";
import { REASONS, reasonShort } from "./names";

/** `""` means "any" for every field. */
export interface QueueFilters {
  readonly status: string;
  readonly reason: string;
  readonly kind: string;
  /** `"yes"`: has a report with a checked message; `"no"`: has none. */
  readonly checked: "" | "yes" | "no";
}

export const NO_FILTERS: QueueFilters = { status: "", reason: "", kind: "", checked: "" };

/** The query string for `GET /cases`, empty values left out. */
export function queueQuery(f: QueueFilters): string {
  const q = new URLSearchParams();
  if (f.status !== "") q.set("status", f.status);
  if (f.reason !== "") q.set("reason", f.reason);
  if (f.kind !== "") q.set("kind", f.kind);
  if (f.checked === "yes") q.set("checked", "true");
  if (f.checked === "no") q.set("checked", "false");
  const s = q.toString();
  return s === "" ? "" : `?${s}`;
}

/** `true` when any filter is set. */
export function hasFilters(f: QueueFilters): boolean {
  return f.status !== "" || f.reason !== "" || f.kind !== "" || f.checked !== "";
}

/** "Spam 14 · Harassment 2": the most reported first, then sheet order. */
export function reasonsSummary(reasons: Readonly<Record<string, number>>): string {
  const order = (name: string): number => {
    const i = REASONS.findIndex((r) => r.name === name);
    return i < 0 ? REASONS.length : i;
  };
  return Object.entries(reasons)
    .filter(([, n]) => n > 0)
    .sort(([a, na], [b, nb]) => nb - na || order(a) - order(b))
    .map(([name, n]) => `${reasonShort(name)} ${n}`)
    .join(" · ");
}

/** The evidence line of a row: checked messages, and reports with none. */
export function evidenceSummary(row: Pick<CaseRow, "reports" | "with_messages" | "checked_messages_reports">): string {
  const checked = row.checked_messages_reports;
  const none = Math.max(0, row.reports - row.with_messages);
  const first =
    checked === 0
      ? "No checked messages"
      : `${checked} ${checked === 1 ? "report has" : "reports have"} checked messages`;
  const second = none === 0 ? "" : ` · ${none} with nothing included`;
  return `${first}${second}`;
}

/** "Nov 1 – Nov 19" or one day, from `YYYY-MM-DD`. */
export function daySpan(first: string, last: string): string {
  return first === last ? first : `${first} – ${last}`;
}

/** The service's order: checked messages first, then reports, then the latest day. */
export function defaultOrder(rows: readonly CaseRow[]): CaseRow[] {
  return [...rows].sort((a, b) => {
    const ca = a.checked_messages_reports > 0 ? 1 : 0;
    const cb = b.checked_messages_reports > 0 ? 1 : 0;
    if (ca !== cb) return cb - ca;
    if (a.reports !== b.reports) return b.reports - a.reports;
    return b.last_day.localeCompare(a.last_day);
  });
}
