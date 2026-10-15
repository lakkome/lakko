// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A new recovery phrase written to its account's row BEFORE the step that
 * makes it the account's only way back, and taken out again when that step
 * does not happen (ADR-0077 decision 3, rule 1; G-47). The web twin of the
 * Android `StagedPhrase`.
 *
 * The step is a key reset on the server (`resetAccountKeys`, the recall's
 * `recallFinish`): from its answer on, the new phrase is the account's and the
 * old one is not. Written AFTER the answer, a page that closes in between, a
 * failed write or a session that changed underneath leaves a reset account
 * whose phrase exists nowhere. Written before, the worst case is a row that
 * names a phrase the server never learned of, and {@link StagedPhrase.rollback}
 * removes exactly that.
 *
 * The row is kept as it is on disk (ciphertext, never opened) so a row the
 * current device key cannot open is put back intact.
 */

/** One account's row of the phrase store, as staging needs it
 *  (production: `devicePhraseRow` in `device-phrase.ts`). */
export interface PhraseRow<Snapshot> {
  /** The row as it is on disk, `null` when absent. Throws when the store
   *  cannot be read. */
  readSnapshot(): Promise<Snapshot | null>;
  /** The phrase the row holds in the clear, `null` when absent or it does
   *  not open. Never throws. */
  readPhrase(): Promise<Uint8Array | null>;
  /** Write the phrase. `false` when the store gate refused it (the account
   *  was wiped since the work began). Throws when the write fails. */
  write(phrase: Uint8Array): Promise<boolean>;
  /** Put a {@link readSnapshot} back. `false` when refused. Throws on
   *  failure. */
  restore(snapshot: Snapshot): Promise<boolean>;
  /** Remove the row. Throws on failure. */
  remove(): Promise<void>;
}

/** The phrase could not be written to its row: nothing irreversible has
 *  happened, and the caller refuses the step. */
export class PhraseNotStagedError extends Error {
  override name = "PhraseNotStagedError";
  constructor(readonly reason: "refused" | "failed") {
    super(`the new recovery phrase could not be stored (${reason})`);
  }
}

/** What {@link StagedPhrase.rollback} did. */
export type PhraseRollback =
  /** The row held this phrase and is back as {@link StagedPhrase.stage}
   *  found it. */
  | "restored"
  /** The row no longer holds this phrase (a wipe removed it, a later write
   *  replaced it): somebody else's decision, left as it is. */
  | "not-held"
  /** The row could not be put back. */
  | "failed";

export class StagedPhrase<Snapshot> {
  private readonly phraseBytes: Uint8Array;
  private previous: Snapshot | null = null;
  private staged = false;

  constructor(
    private readonly row: PhraseRow<Snapshot>,
    phrase: string,
  ) {
    this.phraseBytes = new TextEncoder().encode(phrase);
  }

  /**
   * Remember what the row holds and write the phrase over it.
   *
   * @throws PhraseNotStagedError when the row cannot be read or written, or
   *   the store gate refused the write.
   */
  async stage(): Promise<void> {
    let written: boolean;
    try {
      this.previous = await this.row.readSnapshot();
      written = await this.row.write(this.phraseBytes);
    } catch {
      throw new PhraseNotStagedError("failed");
    }
    if (!written) throw new PhraseNotStagedError("refused");
    this.staged = true;
  }

  /** Whether the row holds this phrase now. Never throws. */
  async isHeld(): Promise<boolean> {
    if (!this.staged) return false;
    let held: Uint8Array | null;
    try {
      held = await this.row.readPhrase();
    } catch {
      return false;
    }
    if (held === null) return false;
    const same = bytesEqual(held, this.phraseBytes);
    held.fill(0);
    return same;
  }

  /**
   * The step did not happen: put the row back as {@link stage} found it,
   * only while it still holds THIS phrase. Never throws.
   */
  async rollback(): Promise<PhraseRollback> {
    if (!(await this.isHeld())) return "not-held";
    try {
      const before = this.previous;
      if (before === null) {
        await this.row.remove();
      } else if (!(await this.row.restore(before))) {
        // Refused: the account was wiped since, and the wipe removes the row.
        return "not-held";
      }
      this.staged = false;
      return "restored";
    } catch {
      return "failed";
    }
  }

  /** Wipe the copies this object holds. The row is not touched. */
  close(): void {
    this.phraseBytes.fill(0);
    this.previous = null;
  }
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= (a[i] ?? 0) ^ (b[i] ?? 0);
  return diff === 0;
}
