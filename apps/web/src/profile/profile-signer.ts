// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 8 - where a profile received in a conversation goes.
 *
 * Profile v6 is signed by its author's account identity key. A profile in a
 * conversation is normally the sender's own: it is filed under the sender's
 * key (resolved at receipt, C-68) only when its signature verifies under
 * that key. After this account's key reset, a peer that returns a
 * conversation also posts there the newest profile it holds for THIS person,
 * signed by this account's previous key: that one is offered as the own
 * profile coming back (`returned-profile.ts`), never filed under the peer.
 * Anything else is dropped: a profile signed by someone who did not send it
 * names nobody.
 */

/** What to do with one received profile. */
export type ProfileRoute = "file" | "returned-own" | "drop";

/**
 * | verifies under the sender | verifies under this account's previous key | route        |
 * |---------------------------|--------------------------------------------|--------------|
 * | yes                       | -                                          | file         |
 * | no                        | yes                                        | returned-own |
 * | no                        | no                                         | drop         |
 */
export function routeReceivedProfile(facts: {
  readonly verifiesUnderSender: boolean;
  readonly verifiesUnderPreviousOwnKey: boolean;
}): ProfileRoute {
  if (facts.verifiesUnderSender) return "file";
  return facts.verifiesUnderPreviousOwnKey ? "returned-own" : "drop";
}

/** The core's verifier, structurally: throws when it does not verify. */
export type ProfileVerifier = (envelope: Uint8Array, identityPk: Uint8Array) => { free(): void };

/** Whether `envelope` verifies under `keyB64` (never throws). */
export function profileVerifiesUnder(
  verify: ProfileVerifier,
  envelope: Uint8Array,
  keyB64: string | null,
  decodeB64: (s: string) => Uint8Array,
): boolean {
  if (keyB64 === null) return false;
  try {
    verify(envelope, decodeB64(keyB64)).free();
    return true;
  } catch {
    return false;
  }
}
