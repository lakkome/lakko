// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The new phrase of a key reset, stored on this device BEFORE the server step
 * that makes it the account's (ADR-0077 decision 3 rule 1, G-47).
 *
 * Every web route that ends in a key reset with a new phrase stages through
 * here: "Reset account keys" (`mintFreshIdentity`, before `resetAccountKeys`)
 * and taking the account back (`runRecall`, before `recallFinish`). The order
 * is the Android `mintFreshIdentity` one:
 *
 * 1. the phrase goes into the account's row of the phrase store, wrapped
 *    under the device key; a row that cannot be written refuses the reset
 *    here, where nothing irreversible has happened
 *    (`restore_identity.lost.error.phrase_not_stored`);
 * 2. the server step;
 * 3. a stale check or a server step that did not happen puts the previous row
 *    back, only while the row still holds THIS phrase.
 *
 * A device with no device key for the account (a fresh browser that got
 * ACCESS without a password) gets one derived from the NEW seed first, as the
 * reset's landing would mint anyway: a phrase wrapped under a key that is
 * stored nowhere could never be read back. A rollback removes that key again.
 *
 * Every write is made under the route's {@link WipeMark}: a Forget device of
 * the account since the route began refuses it (and so the reset), a wipe
 * before it (the staged wipe a recall begins from) does not, and a plain
 * sign-out never does.
 */
import type { WipeMark } from "./account-store-gate";
import { deriveDeviceRootKeyFromIdentitySeed } from "./device-key";
import { deleteDeviceKey, getDeviceKey, putDeviceKey } from "./key-store";
import { devicePhraseRow } from "../backup/device-phrase";
import type { PhraseRowSnapshot } from "../backup/phrase-store";
import { StagedPhrase, type PhraseRollback } from "../backup/staged-phrase";
import { KeyedError } from "../l10n/keyed-error";

/** The catalog key of the refusal: nothing was changed. */
export const PHRASE_NOT_STORED_KEY = "restore_identity.lost.error.phrase_not_stored";

/** A staged reset phrase. */
export interface StagedResetPhrase {
  /** The device key the phrase is wrapped under; the reset's landing uses it
   *  so it does not mint another. */
  readonly deviceKey: CryptoKey;
  /** The server step did not happen (it failed, or the route was overtaken
   *  before it): put the row, and a device key minted here, back. Never
   *  throws. */
  rollback(): Promise<PhraseRollback>;
  /** The server step answered: the row is the account's phrase now. Wipes the
   *  copies this object holds; the row is not touched. */
  close(): void;
}

/**
 * Stage `phrase` for `accountIdB64`.
 *
 * @throws KeyedError({@link PHRASE_NOT_STORED_KEY}) when the device key or the
 *   phrase could not be stored, or the account was wiped since `wipes`.
 *   Nothing is left changed.
 */
export async function stageResetPhrase(args: {
  readonly accountIdB64: string;
  readonly phrase: string;
  /** The 64-byte seed of the NEW phrase (a device key is derived from it when
   *  the account has none here). Not consumed. */
  readonly seed: Uint8Array;
  /** The account's device key when the route holds one. */
  readonly deviceKey: CryptoKey | null;
  readonly wipes: WipeMark;
}): Promise<StagedResetPhrase> {
  const refuse = (why: string): KeyedError => {
    // eslint-disable-next-line no-console
    console.warn(`key reset refused: the new phrase could not be stored on this device (${why})`);
    return new KeyedError(PHRASE_NOT_STORED_KEY);
  };
  let deviceKey = args.deviceKey;
  let mintedKey = false;
  if (deviceKey === null) {
    let stored: CryptoKey | null;
    try {
      stored = await getDeviceKey(args.accountIdB64);
    } catch {
      throw refuse("device key not readable");
    }
    if (stored !== null) {
      // Another tab (or an earlier step) stored one: wrap under it, so
      // nothing it already wraps is orphaned.
      deviceKey = stored;
    } else {
      let written = false;
      try {
        deviceKey = await deriveDeviceRootKeyFromIdentitySeed(args.seed, args.accountIdB64);
        written = await putDeviceKey(args.accountIdB64, deviceKey, args.wipes);
      } catch {
        throw refuse("device key not stored");
      }
      if (!written) throw refuse("the account's local data was wiped");
      mintedKey = true;
    }
  }
  const key = deviceKey;
  const staged = new StagedPhrase<PhraseRowSnapshot>(
    devicePhraseRow(args.accountIdB64, key, args.wipes),
    args.phrase,
  );
  const dropMintedKey = async (): Promise<void> => {
    if (!mintedKey) return;
    try {
      await deleteDeviceKey(args.accountIdB64);
    } catch {
      // A key derived from a seed the server never learned wraps nothing:
      // the next landing that needs one stores its own over it.
    }
  };
  try {
    await staged.stage();
  } catch (err) {
    staged.close();
    await dropMintedKey();
    throw refuse(err instanceof Error ? err.message : "unknown");
  }
  return {
    deviceKey: key,
    async rollback() {
      const outcome = await staged.rollback();
      // The key goes only with the phrase it was minted for: a row someone
      // else rewrote (or a wipe removed) is left to them.
      if (outcome === "restored") await dropMintedKey();
      staged.close();
      return outcome;
    },
    close() {
      staged.close();
    },
  };
}
