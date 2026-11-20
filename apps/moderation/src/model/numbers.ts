// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The columns of the numbers table: reports per reason (only reasons that
 * occur in the period, in sheet order), decisions per action (in the action
 * table's order, `No action` last), then appeals sent / declined / lifted.
 */

import type { NumbersWeek } from "../api/types";
import { ACTIONS, REASONS, actionLabel } from "./names";

export interface NumbersColumn {
  readonly key: string;
  readonly label: string;
  readonly value: (w: NumbersWeek) => number;
}

export function numbersColumns(weeks: readonly NumbersWeek[]): NumbersColumn[] {
  const reasonsSeen = new Set<string>();
  const decisionsSeen = new Set<string>();
  for (const w of weeks) {
    for (const [k, n] of Object.entries(w.reports)) if (n > 0) reasonsSeen.add(k);
    for (const [k, n] of Object.entries(w.decisions)) if (n > 0) decisionsSeen.add(k);
  }
  const known = new Set<string>(REASONS.map((r) => r.name));
  const reasonOrder = [
    ...REASONS.map((r) => r.name as string).filter((n) => reasonsSeen.has(n)),
    ...[...reasonsSeen].filter((n) => !known.has(n)).sort(),
  ];
  const knownActions = new Set<string>(ACTIONS.map((a) => a.name));
  const actionOrder = [
    ...ACTIONS.map((a) => a.name as string).filter((n) => decisionsSeen.has(n)),
    ...[...decisionsSeen].filter((n) => !knownActions.has(n) && n !== "no-action").sort(),
    ...(decisionsSeen.has("no-action") ? ["no-action"] : []),
  ];
  const reasonShortLabel = (n: string): string => REASONS.find((r) => r.name === n)?.short ?? n;
  return [
    { key: "reports", label: "Reports", value: (w) => sum(w.reports) },
    ...reasonOrder.map((n) => ({ key: `r:${n}`, label: reasonShortLabel(n), value: (w: NumbersWeek) => w.reports[n] ?? 0 })),
    { key: "decisions", label: "Decisions", value: (w) => sum(w.decisions) },
    ...actionOrder.map((n) => ({ key: `d:${n}`, label: actionLabel(n), value: (w: NumbersWeek) => w.decisions[n] ?? 0 })),
    { key: "appeals-sent", label: "Appeals", value: (w) => w.appeals.sent },
    { key: "appeals-declined", label: "Declined", value: (w) => w.appeals.declined },
    { key: "appeals-lifted", label: "Lifted", value: (w) => w.appeals.lifted },
  ];
}

function sum(r: Readonly<Record<string, number>>): number {
  let total = 0;
  for (const n of Object.values(r)) total += n;
  return total;
}
