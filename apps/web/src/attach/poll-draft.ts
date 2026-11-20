// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` sections 7.1 and 8.1 — the editor-side bounds of a
 * poll and a checklist. All chosen, not measured; the core re-checks each one
 * (in bytes) when it signs, so these only decide when Create is enabled.
 */
import type { ChecklistDraft, PollDraft } from "./types";

export const POLL_QUESTION_MAX = 255;
export const POLL_OPTION_MAX = 100;
export const POLL_OPTIONS_MIN = 2;
export const POLL_OPTIONS_MAX = 10;
export const POLL_EXPLANATION_MAX = 200;
export const CHECKLIST_TITLE_MAX = 255;
export const CHECKLIST_ITEM_MAX = 100;
export const CHECKLIST_ITEMS_MAX = 30;

/** Close-automatically choices (UX 7.1): 1 hour, 1 day, 1 week. */
export const CLOSE_AFTER_CHOICES: readonly { readonly key: string; readonly ms: number }[] = [
  { key: "poll.editor.close_1h", ms: 60 * 60 * 1000 },
  { key: "poll.editor.close_1d", ms: 24 * 60 * 60 * 1000 },
  { key: "poll.editor.close_1w", ms: 7 * 24 * 60 * 60 * 1000 },
];

/** Length in characters (code points), as a person counts them. */
export function charCount(s: string): number {
  return [...s].length;
}

/** The filled options, trimmed, in order. */
export function normaliseOptions(options: readonly string[]): string[] {
  return options.map((o) => o.trim()).filter((o) => o.length > 0);
}

/** Rows a checklist editor never goes below (UX 8.1). */
export const CHECKLIST_ITEMS_MIN = 1;

/** Whether `Add option` / `Add item` is shown (UX 7.1a: absent at the maximum). */
export function canAddRow(length: number, max: number): boolean {
  return length < max;
}

/** Whether a row's remove icon is shown (UX 7.1a: only above the minimum). */
export function canRemoveRow(length: number, min: number): boolean {
  return length > min;
}

/** The list with one more entry at the end, unless it is at the maximum. */
export function withAddedRow<T>(list: readonly T[], empty: T, max: number): T[] {
  return canAddRow(list.length, max) ? [...list, empty] : [...list];
}

/** The list without entry `index`, unless that would go below the minimum. */
export function withoutRow<T>(list: readonly T[], index: number, min: number): T[] {
  if (index < 0 || index >= list.length || !canRemoveRow(list.length, min)) return [...list];
  return list.filter((_, i) => i !== index);
}

/** What `Enter` in row `index` does (UX 7.1a): move to the next row, or on the
 *  last row add one and move to it; on the last row at the maximum, nothing. */
export type EnterAction = { readonly kind: "focus"; readonly to: number } | { readonly kind: "add"; readonly to: number };

export function enterAction(index: number, length: number, max: number): EnterAction | null {
  if (index < 0 || index >= length) return null;
  if (index < length - 1) return { kind: "focus", to: index + 1 };
  return canAddRow(length, max) ? { kind: "add", to: length } : null;
}

/** Where `Alt+Up` / `Alt+Down` in row `index` moves it (UX 7.1a), or null when
 *  the key is not one of them or the row is already at that end. */
export function keyboardMoveTarget(index: number, key: string, altKey: boolean, length: number): number | null {
  if (!altKey || index < 0 || index >= length) return null;
  if (key === "ArrowUp") return index > 0 ? index - 1 : null;
  if (key === "ArrowDown") return index < length - 1 ? index + 1 : null;
  return null;
}

export function isPollDraftValid(d: PollDraft): boolean {
  const q = d.question.trim();
  if (charCount(q) < 1 || charCount(q) > POLL_QUESTION_MAX) return false;
  const opts = normaliseOptions(d.options);
  if (opts.length !== d.options.length) return false;
  if (opts.length < POLL_OPTIONS_MIN || opts.length > POLL_OPTIONS_MAX) return false;
  if (opts.some((o) => charCount(o) > POLL_OPTION_MAX)) return false;
  if (d.quiz !== undefined) {
    if (d.multiple) return false;
    if (!Number.isInteger(d.quiz.correct) || d.quiz.correct < 0 || d.quiz.correct >= opts.length) return false;
    if (charCount(d.quiz.explanation) > POLL_EXPLANATION_MAX) return false;
  }
  if (d.closeAfterMs !== undefined && !(d.closeAfterMs > 0)) return false;
  return true;
}

export function isChecklistDraftValid(d: ChecklistDraft): boolean {
  const title = d.title.trim();
  if (charCount(title) < 1 || charCount(title) > CHECKLIST_TITLE_MAX) return false;
  if (d.items.length < 1 || d.items.length > CHECKLIST_ITEMS_MAX) return false;
  return d.items.every((i) => {
    const n = charCount(i.text.trim());
    return n >= 1 && n <= CHECKLIST_ITEM_MAX;
  });
}

/** Move one entry to another position (a drag), shifting the rest; out of
 *  range returns a copy unchanged. */
export function movedTo<T>(list: readonly T[], from: number, to: number): T[] {
  const out = [...list];
  if (from < 0 || to < 0 || from >= out.length || to >= out.length) return out;
  const [item] = out.splice(from, 1);
  out.splice(to, 0, item as T);
  return out;
}
