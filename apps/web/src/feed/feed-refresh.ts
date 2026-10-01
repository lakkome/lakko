// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Couldn't check for new messages." (`docs/ux/feed.md`, Feed state table,
 * row "refresh failed (network)"): the rule, kept pure and tested.
 *
 * The feed is fed by two independent checks: the background group sweep
 * (`chat/background-receive-scheduler.ts`) and the channel post sync
 * (`ChannelPostSync.tsx`). Each reports, per run, how many sources it tried
 * and how many answered. The banner shows when no source of either kind
 * answered: the latest group check and the latest channel check both reached
 * nothing (a kind with nothing to try does not count against). A check where
 * some source answered is not a failure, and the banner goes at the next
 * check that reaches anything.
 */

/** What one run of one check found. `null`: nothing was tried, so it says
 *  nothing (no sources, or every one skipped for now). */
export type CheckOutcome = "reached" | "nothing" | null;

/** Tallies of one run of a check. */
export interface CheckTally {
  readonly tried: number;
  readonly answered: number;
}

export function outcomeOf(tally: CheckTally): CheckOutcome {
  if (tally.answered > 0) return "reached";
  if (tally.tried > 0) return "nothing";
  return null;
}

/** The latest outcome per kind. */
export interface RefreshState {
  readonly groups: CheckOutcome;
  readonly channels: CheckOutcome;
}

export const INITIAL_REFRESH_STATE: RefreshState = { groups: null, channels: null };

/** Fold one run in. A run that tried nothing leaves the kind as it was, so a
 *  sweep where every group is backed off neither raises nor clears the banner. */
export function recordCheck(
  state: RefreshState,
  kind: "groups" | "channels",
  tally: CheckTally,
): RefreshState {
  const outcome = outcomeOf(tally);
  if (outcome === null || outcome === state[kind]) return state;
  return { ...state, [kind]: outcome };
}

/** Banner on: something was tried, and nothing of any kind answered. */
export function refreshFailed(state: RefreshState): boolean {
  if (state.groups === "reached" || state.channels === "reached") return false;
  return state.groups === "nothing" || state.channels === "nothing";
}
