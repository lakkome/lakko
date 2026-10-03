// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a `401` means (ADR-0065 sections 3.2, 3.3 and 3.5).
 *
 * Three services can refuse a bearer, and the refresh route can refuse the
 * refresh credential, with one of two JSON codes or with no body at all:
 *
 *   - `key-generation-superseded` (auth's bearer middleware, the refresh
 *     route, discovery's two identity writes): the account's keys were reset
 *     on another device. This device is signed out and wipes itself; nothing
 *     is retried and nothing is published first.
 *   - `refresh-invalid` (the refresh route only): the credential is bad,
 *     expired or names no account. The device lands on the sign-in screen,
 *     nothing wiped: the keys are still this account's.
 *   - a plain `401` from a bearer route: the access token expired (or never
 *     verified). One refresh, then one retry; a second `401` is reported as
 *     it always was.
 *
 * The rules are pure functions so the fetch wrapper that applies them
 * (`session-token-provider.ts`) carries no judgement of its own.
 */

/** The two codes the server names a refusal with. */
export const CODE_KEY_GENERATION_SUPERSEDED = "key-generation-superseded";
export const CODE_REFRESH_INVALID = "refresh-invalid";

/** The `error` code of a `{"error": "<code>", ...}` body, or `null`. */
export function errorCodeOf(body: string | null | undefined): string | null {
  if (body === null || body === undefined || body.length === 0) return null;
  try {
    const parsed = JSON.parse(body) as { error?: unknown };
    return typeof parsed.error === "string" ? parsed.error : null;
  } catch {
    return null;
  }
}

/** What the caller of a bearer route does with its response. */
export type BearerRefusal =
  /** Not a `401`: nothing for the provider to do. */
  | "pass"
  /** The keys were reset elsewhere: sign out and wipe, never retry. */
  | "superseded"
  /** Refresh once, then send the request once more with the new token. */
  | "refresh-and-retry"
  /** Report the `401` to the caller as it is. */
  | "report";

/**
 * The rule for a bearer route's response.
 *
 * `retried` is whether this request already went out once more after a
 * refresh (a second `401` is reported, never looped); `refreshAvailable`
 * whether the provider holds a refresh credential that has not expired (a
 * request sent with a token the provider does not manage, or after the
 * credential expired, is reported as it is: there is nothing to refresh
 * with).
 */
export function bearerRefusal(input: {
  readonly status: number;
  readonly errorCode: string | null;
  readonly retried: boolean;
  readonly refreshAvailable: boolean;
}): BearerRefusal {
  if (input.status !== 401) return "pass";
  if (input.errorCode === CODE_KEY_GENERATION_SUPERSEDED) return "superseded";
  if (input.retried || !input.refreshAvailable) return "report";
  return "refresh-and-retry";
}

/** What a `401` from `POST /auth/session/refresh` means. */
export type RefreshRefusal =
  /** The keys were reset elsewhere: sign out and wipe. */
  | "superseded"
  /** Bad, expired or unknown credential: the sign-in screen, nothing wiped.
   *  Any `401` that is not the superseded code reads this way, so a body the
   *  client cannot parse never wipes a device. */
  | "invalid";

export function refreshRefusal(errorCode: string | null): RefreshRefusal {
  return errorCode === CODE_KEY_GENERATION_SUPERSEDED ? "superseded" : "invalid";
}
