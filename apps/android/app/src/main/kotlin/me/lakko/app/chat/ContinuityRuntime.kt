// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * ADR-0073 "Phase 2 revised", R.6 and R.7 cases 1 and 3 - the ORDER in which
 * a device continues a conversation that cannot be repaired in place, as one
 * function over an injected port.
 *
 * `SuccessorClaim.kt` holds the rules and `ResetRepair.kt` the plans. This
 * file is the sequence between them, and the sequence is the point:
 *
 *   1. create the successor locally and claim its DS journal; nothing is
 *      sent to anyone;
 *   2. post the claim into the OLD group and note its DS message id;
 *   3. wait until the old journal has been read up to that id; an EARLIER
 *      eligible claim means this device lost: discard, adopt the earlier one;
 *   4. only the winner builds: it delivers the Welcomes it prepared, records
 *      the lineage, carries the name, adds the remaining people, restores the
 *      roles and (decision) the owner, and re-sends what was sent into the
 *      dead session (decision).
 *
 * The conversation moves to the successor (the lineage, the held successor)
 * only AFTER its Welcomes were delivered. Moved before, it is a group only
 * this account is in: the row is drawn from a roster naming a key that never
 * sent a profile, and every message sent there is read by nobody (seen live
 * 2026-11-25, when the reset account's new mailbox could not be reached).
 * And nothing is prepared or claimed while the new identity cannot be
 * reached at all ([ContinuityPort.newIdentityReachable]): the old one-to-one
 * stays the conversation, and a send into it is held ([sendRoute]).
 *
 * "Nothing is sent before the claim is confirmed" is what keeps a lost claim
 * harmless; a lost claim AFTER Welcomes went out would leave people in a
 * group nobody continues in (R.14), and this order is the only thing that
 * prevents it. So it lives here, behind a port a test can drive.
 *
 * Every step is idempotent and the pass is re-entered from the top at each
 * trigger: a crash at any point resumes from the claim state the store
 * holds. Mirror of `apps/web/src/chat/continuity-runtime.ts`.
 */

/** What the pass reads off one group, live. */
public data class ContinuityFacts(
    val kind: ConversationKind,
    /** The PERSON keys of the roster, base64. */
    val rosterPersonKeysB64: List<String>,
    /** `null` = no role table. */
    val ownerKeyB64: String?,
    val adminKeysB64: List<String>,
    /** This device's membership is live. */
    val active: Boolean,
)

/** Everything the pass does to the world. Group ids are lower-case hex. */
public interface ContinuityPort {
    public fun ownPersonKeyB64(): String?

    /** Every key an accepted reset retired -> its account id (base64). */
    public fun supersededKeys(): Map<String, String>

    /** The key this device now trusts for an account, or `null`. */
    public fun currentKeyOf(accountIdB64: String): String?

    /** `null` when the group is not held or cannot be read. */
    public fun facts(groupIdHex: String): ContinuityFacts?

    public fun claimState(oldGroupIdHex: String): StoredClaims

    /** How far this device has read the old group's journal. */
    public fun cursorOf(oldGroupIdHex: String): Long?

    /** Whether the new identity publishes at least one device (R.8). */
    public suspend fun newIdentityHasDevices(accountIdB64: String): Boolean

    /**
     * Whether this device holds anything that lets it write a Welcome to the
     * new identity's mailbox (a kept grant of the new key, a delegation). A
     * reset account knows nobody, and since ADR-0078 the continuity key
     * reaches no mailbox, so this is usually `false` until the person sends
     * this account a contact request from the new key (UX 16.6).
     */
    public suspend fun newIdentityReachable(accountIdB64: String): Boolean

    /**
     * Step 1: create the successor of [oldGroupIdHex] locally, add the new
     * identity's devices and this account's other devices in ONE commit,
     * claim its DS journal, and keep the Welcome for step 4. Sends nothing.
     * Returns the successor's id.
     */
    public suspend fun prepareSuccessor(oldGroupIdHex: String, newIdentityAccountIdB64: String): String

    /** Step 2: post the claim; returns the DS message id. The own-claim
     *  record is written before the publish. */
    public suspend fun postClaim(oldGroupIdHex: String, successorGroupIdHex: String): Long

    /** Forget an own claim whose publish never produced a row. */
    public suspend fun abandonOwnClaim(oldGroupIdHex: String)

    /** A lost claim: drop the local successor (never announced). */
    public suspend fun discardSuccessor(successorGroupIdHex: String)

    /** Step 3 decided: record the conversation's successor, clear the own
     *  claim. */
    public suspend fun settle(oldGroupIdHex: String, winner: SuccessorClaim)

    /** The lineage register `successor -> conversation of the old group`.
     *  Idempotent (write-once). */
    public suspend fun recordLineage(oldGroupIdHex: String, successorGroupIdHex: String)

    /** Step 4a: deliver the Welcomes, THEN make the successor a held
     *  conversation (never the other way round: see the file docs).
     *  Idempotent. [SuccessorDelivery.UNREACHABLE]: the new identity's
     *  mailbox could not be written, nothing was made visible;
     *  [SuccessorDelivery.NOT_READY]: the successor could not be brought up.
     *  The next pass tries again either way. */
    public suspend fun deliverSuccessor(oldGroupIdHex: String, successorGroupIdHex: String): SuccessorDelivery

    /** Step 4b: re-send the group name into the successor with its ORIGINAL
     *  stamp (ADR-0051). Idempotent. */
    public suspend fun carryName(oldGroupIdHex: String, successorGroupIdHex: String)

    /** Step 4c: add one person of the old roster (by the key the old roster
     *  names) to the successor. `false` when they could not be resolved or
     *  reached; the completeness re-check retries. */
    public suspend fun addPerson(successorGroupIdHex: String, personKeyB64: String): Boolean

    /** Whether the completeness re-check of [successorGroupIdHex] is due (the
     *  re-admission arm's interval, per successor); stamps it when so. */
    public fun recheckDue(successorGroupIdHex: String): Boolean

    /** Step 4d: set the successor's admin set (one role change). */
    public suspend fun setAdmins(successorGroupIdHex: String, adminKeysB64: List<String>)

    /** Step 4e: propose the ownership transfer to the new identity, the
     *  founder staying an admin when [stayAdmin]. */
    public suspend fun transferOwnership(successorGroupIdHex: String, newOwnerKeyB64: String, stayAdmin: Boolean)

    /** ADR-0101 section 8: post into the delivered successor, verbatim, the
     *  newest profile envelope this device holds for the reset person
     *  (signed by their previous key), so their profile comes back with the
     *  conversation. Idempotent (records that it ran). Runs only after the
     *  successor was delivered. The default carries nothing. */
    public suspend fun carryProfile(oldGroupIdHex: String, successorGroupIdHex: String, accountIdB64: String) {}

    /** R.7 case 1 / D5: send again, into the successor, this account's
     *  messages sent in the old one-to-one after the reset was signed.
     *  Idempotent (records that it ran). */
    public suspend fun resendOwnMessages(oldGroupIdHex: String, successorGroupIdHex: String, accountIdB64: String)
}

/** How step 4a ended. */
public enum class SuccessorDelivery {
    /** The Welcomes are out and the successor is held here. */
    DELIVERED,

    /** Nothing this device holds reaches the new identity's mailbox. */
    UNREACHABLE,

    /** The successor could not be brought up here (yet). */
    NOT_READY,
}

/** What one pass did, for the caller's log and the tests. */
public enum class ContinuityStep {
    /** Nothing to continue (repairable, no accepted reset, not held). */
    NOTHING,

    /** Another device's claim is the conversation's successor: adopted. */
    ADOPTED,

    /** The new identity publishes no device yet (R.8): retry later. */
    WAITING_FOR_DEVICES,

    /** Nothing this device holds reaches the new identity's mailbox: no
     *  successor is prepared or moved to, retry later. */
    WAITING_FOR_REACH,

    /** This device is not eligible to found. */
    NOT_ELIGIBLE,

    /** Steps 1 and 2 ran: the claim is posted. */
    CLAIMED,

    /** The old journal has not been read up to the own claim yet. */
    WAITING_FOR_JOURNAL,

    /** An earlier eligible claim won: the local successor was discarded. */
    LOST,

    /** This device won and built (or continued building) the successor. */
    BUILT,

    /** A moved admin or owner that did not found the successor added people
     *  the successor still lacked. */
    RECHECKED,

    /** A step failed; the next pass resumes. */
    FAILED,
}

/** One pass over one OLD group. Never throws (cancellation aside). */
public suspend fun continueConversation(port: ContinuityPort, oldGroupIdHex: String): ContinuityStep =
    try {
        runContinuity(port, oldGroupIdHex)
    } catch (cancel: kotlinx.coroutines.CancellationException) {
        throw cancel
    } catch (_: Throwable) {
        ContinuityStep.FAILED
    }

private suspend fun runContinuity(port: ContinuityPort, oldGroupIdHex: String): ContinuityStep {
    val facts = port.facts(oldGroupIdHex) ?: return ContinuityStep.NOTHING
    val own = port.ownPersonKeyB64()
    val superseded = port.supersededKeys()
    if (superseded.isEmpty()) return ContinuityStep.NOTHING
    val unrepairableKeyB64 = unrepairableBy(
        kind = facts.kind,
        rosterPersonKeysB64 = facts.rosterPersonKeysB64,
        ownPersonKeyB64 = own,
        ownerKeyB64 = facts.ownerKeyB64,
        supersededKeys = superseded.keys,
    ) ?: return ContinuityStep.NOTHING
    val accountIdB64 = superseded[unrepairableKeyB64] ?: return ContinuityStep.NOTHING
    val state = port.claimState(oldGroupIdHex)

    // An own claim is undecided: step 3.
    val ownClaim = state.own
    if (ownClaim != null) {
        val ownMessageId = ownClaim.messageId
        if (ownMessageId == null) {
            // The publish never reported an id (a crash or a failure
            // mid-send). Posting again is harmless: both rows name the same
            // successor.
            port.postClaim(oldGroupIdHex, ownClaim.successorGroupIdHex)
            return ContinuityStep.CLAIMED
        }
        val cursor = port.cursorOf(oldGroupIdHex)
        if (cursor == null || cursor < ownMessageId) return ContinuityStep.WAITING_FOR_JOURNAL
        when (val outcome = claimOutcome(ownMessageId, state.accepted)) {
            is ClaimOutcome.Lost -> {
                port.discardSuccessor(ownClaim.successorGroupIdHex)
                port.settle(oldGroupIdHex, outcome.winner)
                port.recordLineage(oldGroupIdHex, groupHexOfB64(outcome.winner.successorGroupIdB64))
                return ContinuityStep.LOST
            }
            ClaimOutcome.Won -> Unit
        }
        if (own == null) return ContinuityStep.FAILED
        val winner = SuccessorClaim(
            messageId = ownMessageId,
            successorGroupIdB64 = groupB64OfHex(ownClaim.successorGroupIdHex),
            senderKeyB64 = own,
            senderWasAdmin = own in facts.adminKeysB64,
            adminKeysAtReceiptB64 = facts.adminKeysB64,
        )
        port.settle(oldGroupIdHex, winner)
        return buildSuccessor(port, oldGroupIdHex, winner, facts, own, accountIdB64, unrepairableKeyB64)
    }

    // A claim is held: the conversation's successor is decided.
    val accepted = state.accepted
    if (accepted != null) {
        val successorHex = groupHexOfB64(accepted.successorGroupIdB64)
        if (own != null && accepted.senderKeyB64 == own) {
            // This account founded it (this device, or another of the
            // person's): whoever holds the successor keeps building it. The
            // lineage is recorded by [buildSuccessor], once the successor was
            // delivered.
            return buildSuccessor(port, oldGroupIdHex, accepted, facts, own, accountIdB64, unrepairableKeyB64)
        }
        port.recordLineage(oldGroupIdHex, successorHex)
        // R.7 case 3 "the completeness re-check": ANY moved admin or owner
        // adds whoever of the old roster the successor still lacks.
        if (own != null && facts.kind == ConversationKind.Group) {
            return recheckCompleteness(port, successorHex, facts, own)
        }
        return ContinuityStep.ADOPTED
    }

    // Steps 1 and 2.
    if (!facts.active || own == null) return ContinuityStep.NOT_ELIGIBLE
    // Eligibility first (no network), then the one directory read (R.8).
    if (!shouldFoundSuccessor(
            kind = facts.kind,
            unrepairableKeyB64 = unrepairableKeyB64,
            held = null,
            ownClaimPosted = false,
            membershipEnded = !facts.active,
            ownPersonKeyB64 = own,
            ownIsAdmin = own in facts.adminKeysB64,
            adminKeysB64 = facts.adminKeysB64,
            supersededKeys = superseded.keys,
            newIdentityHasDevices = true,
        )
    ) {
        return ContinuityStep.NOT_ELIGIBLE
    }
    if (!port.newIdentityHasDevices(accountIdB64)) return ContinuityStep.WAITING_FOR_DEVICES
    // Nothing is prepared, claimed or moved to while no Welcome could reach
    // the new identity (2026-11-25): the old one-to-one stays the
    // conversation, and a send into it is held.
    if (!port.newIdentityReachable(accountIdB64)) return ContinuityStep.WAITING_FOR_REACH
    // The claim state is read again after the suspension above: a claim that
    // landed meanwhile means this device stands down (R.6).
    val again = port.claimState(oldGroupIdHex)
    if (again.accepted != null || again.own != null) return ContinuityStep.ADOPTED
    val successorGroupIdHex = port.prepareSuccessor(oldGroupIdHex, accountIdB64)
    try {
        port.postClaim(oldGroupIdHex, successorGroupIdHex)
    } catch (cause: Throwable) {
        // Nothing was announced: the local successor is dropped and the next
        // trigger starts over.
        port.abandonOwnClaim(oldGroupIdHex)
        port.discardSuccessor(successorGroupIdHex)
        throw cause
    }
    return ContinuityStep.CLAIMED
}

/**
 * Whether a device that MOVED to the successor runs the completeness re-check
 * there (R.7 case 3: "any moved admin or owner").
 *
 * | holds the successor, membership live | own role in the successor | re-checks |
 * |--------------------------------------|---------------------------|-----------|
 * | no                                   | -                         | no        |
 * | yes                                  | owner or admin            | yes       |
 * | yes                                  | member                    | no        |
 */
public fun mayRecheckCompleteness(successor: ContinuityFacts?, ownPersonKeyB64: String): Boolean {
    if (successor == null || !successor.active) return false
    if (ownPersonKeyB64 !in successor.rosterPersonKeysB64) return false
    return successor.ownerKeyB64 == ownPersonKeyB64 || ownPersonKeyB64 in successor.adminKeysB64
}

private suspend fun recheckCompleteness(
    port: ContinuityPort,
    successorGroupIdHex: String,
    old: ContinuityFacts,
    own: String,
): ContinuityStep {
    val successor = port.facts(successorGroupIdHex)
    if (successor == null || !mayRecheckCompleteness(successor, own)) return ContinuityStep.ADOPTED
    val missing = successorMissingPeople(
        oldRosterPersonKeysB64 = old.rosterPersonKeysB64,
        successorPersonKeysB64 = successor.rosterPersonKeysB64,
        supersededKeysB64 = port.supersededKeys().keys,
    ).filter { it != own }
    if (missing.isEmpty()) return ContinuityStep.ADOPTED
    if (!port.recheckDue(successorGroupIdHex)) return ContinuityStep.ADOPTED
    for (personKeyB64 in missing) port.addPerson(successorGroupIdHex, personKeyB64)
    return ContinuityStep.RECHECKED
}

/** Step 4, for the winner. Every sub-step is idempotent. */
private suspend fun buildSuccessor(
    port: ContinuityPort,
    oldGroupIdHex: String,
    winner: SuccessorClaim,
    old: ContinuityFacts,
    own: String,
    accountIdB64: String,
    unrepairableKeyB64: String,
): ContinuityStep {
    val successorGroupIdHex = groupHexOfB64(winner.successorGroupIdB64)
    // Delivered first, then moved to: a successor nobody else is in must
    // never become the conversation (file docs).
    when (port.deliverSuccessor(oldGroupIdHex, successorGroupIdHex)) {
        SuccessorDelivery.DELIVERED -> Unit
        SuccessorDelivery.UNREACHABLE -> return ContinuityStep.WAITING_FOR_REACH
        SuccessorDelivery.NOT_READY -> return ContinuityStep.FAILED
    }
    port.recordLineage(oldGroupIdHex, successorGroupIdHex)
    port.carryProfile(oldGroupIdHex, successorGroupIdHex, accountIdB64)
    val superseded = port.supersededKeys().keys
    if (old.kind == ConversationKind.Chat) {
        port.resendOwnMessages(oldGroupIdHex, successorGroupIdHex, accountIdB64)
        return ContinuityStep.BUILT
    }
    // R.7 case 3: the same people, the same roles and the same owner.
    port.carryName(oldGroupIdHex, successorGroupIdHex)
    val successor = port.facts(successorGroupIdHex) ?: return ContinuityStep.FAILED
    val missing = successorMissingPeople(
        oldRosterPersonKeysB64 = old.rosterPersonKeysB64,
        successorPersonKeysB64 = successor.rosterPersonKeysB64,
        supersededKeysB64 = superseded,
    ).filter { it != own }
    // A person who cannot be resolved or reached keeps writing in the old
    // group; the completeness re-check (this same pass, later) tries again.
    for (personKeyB64 in missing) port.addPerson(successorGroupIdHex, personKeyB64)
    val after = port.facts(successorGroupIdHex) ?: successor
    // Roles are restored by the successor's OWNER only, which is this account
    // until the transfer below lands.
    if (after.ownerKeyB64 != own) return ContinuityStep.BUILT
    val wantedAdmins = successorAdmins(
        oldAdminKeysB64 = old.adminKeysB64,
        successorPersonKeysB64 = after.rosterPersonKeysB64,
        successorOwnerKeyB64 = own,
        supersededKeysB64 = superseded,
    )
    if (wantedAdmins.any { it !in after.adminKeysB64 }) {
        port.setAdmins(successorGroupIdHex, (after.adminKeysB64 + wantedAdmins).distinct())
        // One role change per sweep (C-65): the transfer waits for the next
        // pass.
        return ContinuityStep.BUILT
    }
    val newOwnerKeyB64 = port.currentKeyOf(accountIdB64)
    if (restoreOwnershipToResetIdentity() &&
        old.ownerKeyB64 == unrepairableKeyB64 &&
        newOwnerKeyB64 != null &&
        newOwnerKeyB64 in after.rosterPersonKeysB64
    ) {
        port.transferOwnership(successorGroupIdHex, newOwnerKeyB64, own in old.adminKeysB64)
    }
    return ContinuityStep.BUILT
}

/**
 * Whether a Welcome could reach the new identity now ([ContinuityPort.newIdentityReachable]):
 * a verified mailbox for its current key AND at least one grant candidate the
 * core's plan (G-53) offers for a Welcome. A mailbox with no candidate would
 * only take the unsigned write a closed mailbox drops as stored.
 *
 * | mailbox known | grant candidates | reachable |
 * |---------------|------------------|-----------|
 * | no            | any              | no        |
 * | yes           | 0                | no        |
 * | yes           | > 0              | yes       |
 */
public fun newIdentityReachableVerdict(mailboxKnown: Boolean, candidates: Int): Boolean =
    mailboxKnown && candidates > 0

/**
 * Whether a device may record the lineage of an ACCEPTED claim outside the
 * pass (the repository's tail after each pass). A claim this account posted
 * (any of its devices) moves the conversation only once this device holds
 * the successor, which it does only after the Welcomes were delivered (the
 * founder) or one reached it (another device of the account). A claim of
 * someone else is recorded as soon as it is accepted.
 *
 * | claim by this account | holds the successor | records |
 * |-----------------------|---------------------|---------|
 * | no                    | any                 | yes     |
 * | yes                   | yes                 | yes     |
 * | yes                   | no                  | no      |
 */
public fun mayRecordAcceptedLineage(
    acceptedSenderKeyB64: String,
    ownPersonKeyB64: String?,
    holdsSuccessor: Boolean,
): Boolean = ownPersonKeyB64 == null || acceptedSenderKeyB64 != ownPersonKeyB64 || holdsSuccessor

/** Standard base64 of a group id given as hex ("" for malformed hex). */
public fun groupB64OfHex(hex: String): String {
    if (hex.length % 2 != 0) return ""
    val out = ByteArray(hex.length / 2)
    for (i in out.indices) {
        val hi = Character.digit(hex[i * 2], 16)
        val lo = Character.digit(hex[i * 2 + 1], 16)
        if (hi < 0 || lo < 0) return ""
        out[i] = ((hi shl 4) or lo).toByte()
    }
    return java.util.Base64.getEncoder().encodeToString(out)
}

/** Lower-case hex of a group id given as standard base64 ("" for malformed). */
public fun groupHexOfB64(b64: String): String =
    try {
        java.util.Base64.getDecoder().decode(b64).joinToString("") { "%02x".format(it.toInt() and 0xff) }
    } catch (_: IllegalArgumentException) {
        ""
    }
