// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 7.2 — the pure parts of the poll card:
 * the share of each option, the line under the question, the footer.
 */
import type { PollView } from "./types";

/**
 * Whole percentages of each option. A single-answer poll uses the largest
 * remainder so the shares add up to 100; with several answers each share is
 * of the voters, rounded on its own (they may add up to more than 100).
 * Nobody voted: every share is 0.
 */
export function pollPercents(counts: readonly number[], voters: number, multiple: boolean): number[] {
  if (voters <= 0) return counts.map(() => 0);
  if (multiple) return counts.map((c) => Math.min(100, Math.round((Math.max(0, c) * 100) / voters)));
  const total = counts.reduce((a, c) => a + Math.max(0, c), 0);
  if (total <= 0) return counts.map(() => 0);
  const raw = counts.map((c) => (Math.max(0, c) * 100) / total);
  const floors = raw.map(Math.floor);
  let left = 100 - floors.reduce((a, b) => a + b, 0);
  const order = raw
    .map((r, i) => ({ i, frac: r - Math.floor(r) }))
    .filter((x) => x.frac > 0)
    .sort((a, b) => b.frac - a.frac || a.i - b.i);
  for (const { i } of order) {
    if (left <= 0) break;
    floors[i] = (floors[i] ?? 0) + 1;
    left -= 1;
  }
  return floors;
}

/** The catalog key of the line under the question. */
export function pollKindKey(anonymous: boolean, quiz: boolean): string {
  if (quiz) return anonymous ? "poll.kind.anonymous_quiz" : "poll.kind.quiz";
  return anonymous ? "poll.kind.anonymous" : "poll.kind.public";
}

/** The status part of the footer after the vote count. */
export type PollFooterStatus =
  | { readonly kind: "none" }
  | { readonly kind: "closed" }
  | { readonly kind: "closes-in"; readonly remainingMs: number }
  | { readonly kind: "sending" }
  | { readonly kind: "failed" };

/** What the footer says, in priority order: a vote being sent or not sent,
 *  then closed, then the automatic close. */
export function pollFooterStatus(view: PollView, nowMs: number): PollFooterStatus {
  if (view.sending) return { kind: "sending" };
  if (view.failed) return { kind: "failed" };
  if (view.closed) return { kind: "closed" };
  if (view.closesAtMs !== undefined) {
    const remainingMs = view.closesAtMs - nowMs;
    if (remainingMs <= 0) return { kind: "closed" };
    return { kind: "closes-in", remainingMs };
  }
  return { kind: "none" };
}

/** Whether the card shows results (shares) rather than choices. */
export function pollShowsResults(view: PollView): boolean {
  return !view.unverifiable && (view.voted || view.closed);
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** The duration in its largest whole unit, rounded up: days from one day,
 *  hours from one hour, else minutes (at least one). */
export function durationUnit(ms: number): { value: number; unit: "day" | "hour" | "minute" } {
  if (ms >= DAY) return { value: Math.ceil(ms / DAY), unit: "day" };
  if (ms >= HOUR) return { value: Math.ceil(ms / HOUR), unit: "hour" };
  return { value: Math.max(1, Math.ceil(ms / MINUTE)), unit: "minute" };
}

/** A duration for `poll.closes_in`, formatted for the locale. */
export function formatDuration(ms: number, locale: string): string {
  const { value, unit } = durationUnit(ms);
  try {
    return new Intl.NumberFormat(locale, { style: "unit", unit, unitDisplay: "long" }).format(value);
  } catch {
    return String(value);
  }
}

/** Format seconds as `m:ss` (or `h:mm:ss`) for an audio position. */
export function formatClock(totalSeconds: number): string {
  const s = Math.max(0, Math.floor(Number.isFinite(totalSeconds) ? totalSeconds : 0));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}
