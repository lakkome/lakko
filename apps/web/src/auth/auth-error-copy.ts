// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the auth screens say when something fails (docs/ux/client-redesign.md
 * section 6): one sentence on what happened, one on what to do, never the
 * transport or the protocol.
 *
 * `AuthContext` keeps throwing what it throws; this maps those errors to
 * screen copy at the edge, so the enumeration-safe collapse it performs is
 * preserved: an unknown username and a wrong password reach the user as the
 * SAME sentence, and so do the corresponding server statuses.
 *
 * Pure, so the collapse is pinned by a test.
 */
import { AuthHttpError, AuthNetworkError } from "./client";
import { DiscoveryHttpError, DiscoveryNetworkError } from "../discovery/client";
import { isAnonTokenNetworkFailure } from "../tokens/anon-token-pool";
import { t } from "../l10n";
import { keyedErrorText } from "../l10n/keyed-error";
import { usernameRuleText } from "../profile/rules";
import {
  PAIRING_NEEDS_USERNAME,
  PAIRING_UNSUPPORTED,
  PAIRING_WRONG_ACCOUNT,
} from "../devices/pairing";

// ADR-0061: every sentence below is a catalog key rendered when the error is
// shown, so the language current at that moment is used.
const offline = (): string => t("common.error.offline");
const unavailable = (): string => t("common.error.unavailable");

// ---------------------------------------------------------------------------
// Server limits (ADR-0054 S4, docs/ux/anti-abuse.md section 2)
//
// Every auth limit answers `429 {"error": code, "retry_after_secs": N}` with
// `Retry-After: N`. A limit is the server's decision: it never signs anyone
// out, never reads as "invalid", and says when to try again.
// ---------------------------------------------------------------------------

/** `too-many-attempts`: login, recovery login, reset, TOTP, three words,
 *  device authorize. */
export const tooManyAttempts = (): string => t("common.error.too_many_attempts");
/** `recovery-code-send-limited`: the email / SMS code send. */
export const recoveryCodeSendLimited = (): string => t("recovery.error.code_send_limited");
/** `device-request-limited`: "Is this you?" prompts on the other devices. */
export const deviceRequestLimited = (): string => t("device_link.error.request_limited");
/** `signup-rate-limited`: signup start or its challenge. */
export const signupRateLimited = (): string => t("auth.signup.error.rate_limited");

/** `handle-bind-limited` (`PUT /auth/login-handle`, added 2026-09-28):
 *  choosing or changing a username too often. In minutes, rounded up; over
 *  an hour (or with no wait given) it says "later" instead of a number. */
export function handleBindLimitedMessage(retryAfterSecs: number | null): string {
  if (retryAfterSecs === null || retryAfterSecs > 3600) return t("username.error.bind_limited_later");
  const minutes = Math.max(1, Math.ceil(retryAfterSecs / 60));
  return t("username.error.bind_limited_minutes", { minutes });
}

/** `true` when `err` is auth's `429 handle-bind-limited`. */
export function isHandleBindLimited(err: unknown): boolean {
  return err instanceof AuthHttpError && err.status === 429 && err.errorCode === "handle-bind-limited";
}

/** The attempts copy, in minutes when the server asks for more than one. */
export function tooManyAttemptsMessage(retryAfterSecs: number | null): string {
  if (retryAfterSecs !== null && retryAfterSecs > 60) {
    return t("common.error.too_many_attempts_minutes", {
      minutes: Math.ceil(retryAfterSecs / 60),
    });
  }
  return tooManyAttempts();
}

/**
 * The copy for a server limit, or `null` when `err` is not one. Covers the
 * 429 codes and the `503 rate-limiter-unavailable` (the limiter's store is
 * down: fail closed, shown as the generic failure).
 */
