// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * ADR-0048 §2 — when a member publishes its own profile into a group.
 *
 * Three triggers:
 *
 *  - **joining** a group (after a Welcome join, after founding one): published
 *    at once, because nobody in the group knows this member's name yet;
 *  - **changing** the profile: published at once into every group;
 *  - **the group gaining a member**: republished, because a newcomer cannot
 *    read earlier epochs and would otherwise never learn existing names.
 *
 * The growth republish is THROTTLED and JITTERED per group. Every member sees
 * the same add in the same poll window, so an un-jittered republish would be a
 * synchronised burst of N application messages; a burst of adds (a `/g/` link
 * shared in a busy chat) would multiply it. So per group:
 *
 *  - at most one growth republish per [PROFILE_REPUBLISH_MIN_INTERVAL_MS],
 *    measured from the last publish of ANY trigger into that group;
 *  - each scheduled republish waits a random 0..[PROFILE_REPUBLISH_MAX_JITTER_MS];
 *  - growth while a republish is already scheduled schedules nothing more — the
 *    pending one covers every member added before it fires.
 *
 * A growth republish is only ever DELAYED, never dropped: the newcomer still
 * learns the name, a few seconds later.
 *
 * Pure decisions ([growthRepublishDelayMs], [membershipGrew]) plus a small
 * scheduler over an injected scope, clock and jitter source, so a host test
 * drives it on a virtual clock.
 */

/** Minimum spacing between two profile publishes into one group on growth. */
public const val PROFILE_REPUBLISH_MIN_INTERVAL_MS: Long = 30_000L

/** Upper bound of the random delay added to a growth republish. */
public const val PROFILE_REPUBLISH_MAX_JITTER_MS: Long = 10_000L

/**
 * The delay before a growth republish, or `null` when one is already
 * scheduled for the group (it will cover this growth too).
 *
 * @param lastPublishedAtMs the last publish into the group by any trigger, or
 *   `null` when this device has not published there this session.
 * @param jitterMs the random component, expected in `0..MAX_JITTER`; clamped.
 */
public fun growthRepublishDelayMs(
    nowMs: Long,
    lastPublishedAtMs: Long?,
    alreadyScheduled: Boolean,
    jitterMs: Long,
): Long? {
    if (alreadyScheduled) return null
    val jitter = jitterMs.coerceIn(0L, PROFILE_REPUBLISH_MAX_JITTER_MS)
    val throttle = if (lastPublishedAtMs == null) {
        0L
    } else {
        (lastPublishedAtMs + PROFILE_REPUBLISH_MIN_INTERVAL_MS - nowMs).coerceAtLeast(0L)
    }
    return throttle + jitter
}

/**
 * Did the roster GROW between two snapshots of member signature keys?
 *
 * Judged as a set difference over keys, never counts: a remove-and-add in one
 * batch leaves the count unchanged but still brings in a newcomer who needs
 * every name. A torn read on either side (`null`) or an empty baseline is not
 * growth — this device cannot tell who is new, and a republish on a guess is
 * noise rather than a fix.
 */
public fun membershipGrew(before: Collection<String>?, after: Collection<String>?): Boolean {
    if (before == null || after == null || before.isEmpty()) return false
    val known = before.toHashSet()
    return after.any { it !in known }
}

/** Per-group profile republish scheduler. Thread-safe. */
public class ProfileRepublishScheduler(
    private val scope: CoroutineScope,
    private val nowMs: () -> Long = System::currentTimeMillis,
    private val jitterMs: () -> Long = {
        kotlin.random.Random.nextLong(0L, PROFILE_REPUBLISH_MAX_JITTER_MS + 1)
    },
    /** Publish the own profile into the group; `true` when it went out. */
    private val publish: suspend (groupIdHex: String) -> Boolean,
) {
    private val lock = Any()
    private val lastPublishedAt = HashMap<String, Long>()
    private val scheduled = HashSet<String>()

    /** Publish into [groupIdHex] now (join / founding / profile change). */
    public fun publishNow(groupIdHex: String) {
        scope.launch { runPublish(groupIdHex) }
    }

    /** Publish into every group in [groupIdHexes] now (profile change). */
    public fun publishNowToAll(groupIdHexes: Collection<String>) {
        val targets = groupIdHexes.toList()
        scope.launch {
            for (groupIdHex in targets) runPublish(groupIdHex)
        }
    }

    /**
     * The group gained a member: schedule a throttled, jittered republish.
     * @return the delay scheduled, or `null` when one was already pending.
     */
    public fun onGrowth(groupIdHex: String): Long? {
        val delayMs = synchronized(lock) {
            val d = growthRepublishDelayMs(
                nowMs = nowMs(),
                lastPublishedAtMs = lastPublishedAt[groupIdHex],
                alreadyScheduled = groupIdHex in scheduled,
                jitterMs = jitterMs(),
            ) ?: return null
            scheduled += groupIdHex
            d
        }
        scope.launch {
            try {
                delay(delayMs)
            } finally {
                synchronized(lock) { scheduled -= groupIdHex }
            }
            runPublish(groupIdHex)
        }
        return delayMs
    }

    /**
     * ADR-0074 section 3.5a rule 3 - re-send the profile into every group of
     * [groupIdHexes] and WAIT for each: `true` only when none failed, so the
     * caller can keep the re-send owed. A group that took it counts as
     * published for the growth throttle and is reported to [onSent].
     */
    public suspend fun publishAllAwaited(
        groupIdHexes: Collection<String>,
        onSent: (String) -> Unit = {},
        attempt: suspend (groupIdHex: String) -> me.lakko.app.reach.ProfileResend,
    ): Boolean =
        me.lakko.app.reach.resendProfileToAll(
            targets = groupIdHexes.toList(),
            onSent = { groupIdHex ->
                synchronized(lock) { lastPublishedAt[groupIdHex] = nowMs() }
                onSent(groupIdHex)
            },
            send = attempt,
        )

    /** Forget all per-group state (account switch / forget device). */
    public fun reset() {
        synchronized(lock) {
            lastPublishedAt.clear()
            scheduled.clear()
        }
    }

    private suspend fun runPublish(groupIdHex: String) {
        val sent = try {
            publish(groupIdHex)
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            false
        }
        if (sent) {
            synchronized(lock) { lastPublishedAt[groupIdHex] = nowMs() }
        }
    }
}
