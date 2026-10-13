// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which keys a contact request goes out to, as pure decisions.
 *
 * A request sealed to keys the caller already holds (an invite link's
 * capability, a search result's bundle) runs no identity fetch of its own.
 * For a STRANGER (no pin) that is right. For an account this device has
 * pinned, a key that is not the pin sends the published identity through
 * the fail-closed gate first (`ContactsClient.checkContactIdentity`), where
 * a proven rotation re-pins and an attested reset is accepted silently
 * (ADR-0073 R.2); a key that is then still neither the pin nor a proven
 * predecessor of it is refused. Old keys do not come back.
 */

/** The slice of a pin the gate reads. */
export interface PinForRequestGate {
  readonly pinnedSignaturePubkeyB64: string;
}

export type ContactRequestGateVerdict =
  /** Nothing stands in the way: no pin (a stranger), or the key is the pin. */
  | "send"
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
  return pin.pinnedSignaturePubkeyB64 === recipientKeyB64 ? "send" : "check-identity";
}

/** Which keys a contact request goes out to once the recipient's published
 *  identity has passed the fail-closed gate (ADR-0074 section 3.5a rule 1). */
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