export function authLimitMessage(err: unknown): string | null {
  if (!(err instanceof AuthHttpError)) return null;
  const code = err.errorCode;
  if (err.status === 503 && code === "rate-limiter-unavailable") return unavailable();
  if (err.status !== 429) return null;
  switch (code) {
    case "recovery-code-send-limited":
      return recoveryCodeSendLimited();
    case "device-request-limited":
      return deviceRequestLimited();
    case "signup-rate-limited":
      return signupRateLimited();
    case "handle-bind-limited":
      return handleBindLimitedMessage(err.retryAfter);
    default:
      return tooManyAttemptsMessage(err.retryAfter);
  }
}

/** `true` when `err` is the signup's own network limit, so the form offers
 *  `Try again` and keeps its contents. */
export function isSignupRateLimited(err: unknown): boolean {
  return err instanceof AuthHttpError && err.status === 429 && err.errorCode === "signup-rate-limited";
}

/** The one sentence a failed password login shows, whatever failed. */
export const wrongCredentials = (): string => t("auth.error.wrong_credentials");

export function isOffline(err: unknown): boolean {
  return err instanceof AuthNetworkError || err instanceof DiscoveryNetworkError;
}

function status(err: unknown): number | null {
  if (err instanceof AuthHttpError || err instanceof DiscoveryHttpError) return err.status;
  return null;
}

/** A message the context already wrote for the user (sentence case, ends
 *  with a full stop) is passed through; a bare diagnostic is not. English
 *  only: a sentence written for a screen is a {@link KeyedError}, whose text
 *  is taken first by {@link screenText}. */
function isScreenCopy(message: string): boolean {
  return /^[A-Z]/.test(message) && /[.!?]$/.test(message) && !message.includes("→");
}

/** The pairing codec's own sentences (`devices/pairing.ts`, pinned in
 *  English by its tests), rendered from their keys. */
export function pairingText(message: string): string | null {
  switch (message) {
    case PAIRING_WRONG_ACCOUNT:
      return t("device_link.pairing.wrong_account");
    case PAIRING_UNSUPPORTED:
      return t("device_link.pairing.unsupported");
    case PAIRING_NEEDS_USERNAME:
      return t("device_link.pairing.needs_username");
    default:
      return null;
  }
}

/**
 * The text of an error that was written for a screen: a keyed error in the
 * current language, the username rule's reason, or (legacy) an English
 * sentence the context wrote. `null` for a diagnostic.
 */
export function screenText(err: unknown): string | null {
  const keyed = keyedErrorText(err);
  if (keyed !== null) return keyed;
  // The identity checks of a restore carry English `message`s for the logs;
  // whichever form shows them (the three words, a recovery form), they render
  // from their kind (ADR-0061 section 6).
  const check = identityCheckMessage(err);
  if (check !== null) return check;
  const rule = usernameRuleText(err);
  if (rule !== null) return rule;
  const pairing = err instanceof Error ? pairingText(err.message) : null;
  if (pairing !== null) return pairing;
  if (err instanceof Error && isScreenCopy(err.message)) return err.message;
  return null;
}

/** The login form. */
export function loginErrorMessage(err: unknown): string {
  if (isOffline(err)) return offline();
  const limit = authLimitMessage(err);
  if (limit !== null) return limit;
  // The username lookup could not get an anonymous token (ADR-0054 S3).
  if (isAnonTokenNetworkFailure(err)) return offline();
  const s = status(err);
  if (s === 503) return unavailable();
  // 401 / 404 / a lookup miss / "invalid credentials": all the same sentence.
  if (s === 401 || s === 404 || s === 400) return wrongCredentials();
  if (err instanceof Error && err.message === "invalid credentials") return wrongCredentials();
  return screenText(err) ?? wrongCredentials();
}

/** The passkey button on the login form. The context throws keyed errors;
 *  the fallback covers a bare diagnostic. */
export function passkeyErrorMessage(err: unknown): string {
  if (isOffline(err)) return offline();
  return screenText(err) ?? t("auth.passkey.error.generic");
}

/** The signup form. */
export function signupErrorMessage(err: unknown): string {
  if (isOffline(err)) return offline();
  const limit = authLimitMessage(err);
  if (limit !== null) return limit;
  if (isAnonTokenNetworkFailure(err)) return offline();
  const s = status(err);
  if (s === 503) return unavailable();
  if (s === 409 || (err instanceof Error && /already taken/.test(err.message))) {
    return t("auth.signup.error.username_taken");
  }
  return screenText(err) ?? t("auth.signup.error.generic");
}

