// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The `/g/` join screen's result copy (ADR-0053 live walk, 2026-09-27). The
 * same catalog keys ship on iOS and Android, so they are chosen once here and
 * the screen never shows a raw error: the error is logged and the person gets
 * one of a handful of fixed sentences.
 *
 * In a plain module because `vitest` cannot load a `.tsx` route.
 */
import { CLOCK_SKEW_NOTICE, GROUP_NOT_TAKING_REQUESTS, dsClockSkewNotice } from "../ds/access";
import { INVITE_LINK_RATE_LIMITED } from "../contacts/invite-link";
import { isDiscoveryRateLimited } from "../discovery/client";
import { t } from "../l10n";
import { anonTokenQuotaExhausted } from "../tokens/anon-token-pool";
import { ACTION_ALLOWANCE_USED } from "../tokens/token-copy";

/** The join landed. */
export const JOIN_SUCCEEDED = (): string => t("group.join.succeeded");
/** The interim line while web opens the chat it just joined. */
export const JOIN_SUCCEEDED_OPENING = (): string => t("group.join.succeeded_opening");
/** Any failure without a more specific sentence. */
export const JOIN_REQUEST_FAILED = (): string => t("group.join.request_failed");
/** The primary button under {@link JOIN_REQUEST_FAILED}; it re-runs the request. */
export const JOIN_TRY_AGAIN = (): string => t("common.try_again");

/** What the screen says about a failed request, and whether it offers `Try
 *  again`. `render` re-renders the sentence in the current language. */
export interface JoinFailureNotice {
  readonly render: () => string;
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
  if (isGroupNotClaimed(err)) return { render: GROUP_NOT_TAKING_REQUESTS, offerRetry: false };
  if (dsClockSkewNotice(err) !== null) return { render: CLOCK_SKEW_NOTICE, offerRetry: false };
  // The ledger's consume answered 429: the link is fine, the request can be
  // sent again after a pause (ADR-0028 short-link amendment).
  if (isDiscoveryRateLimited(err)) return { render: INVITE_LINK_RATE_LIMITED, offerRetry: true };
  // ADR-0054 S3: the consume and the knock each spend an anonymous token. A
  // day's allowance used up says so, without a retry; tokens off or none to
  // be had is the ordinary network failure.
  if (anonTokenQuotaExhausted(err) !== null) {
    return { render: ACTION_ALLOWANCE_USED, offerRetry: false };
  }
  return { render: JOIN_REQUEST_FAILED, offerRetry: true };
}
