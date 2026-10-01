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
 * ## Approving an external join, and why it took a core change to get here
 *
 * It could not participate at all until `21f6c8c`, and not for a reason any
 * client could work around: `Group::approve_external_join` re-wrapped its inner
 * failure as `Error::Group(format!("external join approve refused: {e}"))`, and
 * `Error::Group` carries no `RefusalReason` — so an approve refused because the
 * pending set was full arrived with no reason at all, indistinguishable from a
 * socket failure. An absent reason never retries, so it did not. Inferring the
 * reason from message text would have been the exact drift class
 * `MembershipRefusal.kt` exists to end.
 *
 * The core now propagates the typed error and wraps only a reasonless one, so
 * the approve joins the rule on the same terms as every other kind: the port
 * decides whether it arms, and a drained queue decides when it fires.
 *
 * It brings **two** stopping conditions of its own, and the second is the one
 * that is easy to get wrong:
 *
 *   - the joiner is now a member. Someone else's approve was swept, or our own
 *     was — either way the ask is satisfied, and it reads off the roster like
 *     every other `ALREADY_APPLIED` test.
 *   - the request has left the **retained quarantine**
 *     (`Group.pendingExternalJoins()`). This is [RetryDropReason.JOIN_REQUEST_GONE],
 *     and it is checked rather than assumed because the quarantine and this
 *     store are two different in-memory lifetimes: the quarantine lives on the
 *     live `Group` handle and is emptied by anything that reloads it from a
 *     snapshot, while this store lives as long as the view model. So the record
 *     CAN outlive the request that justified it, within one session, with no
 *     restart. **Handled, not prevented** — the verdict reads the live set and
 *     drops, rather than firing an approve the core would refuse
 *     `NO_SUCH_JOIN_REQUEST`.
 *
 * And it must never arm for a request the quarantine did not keep in the first
 * place. That rule already exists as [externalJoinAffordance], so
 * [armedApproveJoinRetry] **asks** it rather than restating `!retained` — one
 * definition of "this request is actionable", which the panel and the retry
 * both read.
 *
 * ## Discrimination MEASURED on the approve arm
 *
 * Each was introduced as a deliberate regression, the suite was run, the named
 * test observed to fail, and the probe reverted:
 *
 *   1. drop the affordance guard -> `arms exactly when the affordance says the
 *      request is actionable`
 *   2. remove the retained-quarantine check -> `drops an approve once the
 *      request has left the retained quarantine` + `claims nothing on a torn
 *      read of the retained set`
 *   3. treat a torn retained read as an empty quarantine -> `claims nothing on
 *      a torn read of the retained set`
 *   4. stop sharing the `Add` arm of `alreadyApplied` -> `drops an approve once
 *      the joiner is a member`
 *   5. stop sharing the `Add` arm of `alreadyWaiting` -> `drops an approve once
 *      an Add for the joiner is already waiting`
 *   6. give the approve the `add:` store slot -> `keeps the approve slot apart
 *      from an add of the same person`
 *   7. drop `ApproveJoin` from `needsRoleTable` -> `drops an approve in a group
 *      that has lost its role table`
 *   8. hard-code the retryable reason instead of asking the port -> `the
 *      approve arm follows the port even when the port is wrong`
 *   9. apply the retained gate to every kind -> `does not read the retained set
 *      for any other kind of request`
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

    /**
     * Approving a retained external-join request (ADR-0027).
     *
     * **One field, doing two jobs at once** — a property of the core rather
     * than a shortcut here: the joiner's signature key IS the `proposal_ref`
     * handle (`Group.pendingExternalJoins()` returns exactly these keys, and
     * `approveExternalJoin` looks the request up by matching one). There is no
     * separate proposal reference to carry, and — unlike [Add] — no account id
     * either, because the joiner's KeyPackage is already retained in the
     * quarantine. That is what the quarantine is for.
     *
     * The store slot is deliberately NOT the `add:` one, even though an
     * approve resolves to an `Add` proposal: they are different operations
     * reached by different affordances, so a manual approve tap must disarm
     * the approve record and leave an armed add alone, and vice versa.
     */
    data class ApproveJoin(val subjectKeyB64: String) : RetryableRequest {
        override val key: String get() = "approve-join:$subjectKeyB64"
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

/**
 * Which requests the core refuses outright in a group with no role table.
 *
 * A role change has no committer to sweep it; an approve is refused by
 * `role_table_or_refuse` before it looks at anything else. Both are
 * `NO_ROLE_TABLE`, a `DoNotRetry` answer, so waiting for a drain would wait
 * forever.
 */
private val RetryableRequest.needsRoleTable: Boolean
    get() =
        this is RetryableRequest.SetAdmin ||
            this is RetryableRequest.TransferOwnership ||
            this is RetryableRequest.ApproveJoin

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

/**
 * Arm a retry for a refused **external-join approve**, or `null`.
 *
 * Everything [armedRetryFor] decides is decided the same way here; the one
 * addition is the [retained] gate, and it is delegated to
 * [externalJoinAffordance] rather than written as `if (!retained)`. That
 * function already owns the question *may an approve be offered for this
 * request at all* — the members panel asks it before rendering a button — and
 * an automatic re-send is the same question asked later. Two copies of it could
 * disagree; one cannot.
 *
 * A request the quarantine never kept has nothing for `approveExternalJoin` to
 * look up, so a retry for it could only ever be refused `NO_SUCH_JOIN_REQUEST`.
 * In production the caller reads [retained] LIVE off `pendingExternalJoins()`
 * at the moment of the refusal, so this is a real read rather than a constant
 * `true`: a quarantine emptied between the tap and the refusal arms nothing.
 */
internal fun armedApproveJoinRetry(
    error: Throwable,
    subjectKeyB64: String,
    pendingCountAtRefusal: Int,
    retained: Boolean,
    explicitReason: FfiRefusalReason? = null,
    retryMaySucceed: (FfiRefusalReason) -> Boolean,
): ArmedRetry? {
    if (externalJoinAffordance(retained) !is ExternalJoinAffordance.Actionable) return null
    return armedRetryFor(
        error = error,
        request = RetryableRequest.ApproveJoin(subjectKeyB64),
        pendingCountAtRefusal = pendingCountAtRefusal,
        explicitReason = explicitReason,
        retryMaySucceed = retryMaySucceed,
    )
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
    /**
     * ADR-0027 — the joiner key of every request the core still holds in its
     * retained external-join quarantine (`pendingExternalJoins()`).
     *
     * `null` is a TORN read, not an empty quarantine, and claims nothing — the
     * same rule [roster] follows, and for the same reason: an empty answer here
     * would look exactly like "the request is gone" and would drop a live
     * retry.
     *
     * Read ONLY for a [RetryableRequest.ApproveJoin]. A quarantine says nothing
     * about an add, a removal or a role change, so it must never stop one.
     */
    val retainedJoinKeys: List<String>?,
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
     * The group has no role table, so the operation has no committer and the
     * core would refuse it `NoRoleTable` — a `DoNotRetry` answer.
     */
    NO_ROLE_TABLE,

    /**
     * ADR-0027 — the core no longer holds this external-join request, so an
     * approve has nothing to look up and would be refused
     * `NO_SUCH_JOIN_REQUEST`.
     */
    JOIN_REQUEST_GONE,
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
    // `supersededVerdict` takes with the identical signal.
    val roster = snapshot.roster ?: return RetryVerdict.Wait

    val request = armed.request
    if (request.needsRoleTable && snapshot.roleTable == null) {
        return RetryVerdict.Drop(RetryDropReason.NO_ROLE_TABLE)
    }
    if (alreadyApplied(request, roster, snapshot.roleTable)) {
        return RetryVerdict.Drop(RetryDropReason.ALREADY_APPLIED)
    }
    if (alreadyWaiting(request, roster, snapshot.pendingKeys)) {
        return RetryVerdict.Drop(RetryDropReason.ALREADY_WAITING)
    }
    // ADR-0027 — LAST of the stopping conditions, so the two that describe the
    // world (the joiner landed / somebody's Add is waiting) are reported ahead
    // of the one that describes only our own core's bookkeeping. Both of those
    // also empty the quarantine eventually, and "they are already in" is the
    // more useful thing to have said.
    if (request is RetryableRequest.ApproveJoin) {
        val retained = snapshot.retainedJoinKeys ?: return RetryVerdict.Wait
        if (!retained.contains(request.subjectKeyB64)) {
            return RetryVerdict.Drop(RetryDropReason.JOIN_REQUEST_GONE)
        }
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
        // An approve asks for exactly one thing — that this person be in the
        // group — so it is satisfied by them being in it, whoever's approve was
        // swept. Sharing the `Add` answer is the statement, not a saving.
        is RetryableRequest.ApproveJoin -> inRoster(request.subjectKeyB64)
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
        // ADR-0045 S7 — an approved external join IS an ordinary admin-authored
        // `Add` carrying the joiner's retained KeyPackage, so it lands in the
        // pending set under exactly the key an add of the same person would.
        // That is the point of S7 (indistinguishable from an ordinary add on
        // the wire) and it is why the same lookup answers for both.
        is RetryableRequest.ApproveJoin -> pending.contains("add:${request.subjectKeyB64}")
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

// ---------------------------------------------------------------------------
// ADR-0050 section 2.5a point 4 — re-requesting a SUPERSEDED request
// ---------------------------------------------------------------------------

/*
 * A non-admin sweep (ADR-0050 section 2.5a) commits only device admissions and
 * its own account's self-service proposals, and OpenMLS empties the whole
 * proposal store on every merged commit — so any other pending request is
 * DROPPED by that merge, silently. Its author must ask again (UX
 * `proposal-based-membership.md` section 7, "Superseded"). This is the client
 * half, and it reuses the queue-full retry's vocabulary rather than adding a
 * parallel one: the intent IS a [RetryableRequest], and "landed" and "still
 * waiting" are the same [alreadyApplied] / [alreadyWaiting] tests the
 * queue-full verdict uses.
 *
 * Two things differ from the queue-full retry, and both are why this is not
 * simply that retry armed differently:
 *
 *  1. **It is durable.** The queue-full record is a comparison baseline that
 *     means nothing across a restart. An intent is the user's own request,
 *     which the peers may hold or may have dropped while this device was
 *     away, so it lives in the persisted observation row
 *     ([recordOwnRequestIntent]).
 *  2. **The trigger is an observed MERGED COMMIT** ([supersededCheckDue]),
 *     never a timer and never a poll that applied nothing. A request is only
 *     ever superseded by a commit, so nothing else can make the answer change.
 *     A quiet group costs nothing; there is no attempt cap, because every
 *     attempt is paid for by a commit somebody else made.
 *
 * The re-request is RE-DERIVED from the intent against the current epoch,
 * never old bytes re-sent: an add fetches the KeyPackage again; a removal
 * resolves the subject's CURRENT leaf by signature key (OpenMLS reuses a
 * vacated leaf); a role change is recomputed as the current table plus the
 * delta ([adminsWithDelta]), so it never reverts a change that landed
 * meanwhile; a leave proposes the own leaf's removal again.
 */

/**
 * The intent's wire form inside the observation row: comma-separated, because
 * the row itself is `|`-separated and standard base64 contains neither.
 */
internal fun encodeRequestIntent(intent: RetryableRequest): String = when (intent) {
    is RetryableRequest.Add -> "add,${intent.subjectKeyB64},${intent.subjectAccountIdB64}"
    is RetryableRequest.Remove -> "remove,${intent.subjectKeyB64}"
    is RetryableRequest.Leave -> "leave,${intent.subjectKeyB64}"
    is RetryableRequest.SetAdmin ->
        "set-admin,${intent.subjectKeyB64},${if (intent.makeAdmin) 1 else 0}"
    is RetryableRequest.TransferOwnership ->
        "transfer,${intent.subjectKeyB64},${intent.outgoing.name}"
    is RetryableRequest.ApproveJoin -> "approve-join,${intent.subjectKeyB64}"
}

/** The inverse of [encodeRequestIntent]; `null` for anything it did not write. */
internal fun decodeRequestIntent(raw: String): RetryableRequest? {
    val parts = raw.split(',')
    if (parts.size < 2 || parts[1].isEmpty()) return null
    val key = parts[1]
    return when (parts[0]) {
        "add" -> if (parts.size == 3 && parts[2].isNotEmpty()) RetryableRequest.Add(key, parts[2]) else null
        "remove" -> if (parts.size == 2) RetryableRequest.Remove(key) else null
        "leave" -> if (parts.size == 2) RetryableRequest.Leave(key) else null
        "set-admin" -> when {
            parts.size != 3 -> null
            parts[2] == "1" -> RetryableRequest.SetAdmin(key, true)
            parts[2] == "0" -> RetryableRequest.SetAdmin(key, false)
            else -> null
        }
        "transfer" -> if (parts.size == 3) {
            OutgoingOwner.values().firstOrNull { it.name == parts[2] }
                ?.let { RetryableRequest.TransferOwnership(key, it) }
        } else {
            null
        }
        "approve-join" -> if (parts.size == 2) RetryableRequest.ApproveJoin(key) else null
        else -> null
    }
}

/**
 * Whether the superseded check runs now: only when the group's epoch moved
 * past the last epoch it ran at, i.e. a commit merged in between.
 *
 * [baseline] is the epoch the last check ran at, `null` before the first one in
 * this session; [epochBefore] is the epoch the caller read before applying
 * whatever it applied (a poll batch), used as the baseline when there is none.
 * With neither there is nothing to compare against, so the check does NOT run
 * — it establishes the baseline and waits for the next commit, rather than
 * treating "this device just started" as a commit.
 */
internal fun supersededCheckDue(baseline: ULong?, epochBefore: ULong?, epochNow: ULong): Boolean {
    val since = baseline ?: epochBefore ?: return false
    return epochNow > since
}

/** Why an intent was forgotten without anything being re-sent. */
internal enum class SupersededClearReason {
    /** The effect the request asked for is true of the group. */
    LANDED,

    /** The person the request was about is no longer in the group (a role
     *  change), or no longer publishes the device an add named. */
    SUBJECT_LEFT,

    /** This device is no longer a member, so nothing it asks can apply. */
    MEMBERSHIP_ENDED,
}

/** What the superseded check decides for one own request intent. */
internal sealed interface SupersededVerdict {
    /** Forget the intent; tell the user nothing (the effect is visible, or
     *  there is nothing left to do it to). */
    data class Clear(val why: SupersededClearReason) : SupersededVerdict

    /** Still pending, or the group could not be read: decide at the next
     *  commit. */
    data object Keep : SupersededVerdict

    /** Superseded: re-request it, silently, from the intent. */
    data object ReRequest : SupersededVerdict

    /** It can no longer be re-requested from this side: forget it and list it
     *  under `Not applied`. */
    data object NotApplied : SupersededVerdict
}

/**
 * The superseded verdict for one own request, against the group as it is
 * after a merged commit.
 *
 * Pure and total. The order mirrors [retryVerdict]'s: every reason to stop is
 * checked before the decision to re-send, so a request whose effect already
 * happened is never sent again.
 */
internal fun supersededVerdict(
    intent: RetryableRequest,
    snapshot: RetryGroupSnapshot,
): SupersededVerdict {
    if (snapshot.membershipEnded) {
        return SupersededVerdict.Clear(SupersededClearReason.MEMBERSHIP_ENDED)
    }
    // A torn read claims nothing, in either direction.
    val roster = snapshot.roster ?: return SupersededVerdict.Keep
    if (intent.needsRoleTable && snapshot.roleTable == null) {
        // `NoRoleTable` is a `DoNotRetry` answer: nothing will ever sweep it.
        return SupersededVerdict.NotApplied
    }
    if (alreadyApplied(intent, roster, snapshot.roleTable)) {
        return SupersededVerdict.Clear(SupersededClearReason.LANDED)
    }
    // A role change about somebody who has left has nobody to apply to.
    val subjectMustStay = intent is RetryableRequest.SetAdmin ||
        intent is RetryableRequest.TransferOwnership
    if (subjectMustStay && roster.none { it.signatureKeyB64 == intent.subjectKeyB64() }) {
        return SupersededVerdict.Clear(SupersededClearReason.SUBJECT_LEFT)
    }
    if (alreadyWaiting(intent, roster, snapshot.pendingKeys)) return SupersededVerdict.Keep
    if (intent is RetryableRequest.ApproveJoin) {
        val retained = snapshot.retainedJoinKeys ?: return SupersededVerdict.Keep
        // A successful approve CONSUMED the retained request, so the KeyPackage
        // to re-approve from exists only if the joiner asked again. If it did
        // not, only the joiner holds the material: terminal on this side.
        if (!retained.contains(intent.subjectKeyB64)) return SupersededVerdict.NotApplied
    }
    return SupersededVerdict.ReRequest
}

/** The subject signature key of any intent. */
internal fun RetryableRequest.subjectKeyB64(): String = when (this) {
    is RetryableRequest.Add -> subjectKeyB64
    is RetryableRequest.Remove -> subjectKeyB64
    is RetryableRequest.Leave -> subjectKeyB64
    is RetryableRequest.SetAdmin -> subjectKeyB64
    is RetryableRequest.TransferOwnership -> subjectKeyB64
    is RetryableRequest.ApproveJoin -> subjectKeyB64
}

/** What to do with an intent whose re-request threw. */
internal enum class ReRequestFailure {
    /** Keep the intent; the next merged commit decides again. */
    KEEP,

    /** The core refused it for its own state or for the author's authority:
     *  forget it and list it under `Not applied`. */
    NOT_APPLIED,
}

/**
 * Read a failed re-request through the core's own refusal disposition.
 *
 *  - a refusal that may succeed once a committer has swept (`RetryLater`) is
 *    kept: the next merged commit IS that sweep, which is exactly the drain
 *    the queue-full retry waits for, so keeping the intent hands it to the same
 *    rule rather than arming a second record beside it;
 *  - a refusal that will not (`DoNotRetry`, `NotPermitted` — the author lost
 *    the role the request needs, the request is wrong for the group's state)
 *    stops it, and the user is told under `Not applied`;
 *  - anything that is not a refusal (a dead socket, a directory outage) is
 *    kept: nothing about the request changed.
 *
 * [retryMaySucceed] is the core's port, injected so the tests prove this asks.
 */
internal fun reRequestFailure(
    error: Throwable,
    retryMaySucceed: (FfiRefusalReason) -> Boolean,
): ReRequestFailure {
    val reason = membershipRefusalReason(error) ?: return ReRequestFailure.KEEP
    return if (retryMaySucceed(reason)) ReRequestFailure.KEEP else ReRequestFailure.NOT_APPLIED
}

/** The `Not applied` row for an intent that stopped. */
internal fun unappliedFor(intent: RetryableRequest): UnappliedRequest = when (intent) {
    is RetryableRequest.Add -> UnappliedRequest(intent.subjectKeyB64, UnappliedKind.ADD)
    is RetryableRequest.ApproveJoin ->
        UnappliedRequest(intent.subjectKeyB64, UnappliedKind.JOIN_APPROVE)
    is RetryableRequest.Remove -> UnappliedRequest(intent.subjectKeyB64, UnappliedKind.REMOVE)
    is RetryableRequest.Leave -> UnappliedRequest(intent.subjectKeyB64, UnappliedKind.LEAVE)
    is RetryableRequest.SetAdmin -> UnappliedRequest(
        intent.subjectKeyB64,
        if (intent.makeAdmin) UnappliedKind.MAKE_ADMIN else UnappliedKind.REMOVE_ADMIN,
    )
    is RetryableRequest.TransferOwnership ->
        UnappliedRequest(intent.subjectKeyB64, UnappliedKind.TRANSFER_OWNERSHIP)
}

/**
 * A role change re-derived as a DELTA over the table as it is now: [current]
 * with [subject] added to or removed from the admin set, and nothing else
 * touched.
 *
 * This is the rule the promote / demote path builds its proposal from, and it is
 * what makes a re-request safe: re-sending the full table the user saw would
 * revert every change another admin made in between.
 */
internal fun adminsWithDelta(
    current: List<ByteArray>,
    subject: ByteArray,
    makeAdmin: Boolean,
): List<ByteArray> {
    val admins = current.filterNot { it.contentEquals(subject) }.toMutableList()
    if (makeAdmin) admins.add(subject)
    return admins
}

/** What one superseded pass did, for the caller's log and its follow-up sweep. */
internal data class SupersededPassResult(
    val reRequested: Int,
    val cleared: Int,
    val notApplied: Int,
    val kept: Int,
)

/**
 * One superseded pass over every own request intent of one group, after a
 * merged commit. The decisions are [supersededVerdict] and [reRequestFailure];
 * this is the loop that applies them, written here rather than in the
 * repository so the host suite can run it without an FFI group or a network.
 *
 * [reRequest] re-issues one intent through the user path and answers `false`
 * when the subject is gone (no leaf holds the key, the account no longer lists
 * the device); it throws what the propose path throws. Never called for an
 * intent the verdict did not mark for re-request, so a quiet group, a landed
 * request or a pending one costs no network at all.
 */
internal suspend fun runSupersededPass(
    snapshot: RetryGroupSnapshot,
    store: PendingRequestStore,
    notApplied: UnappliedRequestStore,
    accountIdB64: String,
    groupIdHex: String,
    reRequest: suspend (RetryableRequest) -> Boolean,
    retryMaySucceed: (FfiRefusalReason) -> Boolean,
): SupersededPassResult {
    var reRequested = 0
    var cleared = 0
    var stopped = 0
    var kept = 0
    fun stop(intent: RetryableRequest) {
        recordUnapplied(notApplied, accountIdB64, groupIdHex, listOf(unappliedFor(intent)))
        forgetOwnRequestIntent(store, accountIdB64, groupIdHex, intent)
        stopped += 1
    }
    for (intent in loadOwnRequestIntents(store, accountIdB64, groupIdHex)) {
        when (supersededVerdict(intent, snapshot)) {
            SupersededVerdict.Keep -> kept += 1
            is SupersededVerdict.Clear -> {
                forgetOwnRequestIntent(store, accountIdB64, groupIdHex, intent)
                cleared += 1
            }
            SupersededVerdict.NotApplied -> stop(intent)
            SupersededVerdict.ReRequest -> {
                val sent = try {
                    reRequest(intent)
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (cause: Throwable) {
                    when (reRequestFailure(cause, retryMaySucceed)) {
                        ReRequestFailure.KEEP -> kept += 1
                        ReRequestFailure.NOT_APPLIED -> stop(intent)
                    }
                    continue
                }
                if (sent) {
                    reRequested += 1
                } else {
                    forgetOwnRequestIntent(store, accountIdB64, groupIdHex, intent)
                    cleared += 1
                }
            }
        }
    }
    return SupersededPassResult(reRequested, cleared, stopped, kept)
}
