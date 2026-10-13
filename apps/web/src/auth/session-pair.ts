// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The session pair (ADR-0073 section 3): a short-lived access token every
 * service verifies statelessly, and a refresh credential only
 * `POST /auth/session/refresh` accepts, plus the key generation both were
 * issued under. Every session-landing response of `lakko-svc-auth` carries
 * the five fields flattened into one object; this module parses them once
 * and holds the one timing rule the token provider applies.
 *
 * Both tokens are opaque strings to the client. The generation is kept
 * beside them because it is what a device compares with the generation the
 * directory publishes (`landing-rule.ts`) to tell that its own keys are
 * stale without a server round trip.
 */

/** What every session landing returns and what the device persists. */
export interface SessionPair {
  /** The `v2.` access token, the bearer for every service. */
  readonly sessionToken: string;
  /** Access-token expiry, unix milliseconds. */
  readonly sessionExpiresAtMs: number;
  /** The `r1.` refresh credential; a bearer anywhere else is a plain 401. */
  readonly refreshToken: string;
  /** Refresh-credential expiry, unix milliseconds (slides on every refresh). */
  readonly refreshExpiresAtMs: number;
  /** The account's key generation the pair was issued under. */
  readonly keyGeneration: bigint;
}

/** The pair's five fields as the server writes them (snake_case, flattened). */
export interface SessionPairWire {
  session_token: string;
  session_expires_at_ms: number;
  refresh_token: string;
  refresh_expires_at_ms: number;
  key_generation: number | string | bigint;
}

/** Refresh ahead of expiry when this much (or less) access life remains.
 *  Chosen (ADR-0073 section 3.3), not measured. */
export const REFRESH_AHEAD_MS = 120_000;

/** `true` when `value` is a finite, non-negative millisecond timestamp. */
function isTimestamp(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

/**
 * Read the pair out of a landing response. Throws on a response that lacks
 * any field or carries one of the wrong shape: a landing that cannot be
 * persisted whole must not land at all (a half pair could never refresh).
 * The generation accepts a number, a decimal string or a bigint (JSON has no
 * u64, and the server writes a plain number today).
 */
export function sessionPairOf(wire: Partial<SessionPairWire> | null | undefined): SessionPair {
  if (wire === null || wire === undefined) {
    throw new Error("session pair: the response carried no session");
  }
  const {
    session_token: sessionToken,
    session_expires_at_ms: sessionExpiresAtMs,
    refresh_token: refreshToken,
    refresh_expires_at_ms: refreshExpiresAtMs,
    key_generation: generation,
  } = wire;
  if (typeof sessionToken !== "string" || sessionToken.length === 0) {
    throw new Error("session pair: session_token missing");
  }
  if (!isTimestamp(sessionExpiresAtMs)) {
    throw new Error("session pair: session_expires_at_ms missing");
  }
  if (typeof refreshToken !== "string" || refreshToken.length === 0) {
    throw new Error("session pair: refresh_token missing");
  }
  if (!isTimestamp(refreshExpiresAtMs)) {
    throw new Error("session pair: refresh_expires_at_ms missing");
  }
  let keyGeneration: bigint;
  try {
    keyGeneration =
      typeof generation === "bigint"
        ? generation
        : typeof generation === "number" && Number.isInteger(generation)
          ? BigInt(generation)
          : typeof generation === "string" && /^[0-9]+$/.test(generation)
            ? BigInt(generation)
            : (() => {
                throw new Error("not a generation");
              })();
  } catch {
    throw new Error("session pair: key_generation missing");
  }
  if (keyGeneration < 1n) throw new Error("session pair: key_generation below 1");
  return { sessionToken, sessionExpiresAtMs, refreshToken, refreshExpiresAtMs, keyGeneration };
}

/** What the provider does before handing out the access token. */
export type TokenDecision =
  /** Hand out the access token as it is. */
  | "use-current"
  /** Refresh first (the access token is expired or has at most
   *  {@link REFRESH_AHEAD_MS} left), then hand out the new one. */
  | "refresh-first"
  /** The refresh credential itself has expired: a sign-in is needed and no
   *  request is worth sending with what is held. */
  | "refresh-expired";

/**
 * The timing rule of the token provider (ADR-0073 section 3.3). Pure: the
 * caller passes the clock, so a test can place `nowMs` on either side of
 * each boundary.
 */
export function tokenDecision(
  pair: Pick<SessionPair, "sessionExpiresAtMs" | "refreshExpiresAtMs">,
  nowMs: number,
): TokenDecision {
  if (pair.refreshExpiresAtMs <= nowMs) return "refresh-expired";
  if (pair.sessionExpiresAtMs - nowMs <= REFRESH_AHEAD_MS) return "refresh-first";
  return "use-current";
}

/**
 * The account segment of an access token or a refresh credential
 * (`v2.<account>.<gen>.<exp>.<tag>` / `r1.<account>.<gen>.<exp>.<tag>`), or
 * the whole string when it has no such shape. Used only to tell one
 * account's tokens from another's; never trusted for anything else.
 */
export function tokenAccountSegment(token: string): string {
  const parts = token.split(".");
  return parts.length >= 4 && parts[1] !== undefined && parts[1].length > 0 ? parts[1] : token;
}
