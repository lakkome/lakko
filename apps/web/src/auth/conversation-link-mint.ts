// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The two-way conversation link a rotation uploads beside its supersession
 * proof (ADR-0074 section 3.7).
 *
 * Conversation leaves stay bound to the OLDEST own key (Stage A) while the
 * directory publishes the newest, so a contact first met after the rotation
 * pins a key its roster never names. The link is signed by both keys: it
 * lets that contact follow the person without trusting the directory.
 *
 * Signed on every attempt of the upload; the server stores the bytes opaque,
 * so a retried publish that signs again is the same statement. No expiry.
 */
import { base64Decode } from "./account-id";

/** The wasm surface this needs. */
export interface ConversationLinkWasm {
  readonly Identity: {
    fromSeed(seed: Uint8Array): {
      readonly ed25519Seed: Uint8Array;
      readonly signaturePublicKey: Uint8Array;
      free(): void;
    };
  };
  signConversationLink(
    conversationEd25519Seed: Uint8Array,
    currentEd25519Seed: Uint8Array,
    issuedAtUnixSecs: bigint,
  ): Uint8Array;
}

function sameKey(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((byte, i) => byte === b[i]);
}

/**
 * The link from the account's conversation key (the OLDEST of
 * `predecessorSeedsB64`) to `currentSeed`, or `null` when this device holds
 * no predecessor other than the current key: there is nothing to link.
 * Neither seed outlives the call. Seeds are never logged.
 */
export function mintConversationLink(
  wasm: ConversationLinkWasm,
  /** The 64-byte account seeds this device holds, oldest first. */
  predecessorSeedsB64: readonly string[],
  /** The 64-byte seed of the key the directory publishes. */
  currentSeed: Uint8Array,
  nowSecs: number,
): Uint8Array | null {
  const oldestB64 = predecessorSeedsB64[0];
  if (oldestB64 === undefined) return null;
  const oldestSeed = base64Decode(oldestB64);
  const conversation = wasm.Identity.fromSeed(oldestSeed);
  const current = wasm.Identity.fromSeed(currentSeed);
  let conversationSigning: Uint8Array | null = null;
  let currentSigning: Uint8Array | null = null;
  try {
    if (sameKey(conversation.signaturePublicKey, current.signaturePublicKey)) return null;
    conversationSigning = conversation.ed25519Seed;
    currentSigning = current.ed25519Seed;
    return wasm.signConversationLink(conversationSigning, currentSigning, BigInt(nowSecs));
  } finally {
    conversationSigning?.fill(0);
    currentSigning?.fill(0);
    oldestSeed.fill(0);
    conversation.free();
    current.free();
  }
}
