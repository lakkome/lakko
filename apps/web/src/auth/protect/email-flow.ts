// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The recovery email's two flows as pure step machines (ADR-0075 sections 6
 * and 7, `docs/ux/identity-recovery.md` 19.2 and 19.3):
 *
 *   - enrol: address -> code -> done;
 *   - recover: username and address -> code -> (authenticator code) -> new
 *     password -> the session. A mailed code never brings keys (the email
 *     vault was removed on 2026-11-26): on a device without them "Restore
 *     your keys" follows the landing.
 *
 * Every answer of the server is read by {@link emailFailure}; the screens
 * draw the state and never branch on a status themselves. Nothing here holds
 * the address or a code.
 */

// ---------------------------------------------------------------------------
// The server's answers
// ---------------------------------------------------------------------------

/** Why a step of either flow did not go through. */
export type EmailFailure =
  /** `400 address-invalid`. */
  | { readonly kind: "address-invalid" }
  /** `401 code-refused`: "That code is not right." */
  | { readonly kind: "code-wrong" }
  /** `410 code-expired` (enrol): "That code has expired." */
  | { readonly kind: "code-expired" }
  /** `429 too-many-attempts` on a code: "Too many attempts. Send a new
   *  code." The flow returns to its first step. */
  | { readonly kind: "used-up" }
  /** `429 email-rate-limited` (or any other limit): the countdown. */
  | { readonly kind: "rate-limited"; readonly retryAfterSecs: number | null }
  /** `502 mail-send-failed`. */
  | { readonly kind: "send-failed" }
  /** `503`: mail is not set up on this server. */
  | { readonly kind: "unavailable" }
  /** `403 fresh-auth-required`: the proof block. */
  | { readonly kind: "proof-required" }
  /** `409 username-required`: a guest account. */
  | { readonly kind: "username-required" }
  /** `401 ticket-refused` / `403 totp-required`: the ticket is gone or out
   *  of step; the flow starts over. */
  | { readonly kind: "start-over" }
  /** No answer. */
  | { readonly kind: "offline" }
  /** `409 last-factor` (remove): the email is the account's last second
   *  way in; the app's own dialog says so. */
  | { readonly kind: "last-factor" }
  /** Anything else (`400`, `409 handle-mismatch`, a server error). */
  | { readonly kind: "error" };

/** Which request the answer belongs to: the same status means different
 *  things on a code route and on a letter route. */
export type EmailRequest =
  | "enrol-start"
  | "enrol-finish"
  | "remove"
  | "recover-start"
  | "recover-verify"
  | "recover-totp"
  | "recover-password";

/** Read one answer. `status` is `null` when none arrived. */
export function emailFailure(
  request: EmailRequest,
  answer: {
    readonly status: number | null;
    readonly errorCode: string | null;
    readonly retryAfterSecs: number | null;
  },
): EmailFailure {
  const { status, errorCode } = answer;
  if (status === null) return { kind: "offline" };
  if (status === 400 && errorCode === "address-invalid") return { kind: "address-invalid" };
  if (status === 401 && errorCode === "code-refused") return { kind: "code-wrong" };
  if (status === 410) return { kind: "code-expired" };
  if (status === 403 && errorCode === "fresh-auth-required") return { kind: "proof-required" };
  if (status === 409 && errorCode === "username-required") return { kind: "username-required" };
  if (status === 409 && errorCode === "last-factor") return { kind: "last-factor" };
  if (
    (status === 401 && errorCode === "ticket-refused") ||
    (status === 403 && errorCode === "totp-required")
  ) {
    return { kind: "start-over" };
  }
  if (status === 429) {
    const onCode =
      request === "enrol-finish" || request === "recover-verify" || request === "recover-totp";
    if (onCode && errorCode === "too-many-attempts") return { kind: "used-up" };
    return { kind: "rate-limited", retryAfterSecs: answer.retryAfterSecs };
  }
  if (status === 502) return { kind: "send-failed" };
  if (status === 503 && errorCode !== "rate-limiter-unavailable") return { kind: "unavailable" };
  return { kind: "error" };
}

/** The catalog key of a failure's line, or `null` for the ones that are not
 *  a line (the countdown, the proof block, a step change). */
export function emailFailureKey(failure: EmailFailure): string | null {
  switch (failure.kind) {
    case "address-invalid":
      return "recovery_email.invalid";
    case "code-wrong":
      return "recovery_email.code.wrong";
    case "code-expired":
      return "recovery_email.code.expired";
    case "used-up":
      return "recovery_email.code.used_up";
    case "send-failed":
      return "recovery_email.send_failed";
    case "unavailable":
      return "recovery_email.unavailable";
    case "username-required":
      return "recovery_email.needs_username";
    case "offline":
      return "common.error.offline";
    case "start-over":
    case "error":
      return "recovery.error.unavailable";
    case "rate-limited":
      // A limit that names no wait has no countdown to show.
      return failure.retryAfterSecs === null ? "common.error.too_many_attempts" : null;
    case "proof-required":
    // The app's own dialog says it (`protect.last_factor`), not a line.
    case "last-factor":
      return null;
  }
}

// ---------------------------------------------------------------------------
// Codes and the "Send again" clock
// ---------------------------------------------------------------------------

/** The six digits of a typed code, or `null` (spaces and dashes dropped). */
export function normalizeEmailCode(typed: string): string | null {
  const digits = typed.replace(/[\s-]/g, "");
  return /^[0-9]{6}$/.test(digits) ? digits : null;
}

/** A code has this many tries (the server's rule; counted here as well, so
 *  the person is sent back for a new code even where the server's own
 *  answer for the spent code is the plain refusal). */
