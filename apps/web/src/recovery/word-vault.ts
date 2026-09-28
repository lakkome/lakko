// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Three-word identity recovery (ADR-0049) — the client orchestration over
 * the wasm `wordVaultEnroll` / `wordVaultAnswer` core and the `/vault/*`
 * endpoints.
 *
 * Nothing here interprets the phrase: enrollment seals the phrase entropy
 * under three words, recovery opens it back into the full 24-word phrase and
 * hands that to the SAME paths a typed phrase takes (`provideRecoveryPhrase`
 * on a fresh device, `recover` for a forgotten password), so the fail-closed
 * check against the published identity runs on this route too.
 *
 * Argon2id at the enrolled cost runs on the calling thread inside wasm and
 * takes on the order of a second; callers show a busy state.
 */
import type { AuthClient } from "../auth/client";
import { AuthHttpError, AuthNetworkError } from "../auth/client";
import { authLimitMessage } from "../auth/auth-error-copy";
import { base64Decode, base64Encode } from "../auth/account-id";
import { getDeviceKey } from "../auth/key-store";
import { getPhrase } from "../backup/phrase-store";
import { isBip39Word } from "./bip39-words";
import type { Wasm } from "../wasm";

/** How many words the user answers with. */
export const ANSWER_WORDS = 3;

/** What `/vault/recover/start` served: which words to ask for, and how to
 *  stretch them. */
export interface WordVaultChallenge {
  readonly positions: readonly number[];
  readonly salt: Uint8Array;
  readonly argon2MKib: number;
  readonly argon2T: number;
  readonly argon2P: number;
}

/** Enroll `phrase` into the word vault for the signed-in account. Returns the
 *  three positions the user will be asked for. */
export async function enrollWordVault(args: {
  readonly wasm: Wasm;
  readonly client: AuthClient;
  readonly sessionToken: string;
  readonly phrase: string;
}): Promise<number[]> {
  const mnemonic = args.wasm.RecoveryMnemonic.fromPhrase(args.phrase);
  try {
    const enrollment = args.wasm.wordVaultEnroll(mnemonic);
    try {
      const positions = Array.from(enrollment.positions);
      await args.client.wordVaultEnroll(args.sessionToken, {
        positions,
        salt_b64: base64Encode(enrollment.salt),
        argon2_m_kib: enrollment.argon2MKib,
        argon2_t: enrollment.argon2T,
        argon2_p: enrollment.argon2P,
        verifier_b64: base64Encode(enrollment.verifier),
        sealed_entropy_b64: base64Encode(enrollment.sealedEntropy),
      });
      return positions;
    } finally {
      enrollment.free();
    }
  } finally {
    mnemonic.free();
  }
}

/**
 * Make sure the signed-in account has a vault, when this device can build
 * one. With `phrase` given it enrolls outright; otherwise it asks the server
 * and, if nothing is enrolled, uses the phrase this device cached under its
 * device key (the backup bootstrap caches it at signup / recover). A device
 * holding only the derived seed cannot enroll: the seed does not reverse to
 * the words.
 *
 * Best-effort: every failure is logged and swallowed, since this runs behind
 * a sign-in that has already succeeded. Returns the positions when a vault
 * exists afterwards, `null` when it could not be established.
 */
export async function ensureWordVaultEnrolled(args: {
  readonly wasm: Wasm;
  readonly client: AuthClient;
  readonly accountIdB64: string;
  readonly sessionToken: string;
  readonly phrase?: string;
}): Promise<number[] | null> {
  try {
    if (args.phrase !== undefined) {
      return await enrollWordVault({
        wasm: args.wasm,
        client: args.client,
        sessionToken: args.sessionToken,
        phrase: args.phrase,
      });
    }
    const status = await args.client.wordVaultStatus(args.sessionToken);
    if (status.enrolled && status.positions !== null) {
      return status.positions;
    }
    const deviceKey = await getDeviceKey(args.accountIdB64);
    if (deviceKey === null) return null;
    const cached = await getPhrase(args.accountIdB64, deviceKey);
    if (cached === null) return null;
    const phrase = new TextDecoder().decode(cached.phrase);
    return await enrollWordVault({
      wasm: args.wasm,
      client: args.client,
      sessionToken: args.sessionToken,
      phrase,
    });
  } catch (err) {
    if (err instanceof AuthHttpError && err.status === 503) {
      // The server has no vault key configured; nothing to do on this stack.
      return null;
    }
    // eslint-disable-next-line no-console
    console.warn("word-vault enrollment skipped", err);
    return null;
  }
}

