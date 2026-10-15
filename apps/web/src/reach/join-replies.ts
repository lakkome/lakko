// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 6 - the `/g/` joiner's `reply` grant, kept by an admin
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

/** The part of a wasm `ReachGrant` the binding check needs. */
export interface BindingCheckable {
  mailboxPkVerified(ownerAccountPk: Uint8Array): Uint8Array;
}

/**
 * ADR-0078 - whether a join attribution's `reply` grant names the joiner's
 * own mailbox: its binding verifies against the joiner's ACCOUNT key (for a
 * chat group, the account the proposal's leaf is bound to; for a channel
 * co-publisher, the leaf key itself). A grant that fails is not kept: a
 * member could otherwise hand out its own mailbox under the joiner's name and
 * swallow the joiner's Welcome. `null` account key (unresolvable): `false`.
 * Never throws; logs without identifiers.
 */
export function joinReplyBindingVerifies(
  grant: BindingCheckable,
  joinerAccountKey: Uint8Array | null | undefined,
): boolean {
  if (joinerAccountKey === null || joinerAccountKey === undefined) return false;
  try {
    grant.mailboxPkVerified(joinerAccountKey);
    return true;
  } catch {
    // eslint-disable-next-line no-console
    console.warn("ADR-0078: a join reply grant was not kept; its mailbox binding did not verify");
    return false;
  }
}

const replies = new Map<string, string>();

/** Keep a joiner's reply grant (the 134-byte wire, base64; ADR-0078). */
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
