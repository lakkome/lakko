// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What an action that spends an anonymous token says when the account's
 * daily allowance is used up (docs/ux/anti-abuse.md section 3). The session
 * is never ended and nothing asks for a password. Tokens switched off on the
 * server, or no token to be had, fail with the action's existing network copy.
 *
 * Push registration has no copy: it is retried silently at the next launch.
 */
import { t } from "../l10n";
import { anonTokenQuotaExhausted, isAnonTokenNetworkFailure } from "./anon-token-pool";

/** Search (a username or channel lookup). */
export const LOOKUP_ALLOWANCE_USED = (): string => t("tokens.lookup_allowance_used");
/** Following a channel (ADR-0059 section 4, docs/ux/feed.md section 7). */
export const SUBSCRIBE_ALLOWANCE_USED = (): string => t("tokens.subscribe_allowance_used");
/** A Discover search (docs/ux/feed.md section 7). */
export const SEARCH_ALLOWANCE_USED = (): string => t("tokens.search_allowance_used");
/** An invite link, a group link, a channel name. */
export const ACTION_ALLOWANCE_USED = (): string => t("tokens.action_allowance_used");

/**
 * The copy for a token failure: on a quota refusal, the search copy for a
 * lookup and the allowance copy for an invite link, a group link or a
 * channel name (chosen by the refused token's context); `networkCopy` on a
 * disabled or unavailable token; `null` when `err` is not a token failure,
 * and for push registration, which is silent. The storage contexts
 * (`vault-claim`, `blob-upload`, `blob-read`) have no copy of their own and
 * take the allowance copy.
 */
export function anonTokenMessage(err: unknown, networkCopy: string): string | null {
  const quota = anonTokenQuotaExhausted(err);
  if (quota !== null) {
    switch (quota.context) {
      case "discovery-lookup":
        return LOOKUP_ALLOWANCE_USED();
      case "push-register":
        return null;
      case "channel-subscribe":
        return SUBSCRIBE_ALLOWANCE_USED();
      case "directory-search":
        return SEARCH_ALLOWANCE_USED();
      default:
        return ACTION_ALLOWANCE_USED();
    }
  }
  if (isAnonTokenNetworkFailure(err)) return networkCopy;
  return null;
}
