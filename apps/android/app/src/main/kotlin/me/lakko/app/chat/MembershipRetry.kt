// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock
import uniffi.lakko_ffi_uniffi.FfiRefusalReason
import uniffi.lakko_ffi_uniffi.OutgoingOwner

/**
 * ADR-0045 UX §7 — automatically re-sending a membership request that was
 * refused because a queue was full. Android mirror of the web reference
 * `apps/web/src/chat/membership-retry.ts`.
 *
 * ## What this closes
 *
 * [membershipRefusal] taught this client to *read* the core's
 * `RefusalDisposition`, so a queue-full refusal already renders as `Too many
 * changes are already waiting to apply. Wait for them to land, then try
 * again.` What it did not do was **act** on it: the user re-tapped by hand,
 * and the moment to re-tap is one they cannot observe — the queue drains when
 * some other device sweeps, which produces no notification and no visible
 * change in a panel they have probably closed.
 *
 * The core states the property that makes an automatic re-send safe, and it
 * states it narrowly: `RetryLater` means *the identical call succeeds once a
 * committer has swept, with the caller doing nothing else in between*. Two
 * reasons qualify today. This file never names them.
 *
 * ## The trigger is an observation, never a timer — and ONE rule covers every
 * retryable reason, present and future
 *
 * A schedule is the obvious shape and it is wrong in both directions: ADR-0045
 * decision 2 gives a request no deadline, so a timer either fires uselessly
 * against a group where nothing changed, or spins forever against a group
 * where no committer is running. The condition is not *time passed*, it is
 * *the queue drained*.
 *
 * So the rule is: **fire when the pending-proposal set is strictly smaller
 * than it was at the refusal.** That is sound for both of today's reasons and
 * — more importantly — it follows from the core's *definition* of
 * `RetryLater` rather than from either variant's name:
 *
 *  - the set only ever shrinks when a commit **merges**, because OpenMLS
 *    empties the whole proposal store on a merge and nothing else removes an
 *    entry (a receipt-gate refusal stores nothing, so it moves no count);
 *  - `RetryLater` is defined as *resolves once a committer has swept*, and a
 *    sweep IS a merged commit;
 *  - therefore a nineteenth `RetryLater` reason is handled correctly by this
 *    rule without anybody editing it, and a reason that would NOT be handled
 *    by it could not have been classified `RetryLater` in the first place.
 *
 * The count compared against is `pendingProposals().size`, which is exactly
 * the number the core's own cap gates on, so this is the literal quantity the
 * refusal was about rather than a proxy for it.
 *
 * **It cannot spin.** No drain, no firing — a quiet group produces no attempts
 * at all, and every attempt is paid for by a real committer sweep. That is the
 * mitigation §7 already prescribes for the superseded case ("re-request only
 * on an observed commit that superseded it, never on a timer"), applied to the
 * sibling case.
 *
 * ## Why the capacity is not consulted, and why it does not need to be
 *
 * `MAX_PENDING_PROPOSALS` is `pub(crate)` and reaches neither FFI surface, so
 * a client cannot test *is there room*. §7.1 records that a client-side
 * literal `64` is the stale-drift class this repo has already been bitten by,
 * and that it fails in the **dangerous** direction — reading as room left when
 * there is none.
 *
 * A shrink is therefore a **necessary** condition rather than a sufficient
 * one, and that is fine for a stated reason rather than by luck: a retry
 * refused `PendingSetFull` again is just another retryable refusal, re-armed
 * against the new count. The mechanism is self-correcting, and each correction
 * still costs one real sweep.
 *
 * ## What is remembered, where, and why losing it is the honest failure
 *
 * IN MEMORY, per `(account, group)`, for the session. Three reasons, and the
 * first decides it:
 *
 *  1. **The trigger has no meaning across a restart.** The record's whole
 *     content is a comparison baseline — the pending count at the refusal. A
 *     count captured in one session says nothing about the set in the next.
 *  2. **A deferred action the user never saw start is not the action they
 *     took.** A durable record could re-send days later, silently, in a group
 *     they have stopped thinking about.
 *  3. **Losing it degrades to exactly the documented behaviour.** The copy
 *     already tells the user to try again and the manual path is unchanged, so
 *     an automatic retry is strictly a bonus on top of a complete manual flow
 *     — which is why it may fail safe by simply not happening.
 *
 * ## Exactly-once
 *
 * The store holds **at most one record per request**, keyed on what the
 * request *is* ([RetryableRequest.key]). Firing takes the record out with
 * [MembershipRetryStore.claim], a lock-guarded take-and-remove, so two
 * evaluations cannot both fire it. The **manual** path claims the same slot
 * before it starts, so a user tapping again disarms rather than races — which
 * matters more than the reverse, because the re-issue goes through the
 * repository's own commit lock and the driver cannot hold that across its own
 * last check.
 *
 * The lock is explicit rather than a confined dispatcher, and its test drives
 * it from real platform threads: a previous sprint found that `Dispatchers.IO`
 * masks a broken lock exactly as thoroughly as a confined dispatcher does.
 *
 * ## Stopping is a state, never a count
 *
 * There is no attempt cap and there must not be one. A cap measures the wrong
 * thing: attempts are not a cost the group pays (each is paid for by a sweep
 * that was going to happen anyway) and a cap would give up on a group that is
 * merely busy, converting a recoverable wait into a permanent failure. §7
 * already settles the same question for the superseded case: *"There is no
 * attempt cap."*
 *
 * ## What is deliberately NOT retried, and it is a CORE finding
 *
 * An **approved external join** cannot participate, because the core destroys
 * the reason on that path: `Group::approve_external_join` re-wraps its inner
 * failure as `Error::Group(format!("external join approve refused: {e}"))`,
 * and `Error::Group` carries no `RefusalReason`. So an approve refused because
 * the pending set was full arrives with no reason at all. An absent reason
 * never retries, so it does not. Fixing it is a one-line core change
 * (propagate rather than re-wrap); inferring the reason from message text is
 * exactly the drift class `MembershipRefusal.kt` exists to end.
 */

