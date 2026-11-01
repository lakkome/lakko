// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074 section 3.7 - what a fetched identity bundle's two-way
 * conversation link does.
 *
 * Conversation leaves stay bound to a rotated account's OLDEST key while the
 * directory publishes the newest, so a contact first met after the rotation
 * pins a key the roster never names, and holds no supersession proof that
 * would help: that proof is signed by the OLD key, which this contact never
 * pinned. The link is signed by BOTH keys, so it verifies against the key
 * the contact did pin.
 *
 * Run only AFTER the fail-closed gate passed for the bundle, and only when
 * the fetched key is what the gate left trusted: the pin, else this device's
 * verified binding. A link that does not verify is ignored and changes
 * nothing; it never refuses the identity.
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import { readPin, recordConversationKey } from "./local-store";
import {
  readVerifiedBinding,
  recordBindingConversationKey,
  type VerifiedBinding,
} from "./verified-binding";

/**
 * Record the conversation key `linkB64` proves for `accountIdB64`. `verify`
 * is the core's `verifyConversationLink`: it answers the 32-byte
 * conversation key and throws on anything wrong. Returns whether anything
 * was recorded.
 */
export function recordConversationLink(input: {
  readonly accountIdB64: string;
  /** The key the bundle published, standard base64. */
  readonly fetchedKeyB64: string;
  /** The bundle's `conversation_link`, standard base64. */
  readonly linkB64: string | undefined;
  readonly verify: (link: Uint8Array, trustedCurrentKey: Uint8Array) => Uint8Array;
  readonly owner?: string;
}): boolean {
  const { accountIdB64, fetchedKeyB64, linkB64, verify, owner } = input;
  if (linkB64 === undefined || linkB64.length === 0) return false;
  const pin = readPin(accountIdB64, owner);
  const binding = pin === undefined ? readVerifiedBinding(accountIdB64, owner) : undefined;
  const trustedKeyB64 = pin?.pinnedSignaturePubkeyB64 ?? binding?.keyB64;
  if (trustedKeyB64 !== fetchedKeyB64) return false;
  let conversationKeyB64: string;
  try {
    conversationKeyB64 = base64Encode(verify(base64Decode(linkB64), base64Decode(fetchedKeyB64)));
  } catch {
    // eslint-disable-next-line no-console
    console.info("a conversation link did not verify; ignored");
    return false;
  }
  return pin !== undefined
    ? recordConversationKey(accountIdB64, fetchedKeyB64, conversationKeyB64, owner)
    : recordBindingConversationKey(accountIdB64, fetchedKeyB64, conversationKeyB64, owner);
}

/** A binding becomes a pin: the conversation key it holds moves with it. Call
 *  it after the pin is written under the binding's key and before the binding
 *  is dropped. */
export function carryBindingConversationKey(
  accountIdB64: string,
  binding: VerifiedBinding | undefined,
  owner?: string,
): void {
  if (binding?.conversationKeyB64 === undefined) return;
  recordConversationKey(accountIdB64, binding.keyB64, binding.conversationKeyB64, owner);
}
