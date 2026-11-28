// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Save a recovery phrase" (`docs/ux/identity-recovery.md` 19.5 as amended
 * 2026-11-26; ADR-0075, second amendment; ADR-0101 section 10). Pure.
 *
 * Saving a phrase changes no key. No copy of the phrase ever left the
 * device, so the screen shows the 12 words the device already holds, checks
 * three of them, and then the account's protection is published again with
 * the phrase in it (`auth.savePhraseSeal()`). Only that publish counts: a
 * person who leaves before it has saved nothing, and on its failure the
 * three-word screen stays with "We could not finish. Nothing changed." and
 * `Try again`.
 */

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

/** Where the panel is. */
export type SavePhraseStage =
  /** The explanation and `Show my 12 words` (with the 17.1 proof block
   *  when no proof is live). */
  | "intro"
  /** The words in the grid. */
  | "words"
  /** Three of them, typed back. */
  | "check";

/**
 * What the words to show are: the phrase this device holds when it opens
 * the account as it is now, else `null` (nothing can be saved here: the
 * panel says so and offers `Try again`; it never shows words that do not
 * open the account).
 */
export async function wordsToSave(ports: {
  readonly heldPhrase: () => Promise<string | null>;
  readonly opensAccount: (phrase: string) => Promise<boolean>;
}): Promise<string | null> {
  const phrase = await ports.heldPhrase();
  if (phrase === null) return null;
  return (await ports.opensAccount(phrase)) ? phrase : null;
}

/** What the publish after the three-word check came to. */
export type SavePhraseOutcome =
  /** The protection lists the phrase: the panel closes. */
  | "saved"
  /** Nothing changed: the three-word screen stays, with the failure line
   *  and `Try again`. */
  | "failed";

export function savePhraseOutcome(published: "published" | "retry"): SavePhraseOutcome {
  return published === "published" ? "saved" : "failed";
}
