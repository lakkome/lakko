// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Storing the account's keys for one passkey (docs/ux/identity-recovery.md
 * 10.2, amended 2026-11-11; ADR-0072 decision 6, amended).
 *
 * There is no choice any more: every passkey restores keys wherever the
 * platform gives a PRF output. This runs right after a passkey is created
 * (Settings > Security > Passkeys and the passkey offer at the end of
 * signup), and from a passkey row's "Restore keys with this passkey", the
 * repair for a passkey that can carry keys but holds none.
 *
 * It needs two things:
 *
 *   1. The recovery phrase, checked against the identity this session
 *      holds: the one typed by the person when they were asked for it, else
 *      the one stored on this device. A stored phrase that is not this
 *      account's (replaced on another device) counts as absent, so the
 *      person is asked for it instead.
 *   2. The passkey's PRF output: from the registration when the browser gave
 *      it there, else one more passkey prompt for this credential only. That
 *      output never leaves this device.
 *
 * The phrase is read before the second passkey prompt, so a device without
 * it never shows a prompt that cannot lead anywhere.
 */
import { AuthClient, AuthHttpError, AuthNetworkError } from "./client";
import { base64UrlDecodeNoPad, base64UrlEncodeNoPad } from "./account-id";
import { readDevicePhrase } from "../backup/device-phrase";
import { enrollPasskeyVault, PASSKEY_PRF_SALT } from "../recovery/passkey-vault";
import { loadWasm } from "../wasm";
import {
  WebauthnHttpError,
  WebauthnNetworkError,
  WebauthnUserCancelledError,
  type WebauthnClient,
} from "./webauthn-client";

/** How storing the keys for a passkey ended. Every outcome but `stored`
 *  leaves the passkey "Logs in only". */
export type StoreKeysOutcome =
  /** The keys are stored for the passkey. */
  | "stored"
  /** The passkey gave no PRF output: it cannot carry keys here. */
  | "no-prf"
  /** This device holds no recovery phrase of this account and none was
   *  typed: ask for it (the twelve-slot input), then run again. */
  | "phrase-needed"
  /** The typed phrase is a valid phrase but not this account's. */
  | "phrase-mismatch"
  /** The typed text is not a recovery phrase. */
  | "invalid-phrase"
  /** The person dismissed the passkey prompt. */
  | "cancelled"
  /** Another passkey than this one answered the prompt. */
  | "other-passkey"
  /** The server does not store keys for passkeys (`503`). */
  | "unavailable"
  /** No connection. */
  | "offline"
  /** Anything else; logged. */
  | "failed";

/** What storing the keys reaches for, so the decision can be tested
 *  without a browser, a passkey or a server. */
export interface StoreKeysDeps {
  /** The phrase this device stores for the account, or `null`. */
  readonly readDevicePhrase: () => Promise<string | null>;
  /** The 64-byte seed of `phrase`; throws when it is not a phrase. */
  readonly phraseSeed: (phrase: string) => Uint8Array;
  /** The identity seed this session holds. */
  readonly accountSeed: Uint8Array;
  /** One passkey prompt for `credentialIdB64` with the PRF salt: which
   *  credential answered, and its PRF output when it gave one. */
  readonly evaluatePrf: (
    credentialIdB64: string,
  ) => Promise<{ readonly credentialIdB64: string; readonly prfOutput: Uint8Array | null }>;
  /** Seal `phrase` under `prfOutput` and store it for the credential;
   *  `false` when the server has no vault key. */
  readonly enroll: (prfOutput: Uint8Array, phrase: string) => Promise<boolean>;
}

export async function storeKeysForPasskeyWith(
  deps: StoreKeysDeps,
  args: {
    readonly credentialIdB64: string;
    /** What the registration yielded, when it yielded it. */
    readonly prfOutput: Uint8Array | null;
    /** The phrase the person typed after `phrase-needed`. */
    readonly typedPhrase?: string;
  },
): Promise<StoreKeysOutcome> {
  try {
    // 1. The phrase, checked against this account.
    let phrase: string;
    if (args.typedPhrase !== undefined) {
      const typed = args.typedPhrase.trim();
      const check = phraseMatches(deps, typed);
      if (check === "invalid") return "invalid-phrase";
      if (check === "other") return "phrase-mismatch";
      phrase = typed;
    } else {
      const stored = await deps.readDevicePhrase();
      if (stored === null || phraseMatches(deps, stored) !== "match") return "phrase-needed";
      phrase = stored;
    }
    // 2. The passkey's PRF output: from the registration, else one more
    //    prompt for this credential only.
    let prfOutput = args.prfOutput;
    if (prfOutput === null) {
      const answered = await deps.evaluatePrf(args.credentialIdB64);
      if (answered.credentialIdB64 !== args.credentialIdB64) return "other-passkey";
      prfOutput = answered.prfOutput;
    }
    if (prfOutput === null) return "no-prf";
    // 3. Seal and store.
    return (await deps.enroll(prfOutput, phrase)) ? "stored" : "unavailable";
  } catch (err) {
    if (err instanceof WebauthnUserCancelledError) return "cancelled";
    if (
      (err instanceof AuthHttpError && err.status === 503) ||
      (err instanceof WebauthnHttpError && err.status === 503)
    ) {
      return "unavailable";
    }
    if (err instanceof AuthNetworkError || err instanceof WebauthnNetworkError) return "offline";
    // eslint-disable-next-line no-console
    console.warn("[passkeys] storing keys for a passkey failed", err);
    return "failed";
  }
}

