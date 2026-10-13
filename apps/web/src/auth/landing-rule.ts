// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * No publish-back (ADR-0073 section 4.2): what a session landing does once
 * it has read the identity the directory publishes for the account, BEFORE
 * it uploads anything.
 *
 * Until ADR-0073 a landing that found a different key published raised a
 * dismissible banner and then re-published its own bundle over it: the
 * "heal", which is exactly how a device signed out by a reset would put the
 * old identity back. Discovery now refuses an unattested key change
 * outright, and the client never tries: a landing compares what is
 * published with what it holds and either lands, or shows the signed-out
 * screen (`docs/ux/identity-recovery.md` section 15.2), or publishes
 * because nothing is published yet.
 *
 * The generation is what tells a known reset from an anomaly: a published
 * key of a HIGHER generation than this device's own token is the account's
 * new identity, and the device wipes by itself; a different key at the SAME
 * generation cannot happen through a reset (every reset bumps it), so the
 * device blocks and lets the person decide, never uploading either way.
 */

/** What the directory served, as the landing reads it. */
export interface PublishedIdentityView {
  /** `signature_pubkey` equals the key this device derives from its seed. */
  readonly keyMatches: boolean;
  /** The `key_generation` the published key belongs to. */
  readonly keyGeneration: bigint;
}

export type LandingVerdict =
  /** The published key is this device's: land, upload where the landing
   *  uploads today. */
  | "matches"
  /** Nothing is published yet (404): land and publish, as a first sign-in
   *  always did. */
  | "not-published"
  /** The directory could not be read: land on local data, upload nothing,
   *  check again on the next landing. */
  | "unknown"
  /** The account's keys were reset on another device (a higher generation
   *  is published): the signed-out screen and the automatic wipe. */
  | "superseded"
  /** A different key at the same (or a lower) generation: the blocking
   *  screen with the wipe on the person's tap. Never upload. */
  | "mismatch";

/**
 * The rule. `published` is `null` for a 404 and `undefined` when the fetch
 * failed; `ownKeyGeneration` is the generation of the device's session pair.
 */
export function landingVerdict(
  published: PublishedIdentityView | null | undefined,
  ownKeyGeneration: bigint,
): LandingVerdict {
  if (published === undefined) return "unknown";
  if (published === null) return "not-published";
  if (published.keyMatches) return "matches";
  return published.keyGeneration > ownKeyGeneration ? "superseded" : "mismatch";
}

/** Whether a verdict allows the landing's uploads (the bundle re-publish,
 *  the KeyPackage). Only `matches` and `not-published` do. */
export function landingMayUpload(verdict: LandingVerdict): boolean {
  return verdict === "matches" || verdict === "not-published";
}

/** Whether a verdict ends the session on this device. */
export function landingSignsOut(verdict: LandingVerdict): boolean {
  return verdict === "superseded" || verdict === "mismatch";
}
