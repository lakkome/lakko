// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The best name for a chat (ADR-0048 §5), shared by the chat view and the
 * app-level call host (which names the caller on a ringing banner shown over
 * whatever screen the user is on).
 *
 * A 1x1 chat is the other person, through the one display resolver (local
 * rename, profile name, verified username, fingerprint), read from the live
 * MLS roster so it works however the chat was created. A group keeps its label;
 * a linked contact's rename still names an accept-created chat.
 */

import type { Group as WasmGroup } from "../../wasm/lakko_ffi_wasm";
import { personKeysOf } from "./person-key";

export interface PeerNameInputs {
  /** This account's signature public key, base64, or `null` when unknown. */
  readonly ownKeyB64: string | null;
  /** Every roster member's signature public key, base64. */
  readonly memberKeysB64: readonly string[];
  /** The ADR-0048 display resolver. */
  readonly resolveName: (signatureKeyB64: string) => string;
  /** A linked contact's user-chosen name for this chat, if any. */
  readonly linkedContactName: string | undefined;
  /** The chat's label. */
  readonly label: string;
}

/** Pure half of the rule. */
export function peerName(inputs: PeerNameInputs): string {
  const peers = inputs.memberKeysB64.filter((k) => k !== inputs.ownKeyB64);
  if (inputs.memberKeysB64.length === 2 && peers.length === 1 && peers[0] !== undefined) {
    return inputs.resolveName(peers[0]);
  }
  if (inputs.linkedContactName !== undefined && inputs.linkedContactName.length > 0) {
    return inputs.linkedContactName;
  }
  return inputs.label.length > 0 ? inputs.label : "Peer";
}

/** The roster's PEOPLE — distinct account keys (ADR-0050), so a 1x1 between
 *  two people with three devices each is still a 1x1. A torn group reads as
 *  empty (label fallback). */
export function memberKeysOf(group: WasmGroup): string[] {
  return personKeysOf(group);
}
