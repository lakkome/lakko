// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Save a recovery phrase" (ADR-0075 section 3 as amended 2026-11-01, UX
 * 19.5). There are two ways, and the server says which:
 *
 *   - the words this device already holds: the phrase was never in an email
 *     vault, so no copy of it needs outrunning. The same grid, the same check,
 *     and `POST /account/phrase/saved`. No key changes. Nothing counts as
 *     saved before that call has its answer.
 *   - a key rotation: the phrase was in a vault, or this device does not hold
 *     it. The words are shown only when the change has gone through. Until
 *     the commit, a failure changed nothing ("We could not finish. Nothing
 *     changed." with Try again). After it, the keys ARE changed and there is
 *     no way back: the remaining publishing steps finish by themselves and
 *     the words appear when they have.
 *
 * Which way is never guessed: a failed read shows the failure and Try again.
 * Pure, with the reads as ports.
 */
import { AuthHttpError } from "../client";
import type { RotationOutcome } from "../../devices/key-rotation";
import type { PhraseStatus } from "./recovery-email-client";

/** How this account saves its phrase on this device. */
export type SavePath =
  /** Show `phrase`, check three words, tell the server. */
  | { readonly kind: "words"; readonly phrase: string }
  /** Today's rotation. */
  | { readonly kind: "rotation" };

/**
 * Decide the path. `heldPhrase` is the phrase this device holds, or `null`
 * when it holds none that opens this account; it is not read when the server
 * already said a rotation is needed. A failed status read throws.
 */
export async function readSavePath(ports: {
  readonly status: () => Promise<PhraseStatus>;
  readonly heldPhrase: () => Promise<string | null>;
}): Promise<SavePath> {
  const status = await ports.status();
  if (status.rotationNeeded) return { kind: "rotation" };
  const phrase = await ports.heldPhrase();
  return phrase === null ? { kind: "rotation" } : { kind: "words", phrase };
}

/** The wasm surface {@link phraseOpensAccount} needs. */
export interface PhraseWasm {
  readonly RecoveryMnemonic: {
    fromPhrase(phrase: string): { toSeed(): Uint8Array; free(): void };
  };
}

/**
 * Whether `phrase` derives `accountSeed`: the words a device shows must be
 * the ones that open the account now. A phrase that does not parse does not.
 */
export function phraseOpensAccount(wasm: PhraseWasm, phrase: string, accountSeed: Uint8Array): boolean {
  try {
    const mnemonic = wasm.RecoveryMnemonic.fromPhrase(phrase);
    try {
      const seed = mnemonic.toSeed();
      const same = seed.length === accountSeed.length && seed.every((b, i) => b === accountSeed[i]);
      seed.fill(0);
      return same;
    } finally {
      mnemonic.free();
    }
  } catch {
    return false;
  }
}

/** `POST /account/phrase/saved` told this much. */
export type PhraseSavedVerdict =
  /** The account is phrase-protected. */
  | "saved"
  /** `409`: the phrase was in a vault after all. The rotation takes over. */
  | "needs-rotation"
  /** A refusal the proof block or the countdown words. */
  | "proof"
  /** No answer, or a server failure: it may have landed. Try again. */
  | "unknown"
  /** A refusal: nothing was saved. Try again. */
  | "refused";

export const CODE_NEEDS_ROTATION = "phrase-save-needs-rotation";

/** Read the error `POST /account/phrase/saved` threw (`null`: it answered). */
export function phraseSavedVerdict(err: unknown): PhraseSavedVerdict {
  if (err === null) return "saved";
  if (!(err instanceof AuthHttpError)) return "unknown";
  if (err.status === 409 && err.errorCode === CODE_NEEDS_ROTATION) return "needs-rotation";
  if (err.status === 403 || err.status === 429) return "proof";
  return err.status >= 500 ? "unknown" : "refused";
}

/** Whether no notice can have been written: this device's mark for it would
 *  hide someone else's. */
export function phraseSavedLeftNoNotice(verdict: PhraseSavedVerdict): boolean {
  return verdict === "needs-rotation" || verdict === "proof" || verdict === "refused";
}

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
