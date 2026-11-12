// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The one "ask again" rule for a request this device sent and nobody answered
 * (`docs/ux/group-and-channel-privacy.md` sections 6.2 and 15.2.2, amended
 * 2026-11-12: a day, it was 7 days). Under a day the request reads "Waiting
 * for an admin." (or the page's "Request sent." line) with no control; a day
 * or more, "No answer yet." with `Ask again`, which sends the same request
 * again and restarts the day. A device asks once per group (or per channel,
 * for a publisher request) per day.
 *
 * Used by the `Requests sent` list, the `/g/` invite page and the publisher
 * invitation card. Pure: no store, no network, no Solid.
 */

/** A request may be asked again this long after it was last sent. */
export const ASK_AGAIN_AFTER_MS = 24 * 60 * 60 * 1000;

/** What a sent, unanswered request shows. */
export type AskAgainState = "waiting" | "no-answer";

/** The state of a request last sent at `sentAtMs`. */
export function askAgainState(sentAtMs: number, nowMs: number): AskAgainState {
  return nowMs - sentAtMs >= ASK_AGAIN_AFTER_MS ? "no-answer" : "waiting";
}

/** The part of a recorded group request `Ask again` needs. */
export interface ResendableRequest {
  /** The group id, base64. */
  readonly idB64: string;
  /** The group's knock public key, base64, and its version; empty when the
   *  request came from a link that carried none. */
  readonly knockKeyB64: string;
  readonly knockVersion: number;
  /** A request from a `/g/` link: the plaintext capability the link opened
   *  to and its ledger id, base64. Absent for a request from search. */
  readonly capabilityWireB64?: string | undefined;
  readonly blobIdB64?: string | undefined;
}

/** The two ways a group request goes out (`ContactsClient`). */
export interface ResendClient<I> {
  requestToJoinSealed(
    identity: I,
    groupId: Uint8Array,
    knockPublicKey: Uint8Array,
    knockVersion: number,
    capabilityWire?: Uint8Array,
  ): Promise<void>;
  requestToJoinFromLink(
    identity: I,
    capabilityWire: Uint8Array,
    nowMs: number,
    opts?: { blobId?: Uint8Array | null | undefined; joinerAccountId?: Uint8Array | undefined },
  ): Promise<unknown>;
}

function decodeB64(b64: string): Uint8Array {
  const binary = atob(b64);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}

/**
 * `Ask again`: send the recorded request once more, the same way the first
 * one went. A link request goes through the link again (its expiry is checked
 * and the ledger consume, idempotent per joiner, is repeated); a request from
 * search is knocked sealed to the group's key. The caller records the request
 * again with the new time once this resolves; a throw leaves the record as it
 * was.
 */
export async function sendRequestAgain<I>(
  client: ResendClient<I>,
  identity: I,
  r: ResendableRequest,
  nowMs: number,
  joinerAccountId?: Uint8Array,
): Promise<void> {
  if (r.capabilityWireB64 !== undefined && r.capabilityWireB64.length > 0) {
    await client.requestToJoinFromLink(identity, decodeB64(r.capabilityWireB64), nowMs, {
      blobId: r.blobIdB64 !== undefined && r.blobIdB64.length > 0 ? decodeB64(r.blobIdB64) : null,
      joinerAccountId,
    });
    return;
  }
  await client.requestToJoinSealed(identity, decodeB64(r.idB64), decodeB64(r.knockKeyB64), r.knockVersion);
}