// ---------------------------------------------------------------------------
// What a retryable request is
// ---------------------------------------------------------------------------

/**
 * A membership request that may be re-issued verbatim.
 *
 * **Every case is keyed on a 32-byte Ed25519 signature key, never a leaf
 * index.** OpenMLS hands a vacated leaf to the next `Add`, so a stored leaf can
 * silently come to name somebody else — and for a `remove` that is not a stale
 * label, it is removing the wrong person. The leaf the re-issue needs is
 * resolved from the LIVE roster at fire time, which is also what makes
 * "already applied" checkable.
 */
internal sealed interface RetryableRequest {
    /**
     * A stable key, so a second refusal for the same ask replaces the first
     * rather than queueing beside it.
     */
    val key: String

    /**
     * @property subjectAccountIdB64 carried because re-issuing an add
     *   re-fetches the prospective member's KeyPackage from discovery, which is
     *   keyed on the account rather than the signature key — and a KeyPackage
     *   is single-use, so the stale one from the refused attempt cannot be
     *   reused. It is the one field beyond the subject's identity, and it is
     *   here because the re-issue genuinely cannot be made without it.
     */
    data class Add(val subjectKeyB64: String, val subjectAccountIdB64: String) :
        RetryableRequest {
        override val key: String get() = "add:$subjectKeyB64"
    }

    data class Remove(val subjectKeyB64: String) : RetryableRequest {
        override val key: String get() = "remove:$subjectKeyB64"
    }

    data class Leave(val subjectKeyB64: String) : RetryableRequest {
        override val key: String get() = "leave:$subjectKeyB64"
    }

    data class SetAdmin(val subjectKeyB64: String, val makeAdmin: Boolean) : RetryableRequest {
        override val key: String get() = ROLE_CHANGE_KEY
    }

    data class TransferOwnership(val subjectKeyB64: String, val outgoing: OutgoingOwner) :
        RetryableRequest {
        override val key: String get() = ROLE_CHANGE_KEY
    }

    companion object {
        /**
         * Both role-change shapes share ONE slot, because RFC 9420 admits one
         * `GroupContextExtensions` proposal per commit — two could never both
         * be live, so a newer role-change intent supersedes an older armed one.
         */
        const val ROLE_CHANGE_KEY: String = "role-change"
    }
}

/** Both role-change shapes, which share the single RFC 9420 GCE slot. */
private val RetryableRequest.isRoleChange: Boolean
    get() = this is RetryableRequest.SetAdmin || this is RetryableRequest.TransferOwnership

// ---------------------------------------------------------------------------
// Arming
// ---------------------------------------------------------------------------

/** One request waiting for the queue that refused it to drain. */
internal data class ArmedRetry(
    val request: RetryableRequest,
    /**
     * The core's reason. For logs only — nothing branches on it, which is what
     * keeps this file free of the two retryable variant names.
     */
    val reason: FfiRefusalReason,
    /** `pendingProposals().size` at the refusal — the drain baseline. */
    val pendingCountAtRefusal: Int,
)

