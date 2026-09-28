// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The `/g/` join screen's result copy (ADR-0053 live walk, 2026-09-27). The
 * same sentences ship on iOS and Android, so they are defined once here and
 * the screen never shows a raw error: the error is logged and the person gets
 * one of a handful of fixed sentences.
 *
 * In a plain module because `vitest` cannot load a `.tsx` route.
 */
import { CLOCK_SKEW_NOTICE, GROUP_NOT_TAKING_REQUESTS, dsClockSkewNotice } from "../ds/access";
import { INVITE_LINK_RATE_LIMITED } from "../contacts/invite-link";
import { isDiscoveryRateLimited } from "../discovery/client";

/** The join landed. */
export const JOIN_SUCCEEDED = "You joined the group.";
/** The interim line while web opens the chat it just joined. */
export const JOIN_SUCCEEDED_OPENING = "You joined the group. Opening the chat...";
/** Any failure without a more specific sentence. */
export const JOIN_REQUEST_FAILED =
  "Your request didn't go through. Check your connection and try again.";
/** The primary button under {@link JOIN_REQUEST_FAILED}; it re-runs the request. */
export const JOIN_TRY_AGAIN = "Try again";

/** What the screen says about a failed request, and whether it offers `Try again`. */
export interface JoinFailureNotice {
  readonly message: string;
  readonly offerRetry: boolean;
}

/** `true` when `err`, or anything it wraps, is the knock's "no epoch" refusal. */
function isGroupNotClaimed(err: unknown): boolean {
  let current: unknown = err;
  for (let depth = 0; depth < 8 && current !== undefined && current !== null; depth += 1) {
    if (typeof current !== "object") return false;
    const r = current as { name?: unknown; cause?: unknown; variant?: { cause?: unknown } };
    if (r.name === "GroupNotClaimedError") return true;
    current = r.cause ?? r.variant?.cause;
  }
  return false;
}

/** The sentence for a failed join request. */
export function joinFailureNotice(err: unknown): JoinFailureNotice {
  if (isGroupNotClaimed(err)) return { message: GROUP_NOT_TAKING_REQUESTS, offerRetry: false };
  if (dsClockSkewNotice(err) !== null) return { message: CLOCK_SKEW_NOTICE, offerRetry: false };
  // The ledger's consume answered 429: the link is fine, the request can be
  // sent again after a pause (ADR-0028 short-link amendment).
  if (isDiscoveryRateLimited(err)) return { message: INVITE_LINK_RATE_LIMITED, offerRetry: true };
  return { message: JOIN_REQUEST_FAILED, offerRetry: true };
}
