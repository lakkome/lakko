// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a refused recovery sign-in says about the phrase stored on this
 * device.
 *
 * The three-word route ends in a recovery login made with the seed of the
 * phrase this device holds. When the account's phrase was replaced on
 * another device (a fresh identity, ADR-0017), that login is refused as
 * invalid credentials. The route would then be offered forever and always
 * fail, so it is SUSPENDED on this device (`three-words.ts`,
 * `markThreeWordsSuspended`) and its budget cleared.
 *
 * Nothing is deleted. A `401` is not proof that the phrase is stale (an
 * unbound login handle answers the same way), and a deleted phrase cannot
 * be brought back on this device. The phrase store, the identity seed, MLS
 * state and everything else stay; Settings keeps reporting the phrase as
 * saved. Whether the device's identity is stale is decided by the
 * published-identity checks, not by a failed sign-in.
 *
 * Since ADR-0065 a reset moves the recovery credential and the key
 * generation in one server transaction, so a phrase this device minted is
 * the account's credential the moment the reset answered: there is no
 * "credential owed" exception any more (what can still be owed afterwards
 * is the publish, `key-reset-owed.ts`, which does not touch the sign-in).
 */
import { AuthHttpError, AuthNetworkError } from "../client";

/** Why the recovery login made with the stored seed did not yield a session. */
export type RecoveryLoginFailure =
  /** `401`, or the OPAQUE exchange could not be finished on this side
   *  (`AuthClient.recoveryLogin` reports both as `401`). */
  | "invalid-credentials"
  /** `429`: a limit, says nothing about the credential. */
  | "rate-limited"
  /** `503`: the server cannot answer, says nothing about the credential. */
  | "unavailable"
  /** The request never got an answer. */
  | "network"
  /** Anything else (another status, a local error). */
  | "other";

export function recoveryLoginFailureOf(err: unknown): RecoveryLoginFailure {
  if (err instanceof AuthNetworkError) return "network";
  if (err instanceof AuthHttpError) {
    if (err.status === 401) return "invalid-credentials";
    if (err.status === 429) return "rate-limited";
    if (err.status === 503) return "unavailable";
  }
  return "other";
}

export type StoredPhraseVerdict =
  /** Set the per-account "three words suspended" marker and clear the
   *  three-word budget. The phrase store and everything else are untouched. */
  | "suspend-three-words"
  /** The failure says nothing about the phrase: keep everything. */
  | "keep";

/**
 * The rule. Only an invalid-credentials refusal suspends three words.
 */
export function storedPhraseVerdict(input: {
  readonly failure: RecoveryLoginFailure;
}): StoredPhraseVerdict {
  return input.failure === "invalid-credentials" ? "suspend-three-words" : "keep";
}

/** The sign-in made with the stored seed was refused as invalid
 *  credentials; three words are now suspended on this device. */
export class ThreeWordsSuspendedError extends Error {
  override name = "ThreeWordsSuspendedError";
  constructor() {
    super("the recovery phrase stored on this device did not sign in; three words are suspended");
  }
}