export const CODE_TRIES = 5;

/** Milliseconds until "Send again" is offered. */
export function resendRemainingMs(resendAtMs: number, nowMs: number): number {
  return Math.max(0, resendAtMs - nowMs);
}

// ---------------------------------------------------------------------------
// Enrol
// ---------------------------------------------------------------------------

export type EnrolState =
  | { readonly step: "address"; readonly working: boolean; readonly failure: EmailFailure | null }
  | {
      readonly step: "code";
      readonly working: boolean;
      readonly failure: EmailFailure | null;
      readonly challengeId: string;
      readonly resendAtMs: number;
      /** Wrong codes typed for this code so far. */
      readonly wrong: number;
    }
  | { readonly step: "done" };

export type EnrolEvent =
  | { readonly type: "submitted" }
  | {
      readonly type: "sent";
      readonly challengeId: string;
      readonly resendAfterSecs: number;
      readonly nowMs: number;
    }
  | { readonly type: "failed"; readonly failure: EmailFailure }
  | { readonly type: "other-address" }
  | { readonly type: "finished" };

export const ENROL_START: EnrolState = { step: "address", working: false, failure: null };

export function enrolReduce(state: EnrolState, event: EnrolEvent): EnrolState {
  if (state.step === "done") return state;
  switch (event.type) {
    case "submitted":
      return { ...state, working: true, failure: null };
    case "sent":
      return {
        step: "code",
        working: false,
        failure: null,
        challengeId: event.challengeId,
        resendAtMs: event.nowMs + event.resendAfterSecs * 1000,
        wrong: 0,
      };
    case "other-address":
      return ENROL_START;
    case "finished":
      return { step: "done" };
    case "failed": {
      const failure = event.failure;
      // The code's tries are spent: a new code is needed, from the address
      // step, which keeps the line saying why.
      if (state.step === "code" && failure.kind === "used-up") {
        return { step: "address", working: false, failure };
      }
      if (state.step === "code" && failure.kind === "code-wrong") {
        const wrong = state.wrong + 1;
        if (wrong >= CODE_TRIES) return { step: "address", working: false, failure: { kind: "used-up" } };
        return { ...state, working: false, failure, wrong };
      }
      return { ...state, working: false, failure };
    }
  }
}

// ---------------------------------------------------------------------------
// Recover
// ---------------------------------------------------------------------------

export type RecoverState =
  /** Step 1: username and address. */
  | { readonly step: "address"; readonly working: boolean; readonly failure: EmailFailure | null }
  /** Step 2: the code. The same whatever was typed in step 1. */
  | {
      readonly step: "code";
      readonly working: boolean;
      readonly failure: EmailFailure | null;
      readonly challengeId: string;
      readonly resendAtMs: number;
      /** Wrong codes typed for this code so far. */
      readonly wrong: number;
    }
  /** Step 3: the authenticator code, only when the account has one. */
  | {
      readonly step: "totp";
      readonly working: boolean;
      readonly failure: EmailFailure | null;
      readonly ticket: string;
    }
  /** Step 4: the new password, saved under the ticket. */
  | {
      readonly step: "password";
      readonly working: boolean;
      readonly failure: EmailFailure | null;
      readonly ticket: string;
    }
  /** Mail is not set up on this server. */
  | { readonly step: "unavailable" }
  /** The session landed. */
  | { readonly step: "landed" };

export type RecoverEvent =
  | { readonly type: "submitted" }
  /** `recover/start` answered. It answers the same for a matching pair, a
   *  wrong pair, an unknown username and an account with no email, and the
   *  event carries nothing that could tell them apart. */
  | {
      readonly type: "started";
      readonly challengeId: string;
      readonly resendAfterSecs: number;
      readonly nowMs: number;
    }
  | { readonly type: "verified"; readonly ticket: string; readonly totpRequired: boolean }
  | { readonly type: "totp-passed" }
  | { readonly type: "landed" }
  | { readonly type: "failed"; readonly failure: EmailFailure }
  | { readonly type: "back" };

export const RECOVER_START: RecoverState = { step: "address", working: false, failure: null };

export function recoverReduce(state: RecoverState, event: RecoverEvent): RecoverState {
  if (state.step === "landed" || state.step === "unavailable") return state;
  switch (event.type) {
    case "submitted":
      return { ...state, working: true, failure: null };
    case "started":
      return {
        step: "code",
        working: false,
        failure: null,
        challengeId: event.challengeId,
        resendAtMs: event.nowMs + event.resendAfterSecs * 1000,
        wrong: 0,
      };
    case "verified":
      if (state.step !== "code") return state;
      return event.totpRequired
        ? { step: "totp", working: false, failure: null, ticket: event.ticket }
        : { step: "password", working: false, failure: null, ticket: event.ticket };
    case "totp-passed":
      if (state.step !== "totp") return state;
      return { step: "password", working: false, failure: null, ticket: state.ticket };
    case "landed":
      return { step: "landed" };
    case "back":
      return RECOVER_START;
    case "failed": {
      const failure = event.failure;
      if (failure.kind === "unavailable") return { step: "unavailable" };
      // A spent code, or a ticket that is gone: a new code is needed, from
      // step 1, which keeps the line saying why.
      if (failure.kind === "used-up" || failure.kind === "start-over") {
        return { step: "address", working: false, failure };
      }
      if (state.step === "code" && failure.kind === "code-wrong") {
        const wrong = state.wrong + 1;
        if (wrong >= CODE_TRIES) return { step: "address", working: false, failure: { kind: "used-up" } };
        return { ...state, working: false, failure, wrong };
      }
      return { ...state, working: false, failure };
    }
  }
}
