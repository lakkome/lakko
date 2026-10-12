// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 "Phase 2 revised", R.6 - founding a successor by CLAIM, THEN
 * BUILD, and the receive rule for the claim, as pure decisions.
 *
 * A one-to-one whose other person reset their keys, and a group whose owner
 * did, cannot be repaired in place: the conversation continues in a NEW
 * group behind the same row. Group ids are random, so two devices acting at
 * once would found two successors; the claim makes it one:
 *
 *   1. an eligible device creates the successor locally and claims its DS
 *      journal; nothing is sent to anyone;
 *   2. it posts a `group_closed` envelope naming the successor into the OLD
 *      group and notes the DS message id;
 *   3. it reads the old journal up to that id: an EARLIER eligible claim
 *      means it lost (discard, adopt the earlier one); otherwise it won;
 *   4. only the winner builds (adds, Welcomes, name, roles).
 *
 * The envelope codec is unchanged (`lakko/group-closed/v1`); its only
 * producer is now a claim. Journal order is the DS's arrival order, which
 * every member reads identically; the caller holds the per-group lock over
 * the read-decide-write (C-62 sub-invariant 5).
 *
 * Free of wasm, Solid and storage. iOS and Android mirror this (R.12).
 */
import type { ConversationKind } from "./conversation-kind";

/** One claim as read off the old group's journal. */
export interface SuccessorClaim {
  /** The DS message id of the row in the OLD group's journal: the order. */
  readonly messageId: number;
  /** base64 of the successor group's id. */
  readonly successorGroupIdB64: string;
  /** base64 of the sender's ACCOUNT key, resolved at receipt from the
   *  MLS-authenticated sender (C-68); never read from the envelope. */
  readonly senderKeyB64: string;
  /** Whether the sender was an admin in the live role table at receipt. */
  readonly senderWasAdmin: boolean;
  /** The admin keys of the live role table at receipt (base64). */
  readonly adminKeysAtReceiptB64: readonly string[];
}

/**
 * The account whose accepted reset makes this group unrepairable (R.6 (i)):
 * the other participant of a one-to-one, or the owner of a group. `null`
 * when the group is repairable or nothing was accepted for it.
 *
 * | kind  | superseded key is ...                 | answer  |
 * |-------|---------------------------------------|---------|
 * | chat  | a person of the roster (not this one) | that key|
 * | group | the role table's owner                | that key|
 * | any   | anything else                         | `null`  |
 */
export function unrepairableBy(input: {
  readonly kind: ConversationKind;
  readonly rosterPersonKeysB64: readonly string[];
  readonly ownPersonKeyB64: string | null;
  /** `undefined` = no role table. */
  readonly ownerKeyB64: string | undefined;
  readonly supersededKeys: ReadonlySet<string> | ReadonlyMap<string, unknown>;
}): string | null {
  if (input.kind === "chat") {
    for (const k of input.rosterPersonKeysB64) {
      if (k !== input.ownPersonKeyB64 && input.supersededKeys.has(k)) return k;
    }
    return null;
  }
  if (input.ownerKeyB64 === undefined) return null;
  return input.supersededKeys.has(input.ownerKeyB64) ? input.ownerKeyB64 : null;
}

/**
 * R.6 "Eligible": who may found the successor, and whose claim counts.
 *
 * | kind  | sender                                                    | eligible |
 * |-------|-----------------------------------------------------------|----------|
 * | any   | a superseded key                                          | no       |
 * | chat  | the surviving account (any of its devices)                | yes      |
 * | group | an admin other than the superseded owner                  | yes      |
 * | group | a member, when the table names no admin but that owner    | yes      |
 * | group | a member, when another admin exists                       | no       |
 *
 * `survivingKeyB64` is, for a one-to-one, the account that did not reset:
 * the roster's other person as the judging device sees it.
 */
export function claimEligible(input: {
  readonly kind: ConversationKind;
  readonly senderKeyB64: string;
  readonly senderWasAdmin: boolean;
  readonly adminKeysB64: readonly string[];
  /** The key whose reset made the group unrepairable. */
  readonly supersededKeyB64: string;
  readonly supersededKeys: ReadonlySet<string> | ReadonlyMap<string, unknown>;
  /** One-to-one only: the account that did not reset. */
  readonly survivingKeyB64: string | null;
}): boolean {
  if (input.senderKeyB64 === input.supersededKeyB64) return false;
  if (input.supersededKeys.has(input.senderKeyB64)) return false;
  if (input.kind === "chat") return input.senderKeyB64 === input.survivingKeyB64;
  const otherAdmins = input.adminKeysB64.filter(
    (k) => k !== input.supersededKeyB64 && !input.supersededKeys.has(k),
  );
  if (otherAdmins.length > 0) return input.senderWasAdmin && otherAdmins.includes(input.senderKeyB64);
  return true;
}

/** What the receive rule does with an incoming claim. */
export type ClaimVerdict =
  /** The first eligible claim in the journal's order: the conversation's
   *  successor is the one it names. */
  | "accept"
  /** A claim is already held: later claims are ignored for good. */
  | "ignore-later"
  /** The receiver has no accepted reset for this group YET: keep the claim
   *  and judge it again after the identity check the claim itself triggers. */
  | "keep-pending"
  /** The sender is not eligible (a plain member where an admin exists, the
   *  superseded identity's own leaves): dropped. */
  | "drop-ineligible"
  /** Names no successor, or names this very group: not a claim. */
  | "drop-malformed";

