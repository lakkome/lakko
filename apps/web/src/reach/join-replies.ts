// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0058 section 6 - the `/g/` joiner's `reply` grant, kept by an admin
 * until the sweep that admits the joiner writes their Welcome with it.
 *
 * A join request's self-signed attribution carries the joiner's reply grant;
 * the committer - which may be any admin that read the attribution - writes
 * the sweep's Welcome to the joiner's account mailbox under it (C-69). Keyed
 * by the joiner's LEAF signature key (hex), the key the attribution is
 * self-signed with and the key the sweep's swept Add names.
 *
 * In memory: a request that outlives the session is re-read from the knock
 * queue, attribution included, on the next open.
 */
import { journalHex } from "../ds/access";

const replies = new Map<string, string>();

/** Keep a joiner's reply grant (the 37-byte wire, base64). */
export function rememberJoinReply(joinerSignatureKey: Uint8Array, grantWireB64: string): void {
  replies.set(journalHex(joinerSignatureKey), grantWireB64);
}

/** The kept reply grant of a joiner, or `null`. */
export function joinReplyFor(joinerSignatureKey: Uint8Array): string | null {
  return replies.get(journalHex(joinerSignatureKey)) ?? null;
}

/** The joiner was admitted (or refused): forget their grant. */
export function forgetJoinReply(joinerSignatureKey: Uint8Array): void {
  replies.delete(journalHex(joinerSignatureKey));
}

/** Sign-out. */
export function clearJoinReplies(): void {
  replies.clear();
}
