// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Lakko is full for now" (`docs/ux/beta-account-cap.md`): the decisions of
 * the sign-up screen and of the guest's after-call question, kept apart from
 * the components so they can be tested.
 *
 * The server says two things: `GET /signup/open` (a hint, asked before the
 * proof of work; a failed question reads as open, see
 * `AuthClient.signupOpen`), and `403 registration-closed` at the first
 * login-handle bind (final). Either one closed shows the full state.
 */
import { isRegistrationClosed, isSupporterCodeUsed } from "./login-handle";

/** What the sign-up screen shows. `closed.checking`: `Try again` is asking
 *  the server, its button reads `Checking...` and is disabled.
 *  `closed.used`: the line "This supporter code has already been used."
 *  shows above the full state (`docs/ux/supporters.md` 1.2). `code`: the
 *  supporter code entry (`docs/ux/supporters.md` 1.1) in place of the full
 *  state; `checking` while the code is checked, `error` the line under the
 *  field. */
export type SignupGate =
  | { readonly view: "form" }
  | { readonly view: "closed"; readonly checking: boolean; readonly used?: boolean }
  | {
      readonly view: "code";
      readonly checking: boolean;
      readonly error: "unknown" | "failed" | "limited" | null;
    };

export type SignupGateEvent =
  /** `GET /signup/open` answered (`true` also when the question failed). */
  | { readonly type: "answer"; readonly open: boolean }
  /** `Try again` was pressed on the full state. */
  | { readonly type: "retry" }
  /** The sign-up was refused at the bind (`403 registration-closed`). */
  | { readonly type: "refused" }
  /** The bind refused the held place: already spent (`403 supporter-code-used`). */
  | { readonly type: "used" }
  /** `I have a supporter code` on the full state. */
  | { readonly type: "code-open" }
  /** `Back` on the code entry. */
  | { readonly type: "code-back" }
  /** `Continue` on the code entry. */
  | { readonly type: "code-submit" }
  /** The code was checked. `held`: the ordinary form follows. */
  | { readonly type: "code-result"; readonly outcome: "held" | "unknown" | "failed" | "limited" };

export const SIGNUP_GATE_START: SignupGate = { view: "form" };

export function signupGateReduce(state: SignupGate, event: SignupGateEvent): SignupGate {
  switch (event.type) {
    case "answer":
      // The code entry is left only by its own buttons; a late answer does
      // not take it away.
      if (state.view === "code") return state;
      if (event.open) return { view: "form" };
      return state.view === "closed" && state.used === true
        ? { view: "closed", checking: false, used: true }
        : { view: "closed", checking: false };
    case "retry":
      // Only the full state has the button; a second press while asking is
      // ignored by the disabled button and here.
      return state.view === "closed" ? { ...state, checking: true } : state;
    case "refused":
      return { view: "closed", checking: false };
    case "used":
      return { view: "closed", checking: false, used: true };
    case "code-open":
      return state.view === "closed" && !state.checking
        ? { view: "code", checking: false, error: null }
        : state;
    case "code-back":
      return state.view === "code" && !state.checking ? { view: "closed", checking: false } : state;
    case "code-submit":
      return state.view === "code" && !state.checking
        ? { view: "code", checking: true, error: null }
        : state;
    case "code-result":
      if (state.view !== "code") return state;
      return event.outcome === "held"
        ? { view: "form" }
        : { view: "code", checking: false, error: event.outcome };
  }
}

/** Whether `Continue` on the code entry may run: the field holds a whole
 *  code and nothing is being checked. */
export function signupCodeMaySubmit(state: SignupGate, codeComplete: boolean): boolean {
  return state.view === "code" && !state.checking && codeComplete;
}

/** Whether a `Try again` press should ask the server. */
export function signupGateMayRetry(state: SignupGate): boolean {
  return state.view === "closed" && !state.checking;
}

/** How a failed sign-up is shown: the full state for the bind's refusal,
 *  the full state with the used line for a spent supporter place
 *  (`docs/ux/supporters.md` 1.2), the form's error line for anything else. */
export function signupFailureView(err: unknown): "closed" | "used" | "error" {
  if (isSupporterCodeUsed(err)) return "used";
  return isRegistrationClosed(err) ? "closed" : "error";
}

/** The guest's after-call question (`docs/ux/call-links.md` section 9, the
 *  cap spec section 3): the ordinary question, or the closed one whose only
 *  button does what `No, thanks` does. */
export type GuestQuestion = "ask" | "closed";

export function guestQuestionFor(args: {
  /** `GET /signup/open` said closed (a failed question is open). */
  readonly signupOpen: boolean;
  /** `Create account` was refused at the bind in this question's life. */
  readonly refusedAtBind: boolean;
  /** An earlier `Create account` already bound this username (the guest
   *  marker's `claimedUsername`, `docs/ux/call-links.md` 9.2). That account
   *  holds a place and its re-bind is never refused, so it is never shown
   *  the closed question. */
  readonly reservedUsername: string | null;
}): GuestQuestion {
  if (args.reservedUsername !== null && args.reservedUsername.length > 0) return "ask";
  return args.refusedAtBind || !args.signupOpen ? "closed" : "ask";
}