/** Ask the server which words to type and how to stretch them. `handle` is
 *  the account's login handle (ADR-0054 S5, `auth/login-handle.ts`). */
export async function startWordVault(
  client: AuthClient,
  handle: string,
): Promise<WordVaultChallenge> {
  const resp = await client.wordVaultRecoverStart(handle);
  return {
    positions: resp.positions,
    salt: base64Decode(resp.salt_b64),
    argon2MKib: resp.argon2_m_kib,
    argon2T: resp.argon2_t,
    argon2P: resp.argon2_p,
  };
}

/** Local validation the UI runs before spending a server-side guess: three
 *  words, each in the list. */
export function wordsAreAnswerable(words: readonly string[]): boolean {
  return (
    words.length === ANSWER_WORDS &&
    words.every((w) => w.trim().length > 0 && isBip39Word(w.trim().toLowerCase()))
  );
}

/**
 * Stretch the typed words, submit the guess, and open what comes back into
 * the full 24-word phrase. Throws an `AuthHttpError` (401 wrong / 429 locked
 * / 503 disabled) or a `WordVaultOpenError` when the server accepted the
 * guess but the bytes did not open (which should not happen; it would mean a
 * tampered blob).
 */
export async function openWordVault(args: {
  readonly wasm: Wasm;
  readonly client: AuthClient;
  /** The account's login handle (ADR-0054 S5). */
  readonly handle: string;
  readonly challenge: WordVaultChallenge;
  readonly words: readonly string[];
}): Promise<string> {
  const answer = args.wasm.wordVaultAnswer(
    args.words.map((w) => w.trim().toLowerCase()),
    args.challenge.salt,
    args.challenge.argon2MKib,
    args.challenge.argon2T,
    args.challenge.argon2P,
  );
  try {
    const sealed = await args.client.wordVaultRecoverFinish(args.handle, answer.verifier);
    let mnemonic;
    try {
      mnemonic = answer.open(sealed);
    } catch (err) {
      throw new WordVaultOpenError(err);
    }
    try {
      return mnemonic.phrase;
    } finally {
      mnemonic.free();
    }
  } finally {
    answer.free();
  }
}

/** The server released the sealed entropy but it did not open. */
export class WordVaultOpenError extends Error {
  constructor(cause: unknown) {
    super("The server answered, but the recovery data did not open. Nothing was changed.");
    this.name = "WordVaultOpenError";
    this.cause = cause;
  }
}

/** Plain copy for the recovery screens. `triesLeft` is the client's own
 *  count (the server never reports one, so an unknown account cannot be
 *  told apart from a real one). */
export function wordVaultErrorMessage(err: unknown, triesLeft: number | null): string {
  const limit = authLimitMessage(err);
  if (limit !== null) return limit;
  if (err instanceof AuthHttpError) {
    if (err.status === 401) {
      return triesLeft === null || triesLeft <= 0
        ? "Those words did not match."
        : `Those words did not match. ${triesLeft} ${triesLeft === 1 ? "try" : "tries"} left before the three-word shortcut is switched off.`;
    }
    if (err.status === 503) {
      return "Three-word recovery is not available on this server.";
    }
  }
  if (err instanceof AuthNetworkError) {
    return "Could not reach the server. Check your connection.";
  }
  return err instanceof Error ? err.message : String(err);
}

/** The server destroys the vault after this many consecutive misses. */
export const WORD_VAULT_DESTRUCTION_CAP = 10;