/** Whether `phrase` is this account's phrase. */
function phraseMatches(deps: StoreKeysDeps, phrase: string): "match" | "other" | "invalid" {
  let seed: Uint8Array;
  try {
    seed = deps.phraseSeed(phrase);
  } catch {
    return "invalid";
  }
  const same = sameBytes(seed, deps.accountSeed);
  seed.fill(0);
  return same ? "match" : "other";
}

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i += 1) diff |= (a[i] ?? 0) ^ (b[i] ?? 0);
  return diff === 0;
}

/**
 * Store the keys for a passkey with the real phrase store, passkey and
 * server.
 */
export async function storeKeysForPasskey(args: {
  readonly client: WebauthnClient;
  readonly accountIdB64: string;
  readonly accountSeed: Uint8Array;
  readonly sessionToken: string;
  readonly credentialIdB64: string;
  readonly prfOutput: Uint8Array | null;
  readonly typedPhrase?: string;
}): Promise<StoreKeysOutcome> {
  let wasm: Awaited<ReturnType<typeof loadWasm>>;
  try {
    wasm = await loadWasm();
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[passkeys] could not load the core", err);
    return "failed";
  }
  const deps: StoreKeysDeps = {
    readDevicePhrase: () => readDevicePhrase(args.accountIdB64),
    phraseSeed: (phrase) => {
      const mnemonic = wasm.RecoveryMnemonic.fromPhrase(phrase);
      try {
        return mnemonic.toSeed();
      } finally {
        mnemonic.free();
      }
    },
    accountSeed: args.accountSeed,
    evaluatePrf: async (credentialIdB64) => {
      const extras = await args.client.evaluatePrfFor(credentialIdB64, PASSKEY_PRF_SALT);
      return {
        credentialIdB64: base64UrlEncodeNoPad(extras.credentialId),
        prfOutput: extras.prfOutput,
      };
    },
    enroll: (prfOutput, phrase) =>
      enrollPasskeyVault({
        wasm,
        client: new AuthClient(),
        sessionToken: args.sessionToken,
        credentialId: base64UrlDecodeNoPad(args.credentialIdB64),
        prfOutput,
        phrase,
      }),
  };
  return storeKeysForPasskeyWith(deps, {
    credentialIdB64: args.credentialIdB64,
    prfOutput: args.prfOutput,
    ...(args.typedPhrase !== undefined ? { typedPhrase: args.typedPhrase } : {}),
  });
}

/** The result line after a passkey was just added and its keys stored
 *  (UX spec 10.2 steps 2-4 and the states while storing). `named` means
 *  the line takes the passkey's name as `{name}`. */
export function addedPasskeyMessage(
  outcome: StoreKeysOutcome,
): { readonly kind: "ok" | "error"; readonly key: string; readonly named?: true } {
  switch (outcome) {
    case "stored":
      return { kind: "ok", key: "settings.passkeys.added_restores" };
    case "no-prf":
      return { kind: "ok", key: "settings.passkeys.added_no_keys" };
    // No phrase on this device: the passkey is added "Logs in only" and its
    // row offers the repair, which asks for the phrase.
    case "phrase-needed":
    case "phrase-mismatch":
    case "invalid-phrase":
      return { kind: "ok", key: "settings.passkeys.added", named: true };
    case "cancelled":
      return { kind: "ok", key: "settings.passkeys.keys_choice.cancelled" };
    case "other-passkey":
      return { kind: "error", key: "settings.passkeys.keys_choice.error.other_passkey" };
    case "unavailable":
      return { kind: "error", key: "settings.passkeys.keys_choice.unavailable" };
    // The passkey exists; only storing its keys failed. Its row offers the
    // repair.
    case "offline":
    case "failed":
      return { kind: "error", key: "settings.passkeys.added_keys_failed" };
  }
}

/** The result line of "Restore keys with this passkey" on a row; `null`
 *  for an outcome that shows nothing (a dismissed prompt) or that opens the
 *  phrase input instead (`phrase-needed`). */
export function repairMessage(
  outcome: StoreKeysOutcome,
): { readonly kind: "ok" | "error"; readonly key: string } | null {
  switch (outcome) {
    case "stored":
      return { kind: "ok", key: "settings.passkeys.keys_choice.done" };
    case "no-prf":
      return { kind: "error", key: "settings.passkeys.keys_choice.no_prf" };
    case "phrase-needed":
    case "cancelled":
      return null;
    case "phrase-mismatch":
      return { kind: "error", key: "settings.passkeys.keys_choice.error.phrase_mismatch" };
    case "invalid-phrase":
      return { kind: "error", key: "restore_identity.error.invalid_phrase" };
    case "other-passkey":
      return { kind: "error", key: "settings.passkeys.keys.error.other_passkey" };
    case "unavailable":
      return { kind: "error", key: "settings.passkeys.keys_choice.unavailable" };
    case "offline":
      return { kind: "error", key: "settings.passkeys.error.network" };
    case "failed":
      return { kind: "error", key: "settings.passkeys.error.generic" };
  }
}
