// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/message-actions.md` section 8 — selection mode: the rules behind
 * `Select`, the count, the cap, the two bulk buttons and their copy, decided
 * once here so the row's mark, the bar, the confirmation and the progress
 * notice cannot disagree.
 *
 * A selection is a UI mode and nothing more: it adds no wire surface, is
 * never persisted, and what it does at the end is the one-message Delete or
 * Forward repeated over a set, oldest first. The view (`routes/Chat.tsx`) is
 * keyed on the conversation id, so navigating to another conversation
 * remounts it and the selection goes with it — "leaving on navigation" is
 * structural there; {@link leaveSelection} is what every other exit returns
 * to (Cancel, Escape, a completed action).
 *
 * `vitest` runs in a plain `node` environment with no Solid transform, so
 * everything that DECIDES lives here and the components hold markup.
 */

import type { MediaBubble } from "../media/inline-media";
import type { ConfirmRequest } from "../ui/confirm-dialog";
import { replySnapshotFor } from "./reply-snapshot";
import { t } from "../l10n";

/** Section 8 "Selecting" — "At most 100 messages can be selected". */
export const SELECTION_LIMIT = 100;

/** The selected messages, by DS `messageId`. Immutable: every change returns
 *  a new set, so a signal holding one re-renders on replacement. */
export type Selection = ReadonlySet<number>;

/** Section 8 "Delete" — the helper line under the bar. */
export const DELETE_HELPER = (): string => t("chat.select.delete_helper");

/** Section 8 "Delete" — the destructive action and the exit. */
export const BULK_DELETE_CONFIRM_LABEL = (): string => t("common.delete");
export const BULK_DELETE_CANCEL_LABEL = (): string => t("common.cancel");

/** Nothing selected — the state every exit returns to. */
export function leaveSelection(): Selection {
  return new Set<number>();
}

/** Section 8 "Entering" — `Select` in the card enters the mode "with the
 *  message it was opened on already selected". */
export function enterSelection(messageId: number): Selection {
  return new Set<number>([messageId]);
}

/**
 * Section 8 "Selecting" — which rows can be acted on at all. The gates are
 * the ones the single actions already compute, read once for the whole
 * selection: a DS id to reference (a date separator, a system row and a
 * pending outgoing bubble have none), not deleted, and a completed send (the
 * Forward gate of section 5.1: a failed send or an upload still in flight is
 * not forwardable). Rows that fail this show no mark and take no tap.
 */
export function canSelectRow(row: {
  readonly hasId: boolean;
  readonly deleted: boolean;
  readonly sendComplete: boolean;
}): boolean {
  return row.hasId && !row.deleted && row.sendComplete;
}

/** Whether the selection is full: "at the limit further taps do nothing". */
export function atSelectionLimit(selection: Selection): boolean {
  return selection.size >= SELECTION_LIMIT;
}

/**
 * Toggle one message. Removing always works; adding past the cap is refused
 * and the SAME set is returned, so a caller comparing identity can tell a
 * refused tap from an applied one without counting.
 */
export function toggleSelected(selection: Selection, messageId: number): Selection {
  const next = new Set(selection);
  if (next.has(messageId)) {
    next.delete(messageId);
    return next;
  }
  if (atSelectionLimit(selection)) return selection;
  next.add(messageId);
  return next;
}

/** Section 8 "Entering" / "Selecting" — the count in the middle of the bar:
 *  "1 selected", "12 selected", and at the cap "100 selected, the limit". */
export function selectionCountLabel(count: number): string {
  const n = Math.max(0, Math.floor(count));
  return n >= SELECTION_LIMIT
    ? t("chat.select.count_limit", { count: SELECTION_LIMIT })
    : t("chat.select.count", { count: n });
}

/** What the `Delete` button and the line under it read. */
export interface DeleteDecision {
  readonly enabled: boolean;
  /** {@link DELETE_HELPER} when the selection holds someone else's message,
   *  else `null` (nothing to explain). */
  readonly helper: string | null;
}

/**
 * Section 8 "Delete" — enabled when the selection is not empty and every
 * selected message is the person's own (a delete is sent to everyone and
 * peers accept it only from the author, ADR-0023); when it holds someone
 * else's message the button is disabled and the helper explains why. An
 * empty selection is disabled with no helper: there is nothing to explain.
 */
export function deleteDecision(selected: ReadonlyArray<{ readonly own: boolean }>): DeleteDecision {
  if (selected.length === 0) return { enabled: false, helper: null };
  const allOwn = selected.every((m) => m.own);
  return { enabled: allOwn, helper: allOwn ? null : DELETE_HELPER() };
}

/** Section 8 "Forward" — enabled when the selection is not empty and holds
 *  no message carrying a channel invitation card, which is never forwarded
 *  (maintainer, 2026-11-13; section 5.1). `holdsInvitation` is the caller's
 *  read over the selected messages. */
export function forwardEnabled(selection: Selection, holdsInvitation = false): boolean {
  return selection.size > 0 && !holdsInvitation;
}

/** Section 8 "Delete" — the question: "Delete 12 messages?", "Delete 1 message?". */
export function confirmDeleteLabel(count: number): string {
  const n = Math.max(0, Math.floor(count));
  return t("chat.select.delete_title", { count: n });
}

/** The whole confirmation for a bulk delete: the question, a destructive
 *  `Delete`, and `Cancel`. No body — a single delete asks nothing, and the
 *  one fact that makes the bulk one different (it is multiplied) is already
 *  the number in the title. */
export function bulkDeleteConfirmation(count: number): ConfirmRequest {
  return {
    title: confirmDeleteLabel(count),
    body: null,
    confirmLabel: BULK_DELETE_CONFIRM_LABEL(),
    cancelLabel: BULK_DELETE_CANCEL_LABEL(),
    tone: "danger",
  };
}

/** Section 8 — what the view says when a bulk action stops at a failure:
 *  "Deleted 5 of 12." / "Forwarded 5 of 12.". The unsent messages stay
 *  selected, so the person can try again from where it stopped. */
export function progressLabel(verb: "deleted" | "forwarded", done: number, total: number): string {
  const args = { done: Math.max(0, done), total: Math.max(0, total) };
  return verb === "deleted" ? t("chat.select.deleted_of", args) : t("chat.select.forwarded_of", args);
}

/**
 * The order a bulk action runs in: oldest first. A DS `messageId` is
 * per-conversation and assigned in journal order, so ascending id IS the
 * conversation's order — the same order the timeline draws.
 */
export function oldestFirst(selection: Selection): number[] {
  return [...selection].sort((a, b) => a - b);
}

/** The selection after `done` messages were acted on: those leave it, the
 *  rest stay selected (section 8, the failure case). */
export function withoutDone(selection: Selection, done: readonly number[]): Selection {
  const next = new Set(selection);
  for (const id of done) next.delete(id);
  return next;
}

/**
 * Section 8 "Selecting" — the mark is "named by the message's text or kind".
 * The same naming a reply's quote uses (`reply-snapshot.ts`): the text on one
 * line, else the kind of the first attachment, else "Message". The checked
 * state itself is the control's `aria-checked`, not part of the name.
 */
export function selectionMarkName<A>(
  message: { readonly text: string; readonly attachments: readonly A[] },
  bubbleOf: (attachment: A) => MediaBubble,
): string {
  return replySnapshotFor(message, "", bubbleOf).excerpt;
}
