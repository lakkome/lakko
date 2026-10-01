// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

/**
 * Noticing that this device's DeviceGroup forked from the chain on the
 * Delivery Service and can never catch up by waiting.
 *
 * Mirror of the web and iOS rule of the same name. The case it exists for: a
 * device met a commit it could not apply (its own, which MLS cannot apply)
 * and persisted the OLD epoch with its cursor past the commit. It has read
 * the whole journal, its epoch is N, and the DS chain is at N+1 registered by
 * the account root; nothing that will ever arrive moves it forward.
 *
 * Kept pure so a host JVM test runs it. What the verdict triggers (the
 * re-founding) lives in [DeviceGroupSyncRepository].
 */
internal object DeviceGroupFork {

    /**
     * How long a device may sit drained and behind the DS before it counts as
     * forked. Chosen, not measured: it has to cover the legitimate window in
     * which another device has registered epoch N+1 (ADR-0053: register before
     * publish) and its commit has not reached the journal yet, which is one
     * registration plus one publish, and it is far longer than that on
     * purpose, since a false positive re-founds the DeviceGroup.
     */
    const val FORK_GRACE_MS: Long = 120_000

    /**
     * A poll drained the journal: it returned ZERO rows and no more pages. A
     * fetch that returned rows is never drained, even if none of them applied.
     */
    fun isDrained(messageCount: Int, hasMore: Boolean): Boolean = messageCount == 0 && !hasMore

    /**
     * [localEpoch] is this device's DeviceGroup epoch at the check,
     * [latestEpoch] the newest epoch the DS registered for the journal
     * (`null` when the read did not report one), [suspectSince] the start of
     * the current suspicion or `null`, [nowMs] the wall clock.
     *
     * Not drained, no [latestEpoch], or [latestEpoch] not ahead of
     * [localEpoch]: [ForkVerdict.InSync], which clears the suspicion.
     * Otherwise the suspicion starts now, holds while it is younger than
     * [FORK_GRACE_MS], and past it the verdict is [ForkVerdict.Forked].
     */
    fun forkVerdict(
        localEpoch: ULong,
        drained: Boolean,
        latestEpoch: ULong?,
        suspectSince: Long?,
        nowMs: Long,
    ): ForkVerdict {
        if (!drained || latestEpoch == null || latestEpoch <= localEpoch) return ForkVerdict.InSync
        if (suspectSince == null) return ForkVerdict.Suspect(nowMs)
        if (nowMs - suspectSince > FORK_GRACE_MS) return ForkVerdict.Forked
        return ForkVerdict.Suspect(suspectSince)
    }
}

/** The outcome of [DeviceGroupFork.forkVerdict]. */
internal sealed interface ForkVerdict {
    /** Nothing to suspect; any running suspicion is cleared. */
    data object InSync : ForkVerdict

    /** Drained and behind the DS since [since]; not yet past the grace. */
    data class Suspect(val since: Long) : ForkVerdict

    /** Drained and behind for longer than the grace: re-found. */
    data object Forked : ForkVerdict
}
