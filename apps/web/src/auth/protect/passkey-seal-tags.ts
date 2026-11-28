// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which passkeys keep the chats (`docs/ux/identity-recovery.md` 19.1d): a
 * passkey's row says "Does not keep your chats." when the account's
 * published protection does not list it. The protection names a passkey by
 * a 16-byte tag made from its credential id (the core's
 * `resetSealPasskeyTag`); this module turns credential ids into those tags
 * and decides the row's line.
 */
import { passkeyKeepsChats, type OwnSealFacts } from "../seal/own-seals";

/** The part of the wasm core this needs. */
export interface SealTagCore {
  resetSealPasskeyTag(credentialId: Uint8Array): Uint8Array;
}

export function hexOf(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function base64UrlBytes(slug: string): Uint8Array | null {
  try {
    const padded = slug.replace(/-/g, "+").replace(/_/g, "/");
    const raw = atob(padded + "=".repeat((4 - (padded.length % 4)) % 4));
    return Uint8Array.from(raw, (c) => c.charCodeAt(0));
  } catch {
    return null;
  }
}

/** The tag (lower-case hex) of each credential id (URL-safe base64, no
 *  padding). An id that does not decode has no tag. */
export function sealTagsOf(
  core: SealTagCore,
  credentialIdsB64: readonly string[],
): ReadonlyMap<string, string> {
  const out = new Map<string, string>();
  for (const id of credentialIdsB64) {
    const bytes = base64UrlBytes(id);
    if (bytes === null) continue;
    try {
      out.set(id, hexOf(core.resetSealPasskeyTag(bytes)));
    } catch {
      // No tag: the row says nothing about the chats.
    }
  }
  return out;
}

/**
 * Whether a passkey's row carries `settings.passkeys.no_seal`: the facts
 * are known, the passkey's tag is known, and the protection does not list
 * it. While either is unknown the row says nothing.
 */
export function passkeyRowSaysNoSeal(facts: OwnSealFacts | null, tagHex: string | undefined): boolean {
  if (tagHex === undefined) return false;
  return passkeyKeepsChats(facts, tagHex) === false;
}
