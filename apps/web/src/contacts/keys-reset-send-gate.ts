// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 phase 2 (UX 16.1 "what is blocked") — the decision a contact
 * request runs before anything is sealed or sent.
 *
 * A contact request is sealed to the recipient's account keys. For a person
 * this device already holds a pin for, those keys must be the ones it
 * trusts: a contact whose keys were reset and not yet verified is refused
 * with `Verify {name}'s new safety number first.`, exactly like starting a
 * one-to-one or adding them to a group. A stranger (no pin: first-ever
 * trust on first use) is not a contact and is reached exactly as before.
 *
 * Pure: no wasm, no storage, no network. `ContactsClient` reads the pin and
 * acts on the verdict.
 */

/** The part of a stored pin the decision reads. */
export interface PinForRequestGate {
  readonly pinnedSignaturePubkeyB64: string;
  /** Present while the contact is `reset-unverified`. */
  readonly keyReset?: unknown;
}

export type ContactRequestGateVerdict =
  /** Nothing stands in the way: no pin (a stranger), or the key is the pin. */
  | "send"
  /** The contact is `reset-unverified`: refuse before anything is sent. */
  | "keys-reset"
  /** A pinned account is about to be reached under a key that is NOT the
   *  pin: the published identity must pass the fail-closed gate first. */
  | "check-identity";

/**
 * What to do with a contact request addressed to `recipientKeyB64`
 * (standard base64 of the Ed25519 account key the request would be sealed
 * for), given the pin this device holds for that account (if any).
 */
export function contactRequestGate(
  pin: PinForRequestGate | undefined,
  recipientKeyB64: string,
): ContactRequestGateVerdict {
  if (pin === undefined) return "send";
  if (pin.keyReset !== undefined) return "keys-reset";
  return pin.pinnedSignaturePubkeyB64 === recipientKeyB64 ? "send" : "check-identity";
}

/**
 * Whether a refusal of the supersession gate is the STATE "keys reset, not
 * verified" rather than the gate's own refusal: only when the transparency
 * log confirmed the change as a reset AND this device recorded it. A change
 * with an invalid proof, or one the log does not confirm, stays the gate's
 * refusal (a possible compromise is never presented as a reset).
 */
export function refusalIsRecordedReset(loggedReset: boolean | null, recorded: boolean): boolean {
  return loggedReset === true && recorded;
}

/** Which keys a contact request goes out to once the recipient's published
 *  identity has passed the fail-closed gate (ADR-0066 section 3.5a rule 1). */
export type RequestTarget =
  /** The keys in hand are the published ones. */
  | "as-supplied"
  /** The key in hand is a PROVEN predecessor of the published one: seal to
   *  the published keys and send to the current mailbox. Never to the old
   *  key. */
  | "current"
  /** The key in hand is neither: nothing is sent. Old keys do not come
   *  back, and an unproven other key is not this person. */
  | "refuse";

/** Pure. */
export function requestTargetAfterFetch(facts: {
  readonly suppliedKeyB64: string;
  readonly publishedKeyB64: string;
  /** Whether the published key is the proven successor of the supplied one
   *  (`proven-successor.ts`). */
  readonly publishedIsProvenSuccessor: boolean;
}): RequestTarget {
  if (facts.suppliedKeyB64 === facts.publishedKeyB64) return "as-supplied";
  return facts.publishedIsProvenSuccessor ? "current" : "refuse";
}