/**
 * Arm a retry for [error], or `null` when nothing should be re-sent.
 *
 * `null` covers, deliberately, all four of: not a refusal at all; a
 * `DoNotRetry` refusal; a `NotPermitted` one; and a reason this build has never
 * heard of. The last three are one code path — the port is asked and answers
 * `false` — and that is the point: this file holds no list of retryable
 * reasons and could not disagree with the core if it tried.
 *
 * [retryMaySucceed] is a parameter rather than a direct call so the tests can
 * prove the module *asks*. Production passes `::membershipRefusalRetryMaySucceed`.
 */
internal fun armedRetryFor(
    error: Throwable,
    request: RetryableRequest,
    pendingCountAtRefusal: Int,
    explicitReason: FfiRefusalReason? = null,
    retryMaySucceed: (FfiRefusalReason) -> Boolean,
): ArmedRetry? {
    val reason = explicitReason ?: membershipRefusalReason(error) ?: return null
    if (!retryMaySucceed(reason)) return null
    return ArmedRetry(request, reason, pendingCountAtRefusal)
}

// ---------------------------------------------------------------------------
// The verdict
// ---------------------------------------------------------------------------

/** One roster row as the verdict reads it. */
internal data class RetryRosterMember(val leafIndex: UInt, val signatureKeyB64: String)

/** The ADR-0022 role table as the verdict reads it. */
internal data class RetryRoleTable(
    val ownerKeyB64: String,
    val adminKeysB64: List<String>,
)

/**
 * Everything the verdict reads, captured in ONE pass.
 *
 * The roster and the pending set must come from the same read: the merge that
 * empties the pending set is the merge that moves the roster, so reading one
 * before a commit and one after would compare two different moments.
 */
internal data class RetryGroupSnapshot(
    /** `null` is a TORN read, not an empty group, and claims nothing. */
    val roster: List<RetryRosterMember>?,
    /** [proposalKey] of every live pending record. */
    val pendingKeys: List<String>,
    /** `pendingProposals().size` — the number the core's cap gates on. */
    val pendingCount: Int,
    val roleTable: RetryRoleTable?,
    /** ADR-0045 §6.2 — our own leaf has been removed. */
    val membershipEnded: Boolean,
)

/** Why a retry stopped. Every one is a state; none is a count. */
internal enum class RetryDropReason {
    /** We are no longer in the group, so nothing we ask can apply. */
    MEMBERSHIP_ENDED,

    /** The effect the request asked for is already true. */
    ALREADY_APPLIED,

    /** The request is itself in the live pending set — it landed. */
    ALREADY_WAITING,

    /**
     * The group has no role table, so a role change has no committer and the
     * core would refuse it `NoRoleTable` — a `DoNotRetry` answer.
     */
    NO_ROLE_TABLE,
}

internal sealed interface RetryVerdict {
    data object Fire : RetryVerdict

    data object Wait : RetryVerdict

    data class Drop(val why: RetryDropReason) : RetryVerdict
}

/**
 * What to do with one armed retry, given the group as it is now.
 *
 * Pure and total. The order is load-bearing: every stopping condition is
 * checked BEFORE the drain test, so a request whose effect already happened is
 * dropped rather than re-sent the moment a sweep lands.
 */
internal fun retryVerdict(armed: ArmedRetry, snapshot: RetryGroupSnapshot): RetryVerdict {
    if (snapshot.membershipEnded) return RetryVerdict.Drop(RetryDropReason.MEMBERSHIP_ENDED)

    // A torn read claims NOTHING — neither that the request applied nor that it
    // is safe to re-send. Fail-closed in the same direction
    // `unappliedAddRequests` takes with the identical signal.
    val roster = snapshot.roster ?: return RetryVerdict.Wait

    val request = armed.request
    if (request.isRoleChange && snapshot.roleTable == null) {
        return RetryVerdict.Drop(RetryDropReason.NO_ROLE_TABLE)
    }
    if (alreadyApplied(request, roster, snapshot.roleTable)) {
        return RetryVerdict.Drop(RetryDropReason.ALREADY_APPLIED)
    }
    if (alreadyWaiting(request, roster, snapshot.pendingKeys)) {
        return RetryVerdict.Drop(RetryDropReason.ALREADY_WAITING)
    }
    return if (snapshot.pendingCount < armed.pendingCountAtRefusal) {
        RetryVerdict.Fire
    } else {
        RetryVerdict.Wait
    }
}

