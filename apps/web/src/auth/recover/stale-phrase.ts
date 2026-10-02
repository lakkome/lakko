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
 * One exception: this device itself minted a fresh identity and still owes
 * the server the new recovery credential (`recovery-credential-owed.ts`).
 * The stored phrase is then the account's real phrase and only the server
 * lags, so nothing is suspended and the pending state is shown.
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
  /** A recovery-credential replace is owed on this device: keep everything
   *  and show the pending explanation. */
  | "credential-pending"
  /** The failure says nothing about the phrase: keep everything. */
  | "keep";

/**
 * The rule. Only an invalid-credentials refusal suspends three words, and
 * only when no replace is owed here.
 */
export function storedPhraseVerdict(input: {
  readonly failure: RecoveryLoginFailure;
  /** Whether this device owes the server a recovery-credential replace for
   *  the account. */
  readonly replaceOwed: boolean;
}): StoredPhraseVerdict {
  if (input.failure !== "invalid-credentials") return "keep";
  return input.replaceOwed ? "credential-pending" : "suspend-three-words";
}

/** The sign-in made with the stored seed was refused as invalid
 *  credentials; three words are now suspended on this device. */
export class ThreeWordsSuspendedError extends Error {
  override name = "ThreeWordsSuspendedError";
  constructor() {
    super("the recovery phrase stored on this device did not sign in; three words are suspended");
  }
}

/** The sign-in was refused because this device still owes the server the
 *  new recovery credential; nothing was removed. */
export class RecoveryCredentialPendingError extends Error {
  override name = "RecoveryCredentialPendingError";
  constructor() {
    super("the recovery credential for the new phrase is not registered yet");
  }
}
