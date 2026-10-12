// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * ADR-0065 "Phase 2 revised", R.6 - founding a successor by CLAIM, THEN
 * BUILD, and the receive rule for the claim, as pure decisions.
 *
 * A one-to-one whose other person reset their keys, and a group whose owner
 * did, cannot be repaired in place: the conversation continues in a NEW group
 * behind the same row. Group ids are random, so two devices acting at once
 * would found two successors; the claim makes it one:
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
 * the read-decide-write (C-62 sub-invariant 5). Mirror of
 * `apps/web/src/chat/successor-claim.ts` (R.12).
 */

/** One claim as read off the old group's journal. */
public data class SuccessorClaim(
    /** The DS message id of the row in the OLD group's journal: the order. */
    val messageId: Long,
    /** base64 of the successor group's id. */
    val successorGroupIdB64: String,
    /** base64 of the sender's ACCOUNT key, resolved at receipt from the
     *  MLS-authenticated sender (C-68); never read from the envelope. */
    val senderKeyB64: String,
    /** Whether the sender was an admin in the live role table at receipt. */
    val senderWasAdmin: Boolean,
    /** The admin keys of the live role table at receipt (base64). */
    val adminKeysAtReceiptB64: List<String>,
)

/**
 * The account key whose accepted reset makes this group unrepairable (R.6
 * (i)): the other participant of a one-to-one, or the owner of a group.
 * `null` when the group is repairable or nothing was accepted for it.
 *
 * | kind  | superseded key is ...                 | answer   |
 * |-------|---------------------------------------|----------|
 * | chat  | a person of the roster (not this one) | that key |
 * | group | the role table's owner                | that key |
 * | any   | anything else                         | `null`   |
 */
public fun unrepairableBy(
    kind: ConversationKind,
    rosterPersonKeysB64: Collection<String>,
    ownPersonKeyB64: String?,
    /** `null` = no role table. */
    ownerKeyB64: String?,
    supersededKeys: Set<String>,
): String? {
    if (kind == ConversationKind.Chat) {
        return rosterPersonKeysB64.firstOrNull { it != ownPersonKeyB64 && it in supersededKeys }
    }
    if (ownerKeyB64 == null) return null
    return ownerKeyB64.takeIf { it in supersededKeys }
}

/**
 * R.6 "Eligible": who may found the successor, and whose claim counts.
 *
 * | kind  | sender                                                 | eligible |
 * |-------|--------------------------------------------------------|----------|
 * | any   | a superseded key                                       | no       |
 * | chat  | the surviving account (any of its devices)             | yes      |
 * | group | an admin other than the superseded owner               | yes      |
 * | group | a member, when the table names no admin but that owner | yes      |
 * | group | a member, when another admin exists                    | no       |
 */
public fun claimEligible(
    kind: ConversationKind,
    senderKeyB64: String,
    senderWasAdmin: Boolean,
    adminKeysB64: List<String>,
    /** The key whose reset made the group unrepairable. */
    supersededKeyB64: String,
    supersededKeys: Set<String>,
    /** One-to-one only: the account that did not reset. */
    survivingKeyB64: String?,
): Boolean {
    if (senderKeyB64 == supersededKeyB64) return false
    if (senderKeyB64 in supersededKeys) return false
    if (kind == ConversationKind.Chat) return senderKeyB64 == survivingKeyB64
    val otherAdmins = adminKeysB64.filter { it != supersededKeyB64 && it !in supersededKeys }
    if (otherAdmins.isNotEmpty()) return senderWasAdmin && senderKeyB64 in otherAdmins
    return true
}

/** What the receive rule does with an incoming claim. */
public enum class ClaimVerdict {
    /** The first eligible claim in the journal's order. */
    ACCEPT,

    /** A claim is already held: later claims are ignored for good. */
    IGNORE_LATER,

    /** The receiver has no accepted reset for this group YET: keep the claim
     *  and judge it again after the identity check the claim triggers. */
    KEEP_PENDING,

    /** The sender is not eligible: dropped. */
    DROP_INELIGIBLE,

    /** Names no successor, or names this very group: not a claim. */
    DROP_MALFORMED,
}

