// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/*
 * ADR-0057, the second security pass (finding 1 + LOW 7): repair a journal a
 * lower rank pushed ahead.
 *
 * Every commit registers its next epoch in APPEND mode. A member-rank
 * registrar may append at most one epoch past the newest admin or root
 * registration, which is still enough to put a key at N+1 that no commit on
 * the journal ever reaches (a member that registered and never published).
 * Every device that stays at N then finds its own Append refused (409), and
 * once the fake epoch pushes N out of the write tier its signed requests are
 * refused (401).
 *
 * The owner or an admin repairs it from above:
 *
 *   * Triggers. (a) this device's Append got 409; (b) a signed request of this
 *     device at its current epoch got 401 after every retry the DS client
 *     makes. Either only NOTES the group; a plain member never repairs.
 *   * The check, [JOURNAL_REPAIR_DELAY_MS] later (chosen, not measured: long
 *     enough for a real committer's publish to follow its registration). Poll
 *     the group to the journal head; if the group's epoch is still the N noted
 *     (no commit reached N+1 on the journal), send `replaceEpoch(journal, N,
 *     keyAtN, [])`. 204: the fake epoch is gone. 403: an equal or higher rank
 *     registered above, a real epoch this device reaches by polling; stop.
 *
 * Never part of a commit path: the commit that met 409 was abandoned before
 * anything was noted, and the Replace re-registers only the epoch this device
 * already is at. Only conversation groups have a head poller here; the
 * DeviceGroup's and a PublisherGroup's registrars are all the root, so there
 * is no lower rank above to replace.
 *
 * Every step is logged at info level; no key material is logged.
 */

/** How long after a trigger the check runs. Chosen, not measured. */
public const val JOURNAL_REPAIR_DELAY_MS: Long = 30_000L

/** Why a group was noted. */
public enum class RepairReason { APPEND_CONFLICT, REFUSED_AT_HEAD }

/** This device's standing on a journal. */
public enum class JournalRepairRank { ROOT, ADMIN, MEMBER, NONE }

/** The root or an admin registrar repairs; a member never does. */
public fun mayRepair(rank: JournalRepairRank): Boolean =
    rank == JournalRepairRank.ROOT || rank == JournalRepairRank.ADMIN

/** A group's current epoch and the access key registered for it. */
public class EpochKey(public val epoch: ULong, public val key: ByteArray)

/** What one check came to. */
public enum class RepairOutcome {
    /** 204: the epochs above N were dropped. */
    REPLACED,

    /** 403: an equal or higher rank registered above; stop. */
    OUTRANKED,

    /** The group's epoch moved past N (or away from it): nothing to repair. */
    MOVED,

    /** This device holds no admin or root authority (any more). */
    NOT_PERMITTED,

    /** The group is not held here, so its head cannot be read. */
    NO_POLLER,

    /** The head poll failed for a reason other than the journal refusing this
     *  device (offline, 5xx): no Replace on a head that could not be read. */
    POLL_FAILED,

    /** The group's current key could not be derived. */
    NO_KEY,

    /** The Replace failed for any other reason. */
    FAILED,
}

/** What the check needs from the session. */
public interface JournalRepairPorts {
    /** This device's standing on [groupIdHex]'s journal now. */
    public suspend fun rank(groupIdHex: String): JournalRepairRank

    /** `true` when this session holds the group, so its head can be polled. */
    public fun holds(groupIdHex: String): Boolean

    /** Poll the group's journal to its head, applying everything; throws the
     *  DS refusal or the network failure. */
    public suspend fun pollToHead(groupIdHex: String)

    /** The group's current epoch and access key, or `null`. */
    public fun currentEpochKey(groupIdHex: String): EpochKey?

    /** The root or admin-registrar signer the Replace is signed with, or
     *  `null` (a member, or no key on this device). */
    public suspend fun repairSigner(groupIdHex: String): DsStatementSigning?

    /** The statement PUT. */
    public suspend fun put(make: () -> DsStatement)
}

/** The HTTP status anywhere in a wrapped error chain, or `null`. */
public fun dsStatusDeep(error: Throwable?): Int? {
    var current = error
    var depth = 0
    while (current != null && depth < 8) {
        if (current is DsHttpError) return current.status
        current = current.cause
        depth += 1
    }
    return null
}

/**
 * The per-session repair scheduler. [note] schedules at most one check per
 * group; the check runs on [scope] after [delayMs].
 */
public class JournalRepair(
    private val scope: CoroutineScope,
    private val ports: JournalRepairPorts,
    private val delayMs: Long = JOURNAL_REPAIR_DELAY_MS,
    private val log: (String) -> Unit = {},
) {
    private val pending = ConcurrentHashMap<String, Job>()

    /**
     * Note a trigger at [epoch] (the epoch this device is at). Schedules one
     * check [delayMs] later unless one is pending for the group, the session
     * does not hold it, or [rank] (when the caller already knows it) is not
     * root or admin. A note without a rank is re-checked before any request.
     * Never throws.
     */
    public fun note(
        groupIdHex: String,
        journalId: ByteArray,
        epoch: ULong,
        reason: RepairReason,
        rank: JournalRepairRank? = null,
    ): Boolean {
        if (rank != null && !mayRepair(rank)) return false
        if (!ports.holds(groupIdHex)) return false
        val key = groupIdHex.lowercase()
        if (pending.containsKey(key)) return false
        val id = journalId.copyOf()
        val job = scope.launch(start = kotlinx.coroutines.CoroutineStart.LAZY) {
            try {
                delay(delayMs)
                check(groupIdHex, id, epoch)
            } finally {
                pending.remove(key)
            }
        }
        if (pending.putIfAbsent(key, job) != null) {
            job.cancel()
            return false
        }
        log("$reason at epoch $epoch; checking in ${delayMs / MS_PER_SECOND} s")
        job.start()
        return true
    }

    /** `true` while a check is scheduled or running for the group. */
    public fun isPending(groupIdHex: String): Boolean = pending.containsKey(groupIdHex.lowercase())

    /** Cancel every scheduled check (session end). */
    public fun cancelAll() {
        for (job in pending.values) job.cancel()
        pending.clear()
    }

    /**
     * The check itself: stop at once for a member; poll to the head; Replace at
     * [epoch] only when the group is still there. Never throws except on
     * cancellation.
     */
    public suspend fun check(groupIdHex: String, journalId: ByteArray, epoch: ULong): RepairOutcome {
        if (!mayRepair(safeRank(groupIdHex))) return RepairOutcome.NOT_PERMITTED
        if (!ports.holds(groupIdHex)) {
            log("the group is not held; not repaired")
            return RepairOutcome.NO_POLLER
        }
        try {
            ports.pollToHead(groupIdHex)
            log("polled to the journal head")
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (error: Throwable) {
            val status = dsStatusDeep(error)
            if (status != HTTP_UNAUTHORIZED) {
                log("head poll failed (${status ?: "network"}); not repaired")
                return RepairOutcome.POLL_FAILED
            }
            log("head poll refused (401)")
        }
        val current = try {
            ports.currentEpochKey(groupIdHex)
        } catch (_: Throwable) {
            null
        } ?: return RepairOutcome.NO_KEY
        if (current.epoch != epoch) {
            log("the group moved from epoch $epoch to ${current.epoch}; nothing to repair")
            return RepairOutcome.MOVED
        }
        if (!mayRepair(safeRank(groupIdHex))) return RepairOutcome.NOT_PERMITTED
        val signer = try {
            ports.repairSigner(groupIdHex)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            null
        } ?: return RepairOutcome.NOT_PERMITTED
        log("replacing at epoch $epoch")
        return try {
            ports.put { signer.replaceEpoch(journalId, epoch, current.key, emptyList()) }
            log("replaced at epoch $epoch; the epochs above are gone")
            RepairOutcome.REPLACED
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (error: Throwable) {
            val status = dsStatusDeep(error)
            if (status == HTTP_FORBIDDEN) {
                log("replace refused (403): an equal or higher rank registered above; stopping")
                RepairOutcome.OUTRANKED
            } else {
                log("replace failed (${status ?: "network"})")
                RepairOutcome.FAILED
            }
        }
    }

    private suspend fun safeRank(groupIdHex: String): JournalRepairRank =
        try {
            ports.rank(groupIdHex)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            JournalRepairRank.NONE
        }

    private companion object {
        const val HTTP_UNAUTHORIZED: Int = 401
        const val HTTP_FORBIDDEN: Int = 403
        const val MS_PER_SECOND: Long = 1_000L
    }
}
