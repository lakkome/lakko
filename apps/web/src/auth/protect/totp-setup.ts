// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Setting up an authenticator app (`docs/ux/identity-recovery.md` 19.1b;
 * ADR-0073, amended 2026-11-26): the setup as a pure step machine, the
 * reading of the server's answers, and the key's display form. One screen,
 * used by the "Protect your account" step and by Settings > Security >
 * Recovery.
 *
 * The setup needs the 17.1 proof (`403 fresh-auth-required` without one):
 * the authenticator is one of the key reset's second confirmations, so a
 * bare session must not be able to add one.
 */

/** Why a step of the setup did not go through. */
export type TotpSetupFailure =
  /** `401` on the code: wrong, or from an expired step. */
  | { readonly kind: "wrong" }
  /** `429`: too many tries; the countdown. */
  | { readonly kind: "rate-limited"; readonly retryAfterSecs: number | null }
  /** `403 fresh-auth-required`: the proof block. */
  | { readonly kind: "proof-required" }
  /** `409`: a confirmed authenticator already exists. */
  | { readonly kind: "already" }
  /** No answer. */
  | { readonly kind: "offline" }
  /** Anything else (`400`, `503`, a server error). */
  | { readonly kind: "other" };

/** Read one answer. `status` is `null` when none arrived. */
export function totpSetupFailure(
  request: "start" | "finish",
  answer: { readonly status: number | null; readonly errorCode: string | null; readonly retryAfterSecs: number | null },
): TotpSetupFailure {
  const { status, errorCode } = answer;
  if (status === null) return { kind: "offline" };
  if (status === 403 && errorCode === "fresh-auth-required") return { kind: "proof-required" };
  if (status === 409) return { kind: "already" };
  if (status === 429) return { kind: "rate-limited", retryAfterSecs: answer.retryAfterSecs };
  if (status === 401 && request === "finish") return { kind: "wrong" };
  return { kind: "other" };
}

export type TotpSetupState =
  /** Not started: no proof is live yet (the 17.1 block is drawn), or the
   *  start is about to run. */
  | { readonly step: "proof"; readonly working: boolean; readonly failure: TotpSetupFailure | null }
  /** The key is being made: the QR area shows a spinner. */
  | { readonly step: "starting" }
  /** The QR code and the key are on screen. */
  | {
      readonly step: "ready";
      readonly otpauthUri: string;
      readonly secretB32: string;
      /** The typed code is being checked: the field is disabled. */
      readonly checking: boolean;
      /** The line under the code field (`wrong`, `offline`, a limit). */
      readonly failure: TotpSetupFailure | null;
    }
  /** Any other failure: the generic line with `Try again`, which starts
   *  over with a new key. */
  | { readonly step: "failed" }
  /** A confirmed authenticator already exists. */
  | { readonly step: "already" }
  | { readonly step: "done" };

export type TotpSetupEvent =
  | { readonly type: "start" }
  | { readonly type: "started"; readonly otpauthUri: string; readonly secretB32: string }
  | { readonly type: "check" }
  | { readonly type: "confirmed" }
  | { readonly type: "failed"; readonly failure: TotpSetupFailure }
  /** `Try again` after the generic failure. */
  | { readonly type: "again" };

export const TOTP_SETUP_START: TotpSetupState = { step: "proof", working: false, failure: null };

export function totpSetupReduce(state: TotpSetupState, event: TotpSetupEvent): TotpSetupState {
  if (state.step === "done" || state.step === "already") return state;
  switch (event.type) {
    case "start":
      return state.step === "ready" ? state : { step: "starting" };
    case "started":
      return {
        step: "ready",
        otpauthUri: event.otpauthUri,
        secretB32: event.secretB32,
        checking: false,
        failure: null,
      };
    case "check":
      return state.step === "ready" ? { ...state, checking: true, failure: null } : state;
    case "confirmed":
      return state.step === "ready" ? { step: "done" } : state;
    case "again":
      return TOTP_SETUP_START;
    case "failed": {
      const failure = event.failure;
      if (failure.kind === "already") return { step: "already" };
      if (state.step === "ready") {
        // The QR and the key stay for a wrong code, a limit, no connection
        // and a proof that ran out (the block is drawn above them).
        if (failure.kind === "other") return { step: "failed" };
        return { ...state, checking: false, failure };
      }
      // Before the key exists: the proof block, or its own line.
      if (failure.kind === "other" || failure.kind === "wrong") return { step: "failed" };
      return { step: "proof", working: false, failure };
    }
  }
}

/** The catalog key of a failure's line, or `null` for the ones that are not
 *  a line (the countdown, the proof block). */
export function totpSetupFailureKey(failure: TotpSetupFailure | null): string | null {
  if (failure === null) return null;
  switch (failure.kind) {
    case "wrong":
      return "settings.recovery.totp.error.wrong";
    case "offline":
      return "common.error.offline";
    case "already":
      return "settings.recovery.totp.already_enabled";
    case "other":
      return "settings.recovery.totp.error.generic";
    case "rate-limited":
      // A limit that names no wait has no countdown to show.
      return failure.retryAfterSecs === null ? "common.error.too_many_attempts" : null;
    case "proof-required":
      return null;
  }
}

/** The six digits of a typed code once all six are there, else `null`
 *  (spaces and dashes dropped). The code is checked on the sixth digit,
 *  with no separate button. */
export function completeTotpCode(typed: string): string | null {
  const digits = typed.replace(/[\s-]/g, "");
  return /^[0-9]{6}$/.test(digits) ? digits : null;
}

/** The key as the screen shows it: the same secret in groups of four. */
export function groupedTotpKey(secretB32: string): string {
  const compact = secretB32.replace(/\s+/g, "").toUpperCase();
  return compact.replace(/(.{4})(?=.)/g, "$1 ");
}
