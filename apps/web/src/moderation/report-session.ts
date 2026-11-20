// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0099 section 2, UX `reporting-and-moderation.md` 2.2 - the one report
 * sheet on this page and what it holds.
 *
 * The sheet is opened from many places (a message menu, a person's card, a
 * request row, a group's or channel's info, a post menu, a Discover card) and
 * `Add messages` takes the person into a conversation's select mode and back,
 * across routes. So the draft lives here, in one module-level slot, and the
 * sheet host (`ReportSheet.tsx`, mounted once in the router's root layout) and
 * the conversation view both read it. Nothing in it is persisted: a reload
 * drops an unsent report, as it drops an unsent selection.
 */

import { createSignal, type Accessor } from "solid-js";

import { newDraft, togglePick, type ReportDraft, type SheetPhase } from "./report-model";

/** One open report. */
export interface ReportSession {
  readonly draft: ReportDraft;
  readonly phase: SheetPhase;
  /** The sheet shows its "Choose messages" list in place of the form. */
  readonly picking: boolean;
  /** What `Leave the group` / `Unsubscribe` runs, from the entry point that
   *  knows how (a person's block is the sheet's own). */
  readonly leave: (() => Promise<void>) | null;
}

const [session, setSession] = createSignal<ReportSession | null>(null);
const [sentNotice, setSentNotice] = createSignal(0);

/** Reactive: the open report, or `null`. */
export const reportSession: Accessor<ReportSession | null> = session;

/** Reactive: bumped each time a report was sent (the sheet's notice). */
export const reportSentVersion: Accessor<number> = sentNotice;

/** Open the sheet. A report already open is replaced: one at a time. */
export function openReport(input: Parameters<typeof newDraft>[0] & { readonly leave?: () => Promise<void> }): void {
  setSession({ draft: newDraft(input), phase: "editing", picking: false, leave: input.leave ?? null });
}

/** Close the sheet; the draft goes. */
export function closeReport(): void {
  setSession(null);
}

/** Change the draft (a reason, the comment, the checkbox). */
export function updateDraft(change: (draft: ReportDraft) => ReportDraft): void {
  const s = session();
  if (s === null) return;
  setSession({ ...s, draft: change(s.draft), phase: s.phase === "sending" ? "sending" : "editing" });
}

/** Move the sheet to a phase. */
export function setReportPhase(phase: SheetPhase): void {
  const s = session();
  if (s === null) return;
  setSession({ ...s, phase });
}

/** `Add messages`: the sheet shows its "Choose messages" list. */
export function startPicking(): void {
  const s = session();
  if (s === null || s.draft.pickFrom === null) return;
  setSession({ ...s, picking: true });
}

/** One tap in select mode. `false` when the tap was refused at the limit. */
export function togglePicked(messageId: number): boolean {
  const s = session();
  if (s === null) return false;
  const next = togglePick(s.draft.picked, messageId);
  if (next.refused) return false;
  setSession({ ...s, draft: { ...s.draft, picked: next.picked } });
  return true;
}

/** `Done`: the form comes back with the picks as they are. */
export function finishPicking(): void {
  const s = session();
  if (s === null) return;
  setSession({ ...s, picking: false });
}

/** The report was sent: the sheet closes and the notice shows. */
export function reportSent(): void {
  setSession(null);
  setSentNotice((v) => v + 1);
}
