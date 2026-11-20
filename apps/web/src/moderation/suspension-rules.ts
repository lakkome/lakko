// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0089 step 4, `docs/ux/reporting-and-moderation.md` section 3 - what
 * each mark site shows, given the block register's answer and the standing
 * the moderation log confirmed. Pure, so every site's decision is tested
 * here rather than in the views.
 */

import type { SuspensionStanding } from "./suspension-marks";

/** Which of the two person marks a one-to-one banner or a card line shows:
 *  a block wins over a suspension; nothing when neither applies. */
export function personMark(blocked: boolean, suspended: boolean): "blocked" | "suspended" | null {
  if (blocked) return "blocked";
  if (suspended) return "suspended";
  return null;
}

/** A contact request is dropped unseen only on a VERIFIED suspension: a
 *  timeout or an answer that could not be checked shows it as before. */
export function dropsRequestFrom(standing: SuspensionStanding): boolean {
  return standing === "suspended";
}

/** Search lists no suspended account. Results are shown at once and drop
 *  out as standings arrive, so nothing waits on the check. */
export function withoutSuspended<T extends { readonly keyB64: string }>(
  people: readonly T[],
  isSuspended: (keyB64: string) => boolean,
): T[] {
  return people.filter((p) => !isSuspended(p.keyB64));
}

/** The invite page of a link whose issuer is suspended offers no Add. */
export function inviteOffersAdd(ownAccountSuspended: boolean, inviterSuspended: boolean): boolean {
  return !ownAccountSuspended && !inviterSuspended;
}

/** The bound a contact request's arrival waits for the inviter's standing.
 *  Chosen. */
export const REQUEST_STANDING_BOUND_MS = 3_000;