/** Is the effect the request asked for already true of the group? */
private fun alreadyApplied(
    request: RetryableRequest,
    roster: List<RetryRosterMember>,
    roleTable: RetryRoleTable?,
): Boolean {
    fun inRoster(keyB64: String) = roster.any { it.signatureKeyB64 == keyB64 }
    return when (request) {
        is RetryableRequest.Add -> inRoster(request.subjectKeyB64)
        is RetryableRequest.Remove -> !inRoster(request.subjectKeyB64)
        is RetryableRequest.Leave -> !inRoster(request.subjectKeyB64)
        is RetryableRequest.SetAdmin -> {
            val live = roleTable ?: return false
            // The ask is a PREDICATE about one member, not a whole table, so it
            // is satisfied by the live table saying what was asked however it
            // got there — including by somebody else having made the same
            // change.
            live.adminKeysB64.contains(request.subjectKeyB64) == request.makeAdmin
        }
        is RetryableRequest.TransferOwnership -> {
            val live = roleTable ?: return false
            // The outgoing owner's own new role is deliberately NOT part of the
            // test. The ask that matters — and the only half a peer could
            // observe — is who holds the group.
            live.ownerKeyB64 == request.subjectKeyB64
        }
    }
}

/**
 * Is this exact request already in the live pending set?
 *
 * The pending set keys a removal on a LEAF ([proposalKey] has nothing else to
 * key it on), while a retry record keys it on a signature key — so the leaf is
 * resolved through the live roster here rather than stored, which is what keeps
 * a vacated-and-reused leaf from making an unrelated pending removal look like
 * ours.
 */
private fun alreadyWaiting(
    request: RetryableRequest,
    roster: List<RetryRosterMember>,
    pendingKeys: List<String>,
): Boolean {
    val pending = pendingKeys.toSet()
    return when (request) {
        is RetryableRequest.Add -> pending.contains("add:${request.subjectKeyB64}")
        is RetryableRequest.Remove -> pendingRemoval(request.subjectKeyB64, roster, pending)
        is RetryableRequest.Leave -> pendingRemoval(request.subjectKeyB64, roster, pending)
        is RetryableRequest.SetAdmin,
        is RetryableRequest.TransferOwnership,
        -> pending.contains(RetryableRequest.ROLE_CHANGE_KEY)
    }
}

private fun pendingRemoval(
    subjectKeyB64: String,
    roster: List<RetryRosterMember>,
    pending: Set<String>,
): Boolean {
    val leaf =
        roster.firstOrNull { it.signatureKeyB64 == subjectKeyB64 }?.leafIndex ?: return false
    return pending.contains("remove:$leaf") || pending.contains("leave:$leaf")
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/**
 * Armed retries, per `(account, group)` scope key, per request key.
 *
 * In memory on purpose — see the header. Nothing here touches disk.
 */
internal class MembershipRetryStore {
    private val lock = ReentrantLock()
    private val armedByScope = mutableMapOf<String, MutableMap<String, ArmedRetry>>()

    /** Arm (or re-arm) one request. At most one record per [RetryableRequest.key]. */
    fun arm(scope: String, armed: ArmedRetry) {
        lock.withLock {
            armedByScope.getOrPut(scope) { mutableMapOf() }[armed.request.key] = armed
        }
    }

    /**
     * Take the record for [key] out of the store and return it, or `null`.
     *
     * **This is the exactly-once primitive.** Both the retry driver and the
     * manual handler call it, so whichever runs first owns the request and the
     * other finds nothing. Deliberately not a read followed by a removal at
     * some later suspension point: that is the shape that lets two evaluations
     * both decide to fire.
     */
    fun claim(scope: String, key: String): ArmedRetry? =
        lock.withLock {
            val bucket = armedByScope[scope] ?: return@withLock null
            val found = bucket.remove(key)
            if (bucket.isEmpty()) armedByScope.remove(scope)
            found
        }

    /**
     * Every armed record in [scope], for the driver to evaluate. Never mutates
     * — the driver claims what it decides to act on.
     */
    fun armed(scope: String): List<ArmedRetry> =
        lock.withLock { armedByScope[scope]?.values?.toList() ?: emptyList() }

    /** Forget everything for one group — on leaving it, on sign-out. */
    fun clear(scope: String) {
        lock.withLock { armedByScope.remove(scope) }
    }

    companion object {
        /**
         * The scope key. Both halves are required: an account can be a member
         * of a group on two devices, and a device can hold two accounts.
         */
        fun scopeKey(accountIdB64: String, groupIdHex: String): String =
            "$accountIdB64 $groupIdHex"
    }
}
