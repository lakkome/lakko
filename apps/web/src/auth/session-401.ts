// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a `401` means (ADR-0073 sections 3.2, 3.3 and 3.5).
 *
 * Three services can refuse a bearer, and the refresh route can refuse the
 * refresh credential, with one of two JSON codes or with no body at all:
 *
 *   - `key-generation-superseded` (auth's bearer middleware, the refresh
 *     route, discovery's two identity writes): the account's keys were reset
 *     on another device. This device is signed out and runs the staged
 *     wipe, keeping only the recall record (ADR-0074 section 1.1; a device
 *     that never held the keys keeps nothing); nothing is retried and
 *     nothing is published first.
 *   - `device-revoked` (auth's bearer middleware and the refresh route,
 *     ADR-0074 section 0.4; the server reads the key generation FIRST, so
 *     only a credential of the current generation is ever told this):
 *     this device was removed from the account on another device. It is
 *     signed out and wipes everything by itself, with no recall record;
 *     nothing is retried. Only a `401` carries it: discovery's
 *     `403 device-revoked` on a KeyPackage publish is not proof of removal
 *     and reads as any other refusal here.
 *   - `refresh-invalid` (the refresh route only): the credential is bad,
 *     expired or names no account, or this device logged out. The device
 *     lands on the sign-in screen, nothing wiped: the keys are still this
 *     account's. A logout never answers `device-revoked`.
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
export const CODE_DEVICE_REVOKED = "device-revoked";
/** ADR-0099 section 4: auth's bearer routes refuse a suspended account's
 *  access token with `403` and this code (no credentials); the refresh route
 *  answers it with the restricted session. */
export const CODE_ACCOUNT_SUSPENDED = "account-suspended";

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
  /** This device was removed from the account: sign out and wipe, never
   *  retry. */
  | "device-revoked"
  /** Refresh once, then send the request once more with the new token. */
  | "refresh-and-retry"
  /** Report the `401` to the caller as it is. */
  | "report"
  /** ADR-0099: `403 account-suspended` from a bearer route of auth. The
   *  provider refreshes once (the refresh answers with the restricted
   *  session) and the caller gets the refusal as it is. */
  | "suspended";

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
  if (input.status === 403 && input.errorCode === CODE_ACCOUNT_SUSPENDED) return "suspended";
  if (input.status !== 401) return "pass";
  // The two codes never arrive together and neither outranks the other
  // here: the server reads the generation before the device floor (ADR-0074
  // "T3 as built", item 1), so a credential of an older generation is always
  // told `key-generation-superseded` (the staged wipe and the recall record)
  // and only one of the current generation `device-revoked` (the full wipe,
  // no recall record).
  if (input.errorCode === CODE_DEVICE_REVOKED) return "device-revoked";
  if (input.errorCode === CODE_KEY_GENERATION_SUPERSEDED) return "superseded";
  if (input.retried || !input.refreshAvailable) return "report";
  return "refresh-and-retry";
}

/** What a `401` from `POST /auth/session/refresh` means. */
export type RefreshRefusal =
  /** The keys were reset elsewhere: sign out and wipe. */
  | "superseded"
  /** This device was removed from the account: sign out and wipe. */
  | "device-revoked"
  /** Bad, expired or unknown credential, or a logout: the sign-in screen,
   *  nothing wiped. Any `401` that is neither of the two codes above reads
   *  this way, so a body the client cannot parse never wipes a device. */
  | "invalid";

export function refreshRefusal(errorCode: string | null): RefreshRefusal {
  if (errorCode === CODE_DEVICE_REVOKED) return "device-revoked";
  return errorCode === CODE_KEY_GENERATION_SUPERSEDED ? "superseded" : "invalid";
}
