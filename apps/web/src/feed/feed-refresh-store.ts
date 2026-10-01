// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The reporting seam for the feed's refresh banner: the group sweep and the
 * channel sync report each run here, the Feed screen reads
 * {@link feedRefreshFailed}, and its Retry runs the same checks:
 * {@link requestGroupCheck} plus `requestChannelPostSync`. The rule itself is `feed-refresh.ts`.
 */

import { createSignal } from "solid-js";

import { INITIAL_REFRESH_STATE, recordCheck, refreshFailed, type CheckTally } from "./feed-refresh";

const [state, setState] = createSignal(INITIAL_REFRESH_STATE);

/** `true` while the banner should show. */
export function feedRefreshFailed(): boolean {
  return refreshFailed(state());
}

/** One run of the background group sweep finished. */
export function reportGroupCheck(tally: CheckTally): void {
  setState((s) => recordCheck(s, "groups", tally));
}

/** One run of the channel post sync finished. */
export function reportChannelCheck(tally: CheckTally): void {
  setState((s) => recordCheck(s, "channels", tally));
}

/** Forget everything (sign-out). */
export function resetFeedRefresh(): void {
  setState(INITIAL_REFRESH_STATE);
}

const groupCheckRequests = new Set<() => void>();

/** Registered by the background receive: run one group sweep now. */
export function onGroupCheckRequested(run: () => void): () => void {
  groupCheckRequests.add(run);
  return () => groupCheckRequests.delete(run);
}

/** Run one group sweep now (Retry). A no-op while no receive is running. */
export function requestGroupCheck(): void {
  for (const f of groupCheckRequests) f();
}
