// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Who authored a message — the pure resolution rule behind the
 * ADR-0023 author-only edit/delete gate, the ADR-0023 reaction
 * attribution and the ADR-0024 read watermark.
 *
 * ## Why a signature key and not a leaf index
 *
 * An MLS leaf index is a **position in the ratchet tree, not a
 * person**. OpenMLS hands a vacated leaf to the next member added,
 * so `remove Bob, add Dave` — which ADR-0045 composes into ONE
 * commit, landing in a single poll batch — leaves Dave holding
 * Bob's index. Every rule that compares raw leaf indices then reads
 * Dave as Bob: Dave may edit and delete Bob's history, toggle off
 * Bob's reactions, and inherit Bob's read watermark.
 *
 * The Ed25519 **signature public key** is the stable identity: it
 * names the member, it survives a renumber, and it is what every
 * rule written after ADR-0023 already keys on for exactly this
 * reason — the role table (`group_roles.rs`), the committer rank,
 * the beacon observation store, the push-routing authority gate,
 * the membership-shrink diff. ADR-0023 predates that convention;
 * this module brings it in line.
 *
 * ## Why resolution happens at receipt
 *
 * A leaf only names a member **in the epoch the message was sealed
 * at**. `processIncomingVerifyingRoles` returning an Application
 * frame is the one moment the group's roster IS that epoch's roster
 * (a Commit that renumbers leaves is a separate journal row, and
 * OpenMLS decrypts an Application message only at the current
 * epoch). So the leaf is resolved to a key THERE, in the receive
 * loop, and the KEY is what travels onward and gets persisted —
 * never the leaf, resolved later against whatever roster happens to
 * be current then. That "resolve now, carry the key" shape is the
 * one `push/group-routing.ts` and the ADR-0045 beacon store already
 * use.
 */

import { base64Encode } from "../auth/account-id";
import { personKeyOf } from "./person-key";

/**
 * A member's stable identity: their Ed25519 signature public key,
 * base64-standard — the same encoding the role table, the roster
 * views, the accepted-contact map and the observation stores use,
 * so one member has one string everywhere in the client.
 */
export type MemberIdentity = string;

/**
 * The shape `Group.members()` hands back (the fields this module
 * reads). Declared structurally so the pure rule stays testable
 * with no wasm.
 */
export interface RosterMember {
  readonly leafIndex: number;
  readonly signaturePublicKey: Uint8Array;
  /** ADR-0050 — the account the leaf belongs to; the identity an author
   *  resolves to, so one person is one author across their devices. */
  readonly accountKey?: Uint8Array;
}

/**
 * Sentinel identity for an own reaction whose signature key the
 * client could not resolve (a torn roster read). It still toggles
 * and still counts, exactly as the old numeric sentinel did.
 *
 * `?` is outside the base64 alphabet, so this can never collide
 * with a real key — the property the old `-1` had against a real
 * leaf index.
 */
export const OWN_REACTION_SENTINEL: MemberIdentity = "?own";

/**
 * Resolve one MLS leaf index to the member's signature key, at the
 * epoch the caller is holding.
 *
 * `undefined` on a miss — an unknown leaf, or a roster the caller
 * could not read. Fail-closed is the whole contract: every consumer
 * treats an unresolved identity the way it treats an unknown author
 * today, which is to refuse.
 */
export function resolveMemberIdentity(
  roster: readonly RosterMember[],
  leafIndex: number,
): MemberIdentity | undefined {
  const hit = roster.find((m) => m.leafIndex === leafIndex);
  return hit === undefined ? undefined : base64Encode(personKeyOf(hit));
}

/**
 * Resolve one MLS leaf index to that leaf's OWN signature key (the device
 * key, ADR-0050), not the person key {@link resolveMemberIdentity} returns.
 *
 * For state that is per device rather than per person: the group-call roster
 * (`calls/group-call-record.ts`), where each device joins and leaves on its
 * own. Same rule otherwise — resolve at receipt, `undefined` on a miss.
 */
export function resolveLeafSignatureKey(
  roster: readonly RosterMember[],
  leafIndex: number,
): string | undefined {
  const hit = roster.find((m) => m.leafIndex === leafIndex);
  return hit === undefined ? undefined : base64Encode(hit.signaturePublicKey);
}

/**
 * Read an author identity back off a persisted message-log row.
 *
 * The stored field is `string | null` in a v2 row and **absent
 * entirely** in a v1 row written before it existed. Both the `null`
 * and the absent case collapse to `undefined`, and `undefined` is
 * what the edit/delete gate refuses — which is exactly the posture
 * every reloaded row had before this field existed. There is no
 * migration and none is owed.
 *
 * The tempting alternative for an OUTGOING row — "it is ours, so
 * fall back to our current key" — is wrong: it manufactures a
 * confident author out of a record that has none, on an assumption
 * the stored row does not support. Fail closed, and let the gate
 * refuse, which is what it did for every reloaded row before this
 * field existed.
 */
export function persistedAuthorIdentity(
  stored: string | null | undefined,
): MemberIdentity | undefined {
  return stored ?? undefined;
}

/**
 * The inverse, for the persist sweep. An unresolved identity is
 * written as an explicit `null` rather than dropped, so a row that
 * genuinely had no attributable author is distinguishable on disk
 * from a v1 row that predates the field.
 */
export function authorIdentityToPersisted(
  identity: MemberIdentity | undefined,
): string | null {
  return identity ?? null;
}
