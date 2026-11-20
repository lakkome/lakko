// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074 section 3.7 (2026-11-20) - envelopes sealed to a member's account
 * after that member rotated its account keys.
 *
 * The roster names a rotated person by its CONVERSATION key (the oldest own
 * predecessor: every leaf binding is under it), while the directory serves
 * the CURRENT key and the current identity's X25519 key, plus the two-way
 * conversation link signed by both keys.
 *
 * Sender (the owner sealing a registrar seed, a root handover or a knock
 * seed): the envelope is ADDRESSED to the roster key and SEALED to the X25519
 * key of the bundle, and the bundle is accepted only by
 * {@link bundleServesRosterKey}. Receiver: the address is checked against the
 * conversation identity; the envelope opens with the conversation identity's
 * own X25519 key, else with the current identity's
 * ({@link openSealedToOwnAccount}).
 */

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= (a[i] ?? 0) ^ (b[i] ?? 0);
  return diff === 0;
}

/**
 * Whether a fetched identity bundle is the bundle of the person the MLS
 * roster names by `rosterKey`: its served key IS the roster key, or its
 * conversation link verifies against the served key and names exactly the
 * roster key as the conversation key. Anything else (no link, a link that
 * does not verify, a link naming another key, a malformed key) is refused.
 *
 * `verifyLink` is the core's `verifyConversationLink(link, trustedCurrentKey)`:
 * it answers the 32-byte conversation key and throws on anything wrong.
 */
export function bundleServesRosterKey(input: {
  readonly servedKey: Uint8Array;
  readonly conversationLink: Uint8Array | null;
  readonly rosterKey: Uint8Array;
  readonly verifyLink: (link: Uint8Array, trustedCurrentKey: Uint8Array) => Uint8Array;
}): boolean {
  const { servedKey, conversationLink, rosterKey, verifyLink } = input;
  if (servedKey.length !== 32 || rosterKey.length !== 32) return false;
  if (sameBytes(servedKey, rosterKey)) return true;
  if (conversationLink === null || conversationLink.length === 0) return false;
  let conversationKey: Uint8Array;
  try {
    conversationKey = verifyLink(conversationLink, servedKey);
  } catch {
    return false;
  }
  return sameBytes(conversationKey, rosterKey);
}

/**
 * Open an envelope addressed to this account's conversation identity: first
 * sealed to its own X25519 key (`openOwn`), then, when the account rotated
 * and the directory serves `current`, sealed to the current identity's X25519
 * key (`openSealedTo` with the conversation identity as the addressee).
 * `null` when neither opens.
 */
export function openSealedToOwnAccount<I>(input: {
  readonly conversation: I;
  readonly current: I | null;
  readonly openOwn: (addressee: I) => Uint8Array;
  readonly openSealedTo: (addressee: I, sealedTo: I) => Uint8Array;
}): Uint8Array | null {
  const { conversation, current, openOwn, openSealedTo } = input;
  try {
    return openOwn(conversation);
  } catch {
    // Not sealed to the conversation identity's X25519 key.
  }
  if (current === null || current === conversation) return null;
  try {
    return openSealedTo(conversation, current);
  } catch {
    return null;
  }
}
