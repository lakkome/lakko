// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * ADR-0065 "Phase 2 revised: silent continuity", R.7 and R.8 - what each
 * shared conversation does once a member's account-key reset was ACCEPTED
 * (`contacts/ResetVerdict.kt`), as pure decisions.
 *
 * Nothing here has a surface: no copy, no row, no flag (R.0). The repository
 * gathers the roster, the role table and this device's role, and acts on what
 * these functions answer. Mirror of `apps/web/src/chat/reset-repair.ts`
 * (R.12).
 *
 * The four cases (R.7):
 *   1. one-to-one: not repairable; the surviving side founds a successor by
 *      claim-then-build (`SuccessorClaim.kt`);
 *   2. repair: a role-table group the superseded identity is a member or an
 *      admin of. Owners and admins propose `Remove` for the old leaves and
 *      `Add` for the new identity's devices; the owner restores an admin;
 *   3. owned: the superseded identity OWNS the group; a successor is founded;
 *   4. no-role-table: nothing is done.
 */

/** This device's ADR-0022 role; `null` without a role table. */
public enum class OwnRole { OWNER, ADMIN, MEMBER }

/** Which of R.7's cases a group is, for ONE superseded key in its roster. */
public enum class ResetCase { ONE_TO_ONE, REPAIR, OWNED, NO_ROLE_TABLE }

/**
 * | kind  | role table | owner is the superseded key | case            |
 * |-------|------------|-----------------------------|-----------------|
 * | chat  | any        | any                         | `ONE_TO_ONE`    |
 * | group | absent     | -                           | `NO_ROLE_TABLE` |
 * | group | present    | yes                         | `OWNED`         |
 * | group | present    | no                          | `REPAIR`        |
 */
public fun resetCase(
    kind: ConversationKind,
    /** The role table's owner key, base64; `null` = no role table. */
    ownerKeyB64: String?,
    supersededKeyB64: String,
): ResetCase = when {
    kind == ConversationKind.Chat -> ResetCase.ONE_TO_ONE
    ownerKeyB64 == null -> ResetCase.NO_ROLE_TABLE
    ownerKeyB64 == supersededKeyB64 -> ResetCase.OWNED
    else -> ResetCase.REPAIR
}

/** A roster leaf as these decisions read it: the leaf and the PERSON key (the
 *  account key the leaf is bound to, ADR-0050), base64. */
public data class PersonLeaf(val leafIndex: UInt, val personKeyB64: String)

/** The leaves bound to [supersededKeyB64]: the old identity's devices. */
public fun leavesOfKey(roster: List<PersonLeaf>, supersededKeyB64: String): List<UInt> =
    roster.filter { it.personKeyB64 == supersededKeyB64 }.map { it.leafIndex }

/** What this device proposes in a `REPAIR` group for one superseded key. */
public data class RepairPlan(val removeLeaf: UInt?, val mayAdd: Boolean)

/**
 * | own role       | old leaves in roster | new devices missing | proposes                          |
 * |----------------|----------------------|---------------------|-----------------------------------|
 * | owner / admin  | yes                  | any                 | `Remove` of one old leaf per pass |
 * | owner / admin  | any                  | yes                 | `Add` of the missing new devices  |
 * | member / null  | any                  | any                 | nothing (the core refuses it)     |
 *
 * A `Remove` already in the pending set, or this device's own in flight, is
 * not proposed again; a torn roster (`null`) proposes nothing. The removal is
 * one leaf per pass because the proposal path takes one target; the next pass
 * takes the next.
 */
public fun repairPlan(
    ownRole: OwnRole?,
    roster: List<PersonLeaf>?,
    supersededKeyB64: String,
    /** Leaves a pending `Remove` proposal already names. */
    pendingRemoveLeaves: Collection<UInt>,
    /** This device's own `Remove` for this key is in flight. */
    removeInFlight: Boolean,
    /** This device's membership ended. */
    membershipEnded: Boolean,
): RepairPlan {
    val none = RepairPlan(null, false)
    if (membershipEnded || roster == null) return none
    if (ownRole != OwnRole.OWNER && ownRole != OwnRole.ADMIN) return none
    val candidate = leavesOfKey(roster, supersededKeyB64).firstOrNull { it !in pendingRemoveLeaves }
    return RepairPlan(removeLeaf = if (removeInFlight) null else candidate, mayAdd = true)
}

/**
 * R.7 case 2 / the re-admission arm: whether this device proposes an `Add`
 * for the devices of an account whose reset it accepted and whose NEW key is
 * not in the group yet. Only an owner or an admin: a plain member's `Add` for
 * an account that is not a member is refused by the core.
 */