/**
 * `Create account` on a guest account (`guest-account.ts`).
 *
 * The step that sets the password runs the phrase-proved reset, whose own
 * failures read "the phrase does not match" on `/recover`. Here the person
 * typed no phrase, so those collapse into the form's generic sentence.
 */
export function guestClaimErrorMessage(err: unknown): string {
  if (err instanceof Error && err.name === "GuestPhraseUnavailableError") {
    return t("guest.claim.error.phrase_missing");
  }
  if (isOffline(err)) return offline();
  const limit = authLimitMessage(err);
  if (limit !== null) return limit;
  if (isAnonTokenNetworkFailure(err)) return offline();
  const s = status(err);
  if (s === 503) return unavailable();
  if (s === 409 || (err instanceof Error && /already taken/.test(err.message))) {
    return t("auth.signup.error.username_taken");
  }
  const rule = usernameRuleText(err);
  if (rule !== null) return rule;
  return t("guest.claim.error.generic");
}

/** The recovery forms that take the phrase (or the shares) and a new
 *  password. A wrong phrase and an unknown username are one sentence. */
export function recoverErrorMessage(err: unknown): string {
  if (isOffline(err)) return offline();
  const limit = authLimitMessage(err);
  if (limit !== null) return limit;
  if (isAnonTokenNetworkFailure(err)) return offline();
  const s = status(err);
  if (s === 503) return unavailable();
  if (s === 401 || s === 404 || s === 400) return t("recovery.error.no_match");
  if (err instanceof Error && /username not found|invalid credentials/.test(err.message)) {
    return t("recovery.error.no_match");
  }
  return screenText(err) ?? t("recovery.error.generic");
}

/**
 * The identity checks of a restore (ADR-0017): the phrase or the identity
 * from another device must match the account's published identity. Matched
 * on the error's `name` so this pure module does not import `AuthContext`.
 * The mismatch sentence follows the `source` the error carries (a typed
 * phrase or another device); `fallback` is the form's own source, for an
 * error that carries none.
 */
function identityCheckMessage(
  err: unknown,
  fallback: "phrase" | "device" = "phrase",
): string | null {
  if (!(err instanceof Error)) return null;
  if (err.name === "SupersessionPhraseMismatch") {
    const carried = (err as { source?: unknown }).source;
    const source = carried === "device" || carried === "phrase" ? carried : fallback;
    return source === "device"
      ? t("device_link.error.identity_mismatch")
      : t("restore_identity.error.phrase_mismatch");
  }
  if (err.name === "IdentityVerificationUnavailable") {
    return (err as { kind?: string }).kind === "missing"
      ? t("restore_identity.error.not_published")
      : t("restore_identity.error.unreachable");
  }
  return null;
}

/** The typed-phrase restore on a device that has no identity yet. A phrase
 *  the core cannot parse (unknown word, bad checksum) reads as one sentence. */
export function identityRestoreErrorMessage(err: unknown): string {
  if (isOffline(err)) return offline();
  const check = identityCheckMessage(err, "phrase");
  if (check !== null) return check;
  const keyed = keyedErrorText(err);
  if (keyed !== null) return keyed;
  // `RecoveryMnemonic.fromPhrase` throws a core diagnostic on a bad phrase.
  if (err instanceof Error && /mnemonic|phrase|word|checksum/i.test(err.message)) {
    return t("restore_identity.error.invalid_phrase");
  }
  return t("restore_identity.error.generic");
}

/** The another-device restore, on the new device. */
export function deviceRestoreErrorMessage(err: unknown): string {
  if (isOffline(err)) return offline();
  const limit = authLimitMessage(err);
  if (limit !== null) return limit;
  const check = identityCheckMessage(err, "device");
  if (check !== null) return check;
  return screenText(err) ?? t("device_link.error.generic");
}