/**
 * The receive rule (R.6), replacing "owner or admin, strictly newer stamp
 * wins":
 *
 *   (i)   the receiver has itself accepted a reset of the account that makes
 *         this group unrepairable ({@link unrepairableBy});
 *   (ii)  the sender, resolved at receipt, is eligible ({@link claimEligible});
 *   (iii) it is the FIRST such claim in the journal's order.
 *
 * `held` is the claim already accepted for this group, if any. A claim with
 * a LOWER message id than the held one (the receiver judged a pending claim
 * late, or read the journal out of order) replaces it only through
 * {@link firstEligibleClaim}, which the caller runs over everything it kept.
 */
export function claimVerdict(input: {
  readonly kind: ConversationKind;
  readonly groupIdB64: string;
  readonly claim: SuccessorClaim;
  readonly held: SuccessorClaim | null;
  /** {@link unrepairableBy} at the moment of judging. */
  readonly unrepairableKeyB64: string | null;
  readonly supersededKeys: ReadonlySet<string> | ReadonlyMap<string, unknown>;
  readonly survivingKeyB64: string | null;
}): ClaimVerdict {
  const claim = input.claim;
  if (claim.successorGroupIdB64.length === 0 || claim.successorGroupIdB64 === input.groupIdB64) {
    return "drop-malformed";
  }
  // A superseded sender is never eligible, verdict or not: a removed device
  // that still holds the old keys can post here, and its claim must not even
  // wait in the pending list.
  if (input.supersededKeys.has(claim.senderKeyB64)) return "drop-ineligible";
  if (input.unrepairableKeyB64 === null) return "keep-pending";
  if (
    !claimEligible({
      kind: input.kind,
      senderKeyB64: claim.senderKeyB64,
      senderWasAdmin: claim.senderWasAdmin,
      adminKeysB64: claim.adminKeysAtReceiptB64,
      supersededKeyB64: input.unrepairableKeyB64,
      supersededKeys: input.supersededKeys,
      survivingKeyB64: input.survivingKeyB64,
    })
  ) {
    return "drop-ineligible";
  }
  if (input.held !== null && input.held.messageId <= claim.messageId) return "ignore-later";
  return "accept";
}

/**
 * The first eligible claim among `claims` in the journal's order, or `null`.
 * Used to re-judge the claims kept while the receiver had no verdict, and by
 * a founder reading the old journal up to its own claim (step 3).
 */
export function firstEligibleClaim(input: {
  readonly kind: ConversationKind;
  readonly groupIdB64: string;
  readonly claims: readonly SuccessorClaim[];
  readonly unrepairableKeyB64: string | null;
  readonly supersededKeys: ReadonlySet<string> | ReadonlyMap<string, unknown>;
  readonly survivingKeyB64: string | null;
}): SuccessorClaim | null {
  const ordered = [...input.claims].sort((a, b) => a.messageId - b.messageId);
  for (const claim of ordered) {
    const verdict = claimVerdict({ ...input, claim, held: null });
    if (verdict === "accept") return claim;
  }
  return null;
}

/** Step 3: how a founder's own claim ended. */
export type ClaimOutcome =
  /** No earlier eligible claim: this device builds the successor. */
  | { readonly kind: "won" }
  /** An earlier eligible claim is in the journal: discard the local
   *  successor (never announced, no Welcome sent) and adopt that one. */
  | { readonly kind: "lost"; readonly winner: SuccessorClaim };

/**
 * Decide step 3 from the old journal's eligible claims up to and including
 * the founder's own row.
 */
export function claimOutcome(
  ownClaimMessageId: number,
  first: SuccessorClaim | null,
): ClaimOutcome {
  if (first === null || first.messageId >= ownClaimMessageId) return { kind: "won" };
  return { kind: "lost", winner: first };
}

/**
 * Whether THIS device founds a successor now (steps 1 and 2).
 *
 * | condition                                                     | founds |
 * |---------------------------------------------------------------|--------|
 * | the group is not unrepairable for an accepted reset           | no     |
 * | an eligible claim is already held (stand down, adopt it)      | no     |
 * | this device's own claim is in flight or already posted        | no     |
 * | this device's membership ended                                | no     |
 * | this device's account is not eligible                         | no     |
 * | the new identity publishes no device yet (R.8, no KeyPackage) | no     |
 * | otherwise                                                     | yes    |
 *
 * The last row is the R.8 rule: a founder that finds no published device
 * for the accepted key does nothing and retries at the next trigger.
 */
export function shouldFoundSuccessor(input: {
  readonly kind: ConversationKind;
  readonly unrepairableKeyB64: string | null;
  readonly held: SuccessorClaim | null;
  readonly ownClaimPosted: boolean;
  readonly membershipEnded: boolean;
  readonly ownPersonKeyB64: string | null;
  readonly ownIsAdmin: boolean;
  readonly adminKeysB64: readonly string[];
  readonly supersededKeys: ReadonlySet<string> | ReadonlyMap<string, unknown>;
  readonly newIdentityHasDevices: boolean;
}): boolean {
  if (input.unrepairableKeyB64 === null) return false;
  if (input.held !== null || input.ownClaimPosted || input.membershipEnded) return false;
  if (input.ownPersonKeyB64 === null) return false;
  if (!input.newIdentityHasDevices) return false;
  return claimEligible({
    kind: input.kind,
    senderKeyB64: input.ownPersonKeyB64,
    senderWasAdmin: input.ownIsAdmin,
    adminKeysB64: input.adminKeysB64,
    supersededKeyB64: input.unrepairableKeyB64,
    supersededKeys: input.supersededKeys,
    survivingKeyB64: input.ownPersonKeyB64,
  });
}