public fun mayAddResetIdentity(ownRole: OwnRole?): Boolean =
    ownRole == OwnRole.OWNER || ownRole == OwnRole.ADMIN

/**
 * R.7 case 2: the admin set the OWNER proposes when the superseded key was an
 * admin: the same set with the new key in its place. `null` when nothing is
 * to be proposed (not the owner, the superseded key is no admin, or the new
 * identity is not a member yet: a role for a key no leaf holds is never
 * written).
 */
public fun adminRestorePlan(
    ownRole: OwnRole?,
    adminKeysB64: List<String>,
    supersededKeyB64: String,
    newKeyB64: String,
    /** The PERSON keys of the live roster. */
    rosterPersonKeysB64: Collection<String>,
): List<String>? {
    if (ownRole != OwnRole.OWNER) return null
    if (supersededKeyB64 !in adminKeysB64) return null
    if (newKeyB64 !in rosterPersonKeysB64) return null
    val next = adminKeysB64.filter { it != supersededKeyB64 }.toMutableList()
    if (newKeyB64 !in next) next += newKeyB64
    return next
}

/** One role change the owner proposes next (one per sweep, C-65). */
public sealed interface AdminRestoreStep {
    public data class Promote(val keyB64: String) : AdminRestoreStep
    public data class Demote(val keyB64: String) : AdminRestoreStep
}

/**
 * [adminRestorePlan] as the NEXT single role change (one per sweep, C-65).
 *
 * | superseded key is an admin | new key in roster | new key is an admin | a role change is pending | next step       |
 * |----------------------------|-------------------|---------------------|--------------------------|-----------------|
 * | any                        | any               | any                 | yes                      | none            |
 * | no                         | any               | any                 | no                       | none            |
 * | yes                        | no                | -                   | no                       | none (wait)     |
 * | yes                        | yes               | no                  | no                       | promote the new |
 * | yes                        | yes               | yes                 | no                       | demote the old  |
 *
 * The new key is promoted BEFORE the old one is demoted, so the person is
 * never without the role in between.
 */
public fun adminRestoreStep(
    ownRole: OwnRole?,
    adminKeysB64: List<String>,
    supersededKeyB64: String,
    newKeyB64: String,
    rosterPersonKeysB64: Collection<String>,
    roleChangePending: Boolean,
): AdminRestoreStep? {
    if (roleChangePending) return null
    adminRestorePlan(ownRole, adminKeysB64, supersededKeyB64, newKeyB64, rosterPersonKeysB64) ?: return null
    return if (newKeyB64 in adminKeysB64) {
        AdminRestoreStep.Demote(supersededKeyB64)
    } else {
        AdminRestoreStep.Promote(newKeyB64)
    }
}

/**
 * R.7 case 2 / UX 16.5: the `Members changed` row is suppressed for a commit
 * that leaves the set of PEOPLE unchanged, each person named by account id. A
 * torn read on either side suppresses nothing.
 */
public fun membersChangedSuppressed(peopleBefore: List<String>?, peopleAfter: List<String>?): Boolean {
    if (peopleBefore == null || peopleAfter == null) return false
    return peopleBefore.toSet() == peopleAfter.toSet()
}

/** The people of a roster by the name the suppression compares: the account
 *  id a key is known under, else the key itself. */
public fun peopleByAccount(personKeysB64: List<String>, accountOfKey: (String) -> String?): List<String> {
    val out = ArrayList<String>()
    for (k in personKeysB64) {
        val person = accountOfKey(k) ?: k
        if (person !in out) out += person
    }
    return out
}

/** How long a person whose superseded devices were removed is expected back
 *  before the memory of the first half is dropped. The journal retention
 *  window; chosen, not measured. */
public const val DEVICE_SWAP_PAIR_WINDOW_MS: Long = 30L * 24 * 60 * 60 * 1000

/** The verdict on one commit, and the memory to keep. */
public data class DeviceSwapVerdict(val suppress: Boolean, val awaited: Map<String, Long>)

/**
 * R.7 case 2 / UX 16.5 across TWO commits. The repair of a member who reset
 * is a `Remove` of the old leaves and an `Add` of the new devices; when they
 * land in two commits the first looks like a person leaving and the second
 * like a person joining, though the set of people across the PAIR is
 * unchanged. This judges one commit with the memory of the halves seen
 * before it, and returns the memory to keep.
 *
 * | the commit ...                                                  | row        | memory                 |
 * |-----------------------------------------------------------------|------------|------------------------|
 * | changes no key                                                  | kept       | unchanged              |
 * | changes keys, not people (one batch)                            | suppressed | unchanged              |
 * | only REMOVES people, each present through superseded keys alone | suppressed | those people are added |
 * | only ADDS people, each one awaited                              | suppressed | those people leave it  |
 * | anything else                                                   | kept       | whoever came leaves it |
 *
 * An awaited person older than [DEVICE_SWAP_PAIR_WINDOW_MS] is forgotten
 * first. A torn read keeps the row and the memory.
 */
