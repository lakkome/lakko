// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Save a recovery phrase" (ADR-0067 section 3, UX 19.5): what one run of
 * the key rotation that saves a phrase means for the surface. Pure.
 *
 * The words are shown only when the change has gone through. Until the
 * commit, a failure changed nothing ("We could not finish. Nothing
 * changed." with Try again). After it, the keys ARE changed and there is no
 * way back: the remaining publishing steps finish by themselves and the
 * words appear when they have.
 */
import type { RotationOutcome } from "../../devices/key-rotation";

export type SavePhraseView =
  /** Every step has its answer: the new words are up next. */
  | { readonly kind: "done" }
  /** The keys were changed; the publishing steps are finishing by
   *  themselves. A progress line, no button. */
  | { readonly kind: "finishing" }
  /** The proof is needed (again): the block of 17.1, no error. */
  | { readonly kind: "proof" }
  /** A limit: the countdown, or its plain line. */
  | { readonly kind: "rate-limited" }
  /** Nothing changed: the line and Try again. */
  | { readonly kind: "failed" };

/** `null` outcome: the run could not start (no keys in memory, or a key
 *  reset's publish is owed). */
export function savePhraseView(outcome: RotationOutcome | null): SavePhraseView {
  if (outcome === null) return { kind: "failed" };
  switch (outcome.kind) {
    case "complete":
      return { kind: "done" };
    case "publishing":
      return { kind: "finishing" };
    case "not-removed":
      // No device is removed by this rotation; read as a failure.
      return { kind: "failed" };
    case "not-started":
      if (outcome.reason === "proof-required") return { kind: "proof" };
      if (outcome.reason === "rate-limited") return { kind: "rate-limited" };
      return { kind: "failed" };
  }
}

/** Whether the account is phrase-protected from this outcome on: the
 *  commit carried `phrase_saved` and was answered. */
export function phraseSavedBy(outcome: RotationOutcome | null): boolean {
  return outcome !== null && (outcome.kind === "complete" || outcome.kind === "publishing");
}
