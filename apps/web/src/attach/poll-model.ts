// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 / UX 7.2, 8.2 — the views a poll card and a checklist card draw,
 * from a tally the core folded. Pure: no wasm, no store, so the rules here
 * (who may vote, what is shown when, who may tick) are tested directly.
 */
import type { ChecklistItemRef, PollBody } from "../../wasm/lakko_ffi_wasm";
import type { ChecklistView, PollResultSection, PollView } from "./types";
import { pollPercents } from "./poll-view";

/** A folded tally, whichever fold produced it. */
export interface TallyLike {
  readonly counts: readonly number[];
  readonly voters: number;
  readonly closed: boolean;
  /** Public polls only: who chose what. */
  readonly choicesByVoter?: readonly { readonly voter: string; readonly choices: readonly number[] }[];
}

export interface PollViewInput {
  readonly body: PollBody;
  /** A channel poll is anonymous whatever its flags. */
  readonly channel: boolean;
  readonly verified: boolean;
  readonly tally: TallyLike | null;
  /** The person's own choice (register, a public vote, or the queued one). */
  readonly ownChoice: readonly number[] | undefined;
  readonly sending: boolean;
  readonly failed: boolean;
  /** The conversation takes this person's ops (live membership). */
  readonly canAct: boolean;
  readonly nowMs: number;
}

export function pollViewOf(i: PollViewInput): PollView {
  const body = i.body;
  const anonymous = i.channel || body.anonymous != null;
  const quiz = body.quiz != null;
  const timeClosed = body.closeAtMs != null && i.nowMs >= body.closeAtMs;
  const closed = (i.tally?.closed ?? false) || timeClosed;
  const own = i.ownChoice ?? [];
  const voted = own.length > 0 || i.sending;
  const counts = body.options.map((_, idx) => (i.verified ? i.tally?.counts[idx] ?? 0 : 0));
  const voters = i.verified ? i.tally?.voters ?? 0 : 0;
  const percents = pollPercents(counts, voters, body.multiple);
  const revealQuiz = quiz && (voted || closed);
  const options = body.options.map((text, idx) => ({
    text,
    count: counts[idx] ?? 0,
    percent: percents[idx] ?? 0,
    mine: own.includes(idx),
    ...(revealQuiz ? { correct: body.quiz?.correct === idx } : {}),
  }));
  const explanation = revealQuiz && body.quiz !== undefined && body.quiz !== null && body.quiz.explanation.length > 0
    ? body.quiz.explanation
    : undefined;
  return {
    question: body.question,
    anonymous,
    quiz,
    multiple: body.multiple,
    options,
    voters,
    voted,
    closed,
    ...(!closed && body.closeAtMs != null ? { closesAtMs: body.closeAtMs } : {}),
    ...(explanation === undefined ? {} : { explanation }),
    sending: i.sending,
    failed: i.failed,
    canVote: i.canAct && i.verified && !closed && !i.sending,
    unverifiable: !i.verified,
    canViewResults: !anonymous && i.verified && (voted || closed),
  };
}

/** UX 7.4 — the results of a public poll: per option, who chose it. */
export function pollResultSections(
  body: PollBody,
  tally: TallyLike,
  nameOf: (keyB64: string) => string,
): PollResultSection[] {
  return body.options.map((text, idx) => {
    const voters = (tally.choicesByVoter ?? [])
      .filter((v) => v.choices.includes(idx))
      .map((v) => ({ keyB64: v.voter, name: nameOf(v.voter) }));
    return { text, count: tally.counts[idx] ?? 0, voters };
  });
}

/** Who may vote at all: the menu's `Retract vote`, offered on a poll the
 *  person voted in that is not closed (UX 7.3). */
export function canRetract(view: PollView): boolean {
  return view.voted && !view.closed && !view.sending && !view.unverifiable;
}

/** UX 7.3 — `Close poll`: the creator, and in a group an owner or admin. */
export function canClosePoll(args: {
  readonly isAuthor: boolean;
  readonly isAdmin: boolean;
  readonly group: boolean;
  readonly closed: boolean;
}): boolean {
  if (args.closed) return false;
  return args.isAuthor || (args.group && args.isAdmin);
}

/** A checklist item as the folded state reports it. */
export interface ChecklistItemState {
  readonly item: ChecklistItemRef;
  readonly text: string;
  readonly doneBy?: string | null;
}

export interface ChecklistViewInput {
  readonly title: string;
  readonly othersAdd: boolean;
  readonly othersMark: boolean;
  readonly items: readonly ChecklistItemState[];
  readonly closed: boolean;
  readonly isAuthor: boolean;
  readonly canAct: boolean;
  /** A one-to-one: the ticker's name shows only when it was the other person. */
  readonly oneToOne: boolean;
  readonly ownKeyB64: string | undefined;
  readonly firstNameOf: (keyB64: string) => string;
  /** A channel names no ticker (publishers tick as the channel). */
  readonly showTicker?: boolean;
}

export function checklistViewOf(i: ChecklistViewInput): ChecklistView {
  const mayTick = i.canAct && !i.closed && (i.isAuthor || i.othersMark);
  const items = i.items.map((it) => {
    const by = it.doneBy ?? undefined;
    const showName = i.showTicker !== false && by !== undefined && !(i.oneToOne && by === i.ownKeyB64);
    return {
      ref: it.item,
      text: it.text,
      done: by !== undefined,
      ...(showName ? { doneByName: i.firstNameOf(by) } : {}),
      canToggle: mayTick,
    };
  });
  return {
    title: i.title,
    items,
    doneCount: items.filter((x) => x.done).length,
    canAdd: i.canAct && !i.closed && (i.isAuthor || i.othersAdd),
    closed: i.closed,
  };
}

/** The first word of a display name (UX 8.2: "the first name of whoever
 *  ticked it"). */
export function firstName(name: string): string {
  const trimmed = name.trim();
  const space = trimmed.search(/\s/);
  return space <= 0 ? trimmed : trimmed.slice(0, space);
}