public fun deviceSwapAcrossCommits(
    personKeysBefore: List<String>?,
    personKeysAfter: List<String>?,
    accountOfKey: (String) -> String?,
    isSuperseded: (String) -> Boolean,
    awaited: Map<String, Long>,
    nowMs: Long,
): DeviceSwapVerdict {
    val live = LinkedHashMap<String, Long>()
    for ((person, sinceMs) in awaited) if (nowMs - sinceMs < DEVICE_SWAP_PAIR_WINDOW_MS) live[person] = sinceMs
    if (personKeysBefore == null || personKeysAfter == null) return DeviceSwapVerdict(false, live)
    // The same KEYS: nothing happened to the membership at all.
    if (personKeysBefore.toSet() == personKeysAfter.toSet()) return DeviceSwapVerdict(false, live)
    val name = { k: String -> accountOfKey(k) ?: k }
    val peopleBefore = peopleByAccount(personKeysBefore, accountOfKey)
    val peopleAfter = peopleByAccount(personKeysAfter, accountOfKey)
    val gone = peopleBefore.filter { it !in peopleAfter }
    val came = peopleAfter.filter { it !in peopleBefore }
    // Whoever is in the roster now is no longer awaited, whatever the verdict.
    for (p in peopleAfter) live.remove(p)
    if (gone.isEmpty() && came.isEmpty()) return DeviceSwapVerdict(true, live)
    val goneBySupersededOnly = gone.all { p ->
        personKeysBefore.filter { name(it) == p }.all(isSuperseded)
    }
    val cameAwaited = came.all { p ->
        val since = awaited[p]
        since != null && nowMs - since < DEVICE_SWAP_PAIR_WINDOW_MS
    }
    if (came.isEmpty() && goneBySupersededOnly) {
        for (p in gone) live[p] = nowMs
        return DeviceSwapVerdict(true, live)
    }
    if (gone.isEmpty() && cameAwaited) return DeviceSwapVerdict(true, live)
    return DeviceSwapVerdict(false, live)
}

// ---------------------------------------------------------------------------
// The two decisions awaiting the maintainer's confirmation. Each is ONE
// function: flipping the default is a one-line change here and nowhere else.
// ---------------------------------------------------------------------------

/**
 * R.7 case 3: after the successor of an OWNED group is built, is ownership
 * transferred back to the identity that reset? `true` (the ADR's default):
 * the founding admin proposes the transfer once the new identity is a member,
 * and stays an admin if it was one. `false`: the founding admin stays owner.
 */
public fun restoreOwnershipToResetIdentity(): Boolean = true

/**
 * R.7 case 1 / decision D5: does the founder of a successor one-to-one
 * re-send, into it, its own account's messages sent in the OLD one-to-one
 * after the attestation's `signed_at`? `true` (the ADR's default): re-send.
 * `false`: they are lost with no sign.
 */
public fun resendOwnMessagesAfterReset(): Boolean = true

/** A stored message of the old one-to-one, as the re-send reads it. */
public data class OldSegmentMessage(
    val messageId: Long,
    /** Whether THIS account sent it (any of its devices). */
    val own: Boolean,
    /** Sender-clock or DS time of the row, UNIX milliseconds; `null` when the
     *  row carries no readable time. */
    val sentAtMs: Long?,
    /** A chat message with content to send again (not an op, a system row, a
     *  deletion or a call record). */
    val resendable: Boolean,
)

/**
 * R.7 case 1: the ids of the old segment's messages the founder sends again
 * into the successor, in their original order. Empty when
 * [resendOwnMessagesAfterReset] is off. A row with no readable time is NOT
 * re-sent: the rule is "after the reset was signed", and an unknown time
 * proves nothing.
 */
public fun messagesToResend(messages: List<OldSegmentMessage>, resetSignedAtMs: Long): List<Long> {
    if (!resendOwnMessagesAfterReset()) return emptyList()
    return messages
        .filter { it.own && it.resendable && it.sentAtMs != null && it.sentAtMs > resetSignedAtMs }
        .map { it.messageId }
        .sorted()
}

/**
 * R.7 case 1: what is left of [ids] (ascending, as [messagesToResend] gives
 * them) after an interrupted re-send whose last LANDED message is
 * [cursor]: only the later ones. A re-send that never landed anything
 * (`null`) starts at the first.
 */
public fun resendRemaining(ids: List<Long>, cursor: Long?): List<Long> =
    if (cursor == null) ids else ids.filter { it > cursor }

