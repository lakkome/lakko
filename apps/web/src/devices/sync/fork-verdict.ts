// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Is this device's DeviceGroup forked off the account's chain?
 *
 * A device can end up holding its DeviceGroup at epoch N with its journal
 * cursor past the commit that moved the account to N+1 (a removal it made
 * itself through a stale handle, before the persist-order fix: MLS cannot
 * apply its own commit, so the old epoch was persisted with the cursor past
 * it). It has then read the whole journal, the DS chain is at N+1 registered
 * by the account root, and nothing it can ever read will move it. The sync
 * runtime detects that here and re-founds the DeviceGroup.
 *
 * The signal: a signed read of the claimed DeviceGroup journal names the
 * newest epoch registered on the DS (`latest_epoch`). When the read after the
 * cursor is DRAINED (no rows, nothing more) and the DS is still ahead of the
 * local epoch, nothing left in the journal can close the gap. That can be
 * legitimate for a short while: ADR-0053 registers an epoch BEFORE the commit
 * that reaches it is published, so another device may have registered N+1 and
 * not published its commit yet. The verdict therefore waits out a grace window
 * before it says `forked`.
 *
 * Pure: the caller keeps `suspectSince` between polls and passes the clock.
 */

/**
 * How long a drained journal may stay behind the DS chain before this device
 * counts as forked. Chosen, not measured: it has to cover a sibling device's
 * register-then-publish window (ADR-0053), which is one registration request
 * plus one publish, with room for a slow network in between.
 */
export const FORK_GRACE_MS = 120_000;

/** The verdict of one poll. */
export type ForkVerdict =
  /** Nothing to suspect; the caller clears its timer. */
  | { readonly kind: "in-sync" }
  /** Behind with nothing left to read, since `since`; keep the timer. */
  | { readonly kind: "suspect"; readonly since: number }
  /** Behind for longer than {@link FORK_GRACE_MS}: repair. */
  | { readonly kind: "forked" };

/**
 * @param localEpoch - the sync handle's DeviceGroup epoch at the time of the
 *   check.
 * @param drained - this poll's fetch after the cursor returned ZERO rows and
 *   `has_more` was false. A fetch that returned rows is never drained, even if
 *   none of them applied.
 * @param latestEpoch - the DS's `latest_epoch` for the journal, absent when
 *   the DS did not name one.
 * @param suspectSince - when the current suspicion started, from the previous
 *   verdict; absent when there is none.
 * @param now - the current time, in ms.
 */
export function forkVerdict(
  localEpoch: bigint,
  drained: boolean,
  latestEpoch: bigint | undefined,
  suspectSince: number | undefined,
  now: number,
): ForkVerdict {
  if (!drained || latestEpoch === undefined || latestEpoch <= localEpoch) {
    return { kind: "in-sync" };
  }
  if (suspectSince === undefined) return { kind: "suspect", since: now };
  if (now - suspectSince >= FORK_GRACE_MS) return { kind: "forked" };
  return { kind: "suspect", since: suspectSince };
}
