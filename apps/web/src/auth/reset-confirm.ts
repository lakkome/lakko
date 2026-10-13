// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The typed confirmation in front of "Reset account keys"
 * (docs/ux/identity-recovery.md section 15.1, ADR-0073).
 *
 * The word is the same Latin letters in every language (like the slot
 * numbers of the phrase grid), and the line above the field names it:
 * "Type RESET to confirm". The button is disabled until the typed text is
 * exactly that word; surrounding whitespace is forgiven (a trailing space
 * from a mobile keyboard), nothing else is (a lower-case `reset` is not the
 * word the line asked for).
 */

/** The word the person types. Not translated. */
export const RESET_CONFIRM_WORD = "RESET";

/** Whether `typed` confirms the reset. */
export function resetConfirmed(typed: string): boolean {
  return typed.trim() === RESET_CONFIRM_WORD;
}

/** Why the reset did not start, for the inline line under the button
 *  (section 15.1: a failure before anything changed leaves the panel open). */
export type ResetStartFailure =
  /** The shared `429` line with its countdown. */
  | "rate-limited"
  /** "Could not reset. Check your connection and try again." */
  | "offline"
  /** "Could not reset. Try again later." */
  | "other";

export function resetStartFailure(input: {
  readonly rateLimitedSecs: number | null;
  readonly offline: boolean;
}): ResetStartFailure {
  if (input.rateLimitedSecs !== null) return "rate-limited";
  return input.offline ? "offline" : "other";
}