/** The quote a stored message carries, as the re-send reads it. */
public data class ResendReply(val author: String?, val excerpt: String?)

/** The forward attribution a stored message carries. */
public data class ResendForward(val author: String?, val originGroupIdB64: String?, val originMessageId: Long?)

/** What a re-sent message's envelope carries beside its text and
 *  attachments. */
public data class ResendEnvelopeParts(
    val replyTo: Long?,
    val replyAuthor: String?,
    val replyExcerpt: String?,
    val forwardedFrom: String?,
    val forwardOriginGroupB64: String?,
    val forwardOriginMessageId: Long?,
    val resendOriginGroupB64: String,
    val resendOriginMessageId: Long,
)

/**
 * R.7 case 1: a re-sent message is the SAME message, so what it said about
 * other messages goes with it.
 *
 * | the original | the re-sent envelope carries                                        |
 * |--------------|---------------------------------------------------------------------|
 * | plain        | the marker                                                          |
 * | a reply      | the marker, the quote's author and excerpt, and target 0 (no jump)  |
 * | a forward    | the marker, the attribution, and the origin pair when it was whole  |
 *
 * A reply's target is an id in the OLD journal, which means nothing in the
 * successor's: the quote keeps its snapshot and loses its jump
 * ([REPLY_NO_JUMP_TARGET]), as a held reply does. A quote with neither an
 * author nor an excerpt is nothing to carry. A forward's origin names
 * another conversation and stays as it was; half a pair is dropped.
 */
public fun resendEnvelopeParts(
    originGroupB64: String,
    originMessageId: Long,
    reply: ResendReply?,
    forward: ResendForward?,
): ResendEnvelopeParts {
    val quote = reply?.takeIf { !it.author.isNullOrEmpty() || !it.excerpt.isNullOrEmpty() }
    val wholeOrigin = forward != null && !forward.originGroupIdB64.isNullOrEmpty() && forward.originMessageId != null
    return ResendEnvelopeParts(
        replyTo = if (quote != null) REPLY_NO_JUMP_TARGET else null,
        replyAuthor = quote?.author,
        replyExcerpt = quote?.excerpt,
        forwardedFrom = forward?.author,
        forwardOriginGroupB64 = if (wholeOrigin) forward?.originGroupIdB64 else null,
        forwardOriginMessageId = if (wholeOrigin) forward?.originMessageId else null,
        resendOriginGroupB64 = originGroupB64,
        resendOriginMessageId = originMessageId,
    )
}

/**
 * R.7 case 3: who the winner adds to an owned group's successor: every person
 * of the old roster except the superseded keys and this device's own account
 * (it is the founder), each once.
 */
public fun successorPeople(
    oldRosterPersonKeysB64: List<String>,
    supersededKeysB64: Set<String>,
    ownPersonKeyB64: String,
): List<String> {
    val out = ArrayList<String>()
    for (k in oldRosterPersonKeysB64) {
        if (k == ownPersonKeyB64 || k in supersededKeysB64) continue
        if (k !in out) out += k
    }
    return out
}

/**
 * R.7 case 3 "the completeness re-check": the people of the old roster that
 * the successor still lacks.
 */
public fun successorMissingPeople(
    oldRosterPersonKeysB64: List<String>,
    successorPersonKeysB64: Collection<String>,
    supersededKeysB64: Set<String>,
): List<String> {
    val have = successorPersonKeysB64.toSet()
    val out = ArrayList<String>()
    for (k in oldRosterPersonKeysB64) {
        if (k in supersededKeysB64 || k in have) continue
        if (k !in out) out += k
    }
    return out
}

/**
 * R.7 case 3: the admin set the winner restores in the successor: the old
 * table's admins that are members of the successor, minus the superseded keys
 * and minus the successor's owner (an owner is not also listed).
 */
public fun successorAdmins(
    oldAdminKeysB64: List<String>,
    successorPersonKeysB64: Collection<String>,
    successorOwnerKeyB64: String,
    supersededKeysB64: Set<String>,
): List<String> {
    val members = successorPersonKeysB64.toSet()
    return oldAdminKeysB64.filter { it in members && it != successorOwnerKeyB64 && it !in supersededKeysB64 }
}

/**
 * R.5 "the old segment after the move" / R.8: a row of an old segment whose
 * MLS-authenticated sender resolves to a SUPERSEDED key is dropped before
 * storage (a removed device still holding the old keys can post there). Only
 * for a key an accepted reset retired; a proven predecessor of a current key
 * is the same person and is never dropped.
 */
public fun dropsSupersededSender(senderKeyB64: String?, supersededKeys: Set<String>): Boolean =
    senderKeyB64 != null && senderKeyB64 in supersededKeys