/**
 * The receive rule (R.6), replacing "owner or admin, strictly newer stamp
 * wins":
 *
 *   (i)   the receiver has itself accepted a reset of the account that makes
 *         this group unrepairable ([unrepairableBy]);
 *   (ii)  the sender, resolved at receipt, is eligible ([claimEligible]);
 *   (iii) it is the FIRST such claim in the journal's order.
 *
 * [held] is the claim already accepted for this group, if any.
 */
public fun claimVerdict(
    kind: ConversationKind,
    groupIdB64: String,
    claim: SuccessorClaim,
    held: SuccessorClaim?,
    /** [unrepairableBy] at the moment of judging. */
    unrepairableKeyB64: String?,
    supersededKeys: Set<String>,
    survivingKeyB64: String?,
): ClaimVerdict {
    if (claim.successorGroupIdB64.isEmpty() || claim.successorGroupIdB64 == groupIdB64) {
        return ClaimVerdict.DROP_MALFORMED
    }
    // A superseded sender is never eligible, verdict or not: a removed device
    // that still holds the old keys can post here, and its claim must not
    // even wait in the pending list.
    if (claim.senderKeyB64 in supersededKeys) return ClaimVerdict.DROP_INELIGIBLE
    if (unrepairableKeyB64 == null) return ClaimVerdict.KEEP_PENDING
    if (!claimEligible(
            kind = kind,
            senderKeyB64 = claim.senderKeyB64,
            senderWasAdmin = claim.senderWasAdmin,
            adminKeysB64 = claim.adminKeysAtReceiptB64,
            supersededKeyB64 = unrepairableKeyB64,
            supersededKeys = supersededKeys,
            survivingKeyB64 = survivingKeyB64,
        )
    ) {
        return ClaimVerdict.DROP_INELIGIBLE
    }
    if (held != null && held.messageId <= claim.messageId) return ClaimVerdict.IGNORE_LATER
    return ClaimVerdict.ACCEPT
}

/**
 * The first eligible claim among [claims] in the journal's order, or `null`.
 * Used to re-judge the claims kept while the receiver had no verdict, and by
 * a founder reading the old journal up to its own claim (step 3).
 */
public fun firstEligibleClaim(
    kind: ConversationKind,
    groupIdB64: String,
    claims: List<SuccessorClaim>,
    unrepairableKeyB64: String?,
    supersededKeys: Set<String>,
    survivingKeyB64: String?,
): SuccessorClaim? =
    claims.sortedBy { it.messageId }.firstOrNull {
        claimVerdict(kind, groupIdB64, it, null, unrepairableKeyB64, supersededKeys, survivingKeyB64) ==
            ClaimVerdict.ACCEPT
    }

/** Step 3: how a founder's own claim ended. */
public sealed interface ClaimOutcome {
    /** No earlier eligible claim: this device builds the successor. */
    public data object Won : ClaimOutcome

    /** An earlier eligible claim is in the journal: discard the local
     *  successor (never announced, no Welcome sent) and adopt that one. */
    public data class Lost(val winner: SuccessorClaim) : ClaimOutcome
}

/** Decide step 3 from the old journal's first eligible claim up to and
 *  including the founder's own row. */
public fun claimOutcome(ownClaimMessageId: Long, first: SuccessorClaim?): ClaimOutcome =
    if (first == null || first.messageId >= ownClaimMessageId) ClaimOutcome.Won else ClaimOutcome.Lost(first)

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
 */
public fun shouldFoundSuccessor(
    kind: ConversationKind,
    unrepairableKeyB64: String?,
    held: SuccessorClaim?,
    ownClaimPosted: Boolean,
    membershipEnded: Boolean,
    ownPersonKeyB64: String?,
    ownIsAdmin: Boolean,
    adminKeysB64: List<String>,
    supersededKeys: Set<String>,
    newIdentityHasDevices: Boolean,
): Boolean {
    if (unrepairableKeyB64 == null) return false
    if (held != null || ownClaimPosted || membershipEnded) return false
    if (ownPersonKeyB64 == null) return false
    if (!newIdentityHasDevices) return false
    return claimEligible(
        kind = kind,
        senderKeyB64 = ownPersonKeyB64,
        senderWasAdmin = ownIsAdmin,
        adminKeysB64 = adminKeysB64,
        supersededKeyB64 = unrepairableKeyB64,
        supersededKeys = supersededKeys,
        survivingKeyB64 = ownPersonKeyB64,
    )
}
