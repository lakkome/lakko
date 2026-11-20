// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.merge

/**
 * How often the app-wide receive sweep reads every group while the app is
 * open (the idle request budget web keeps, 2026-11-06). Short only while
 * something is known to be arriving: a push wake's catch-up window or a call
 * in progress. Otherwise long, longest when push wakes reach this device: the
 * wake brings a message in, the sweep is only a safety net. The open
 * conversation reads its own journal and is not part of the sweep. Every
 * value here is chosen, not measured.
 */
public object ReceiveCadence {
    /** Between sweeps while a wake's window is open or a call is up. */
    public const val ACTIVE_MS: Long = 4_000L

    /** Between sweeps when idle and push wakes reach this device. */
    public const val IDLE_PUSH_MS: Long = 120_000L

    /** Between sweeps when idle and no push wake is known to reach it. */
    public const val IDLE_NO_PUSH_MS: Long = 30_000L

    /** How long a push wake keeps the sweep short. */
    public const val WAKE_WINDOW_MS: Long = 60_000L

    /** The pause before the next sweep. */
    public fun intervalMs(wakeWindowOpen: Boolean, callInProgress: Boolean, pushReachable: Boolean): Long = when {
        wakeWindowOpen || callInProgress -> ACTIVE_MS
        pushReachable -> IDLE_PUSH_MS
        else -> IDLE_NO_PUSH_MS
    }

    private val lastWake = MutableStateFlow(0L)

    /** When the last push wake for the signed-in account arrived (wall
     *  clock ms; 0 = none this process). */
    public val lastWakeAtMs: StateFlow<Long> = lastWake.asStateFlow()

    /** A push wake for the signed-in account arrived: the sweep runs at
     *  once and stays short for [WAKE_WINDOW_MS]. */
    public fun noteWake(nowMs: Long = System.currentTimeMillis()) {
        lastWake.value = nowMs
    }

    /**
     * Whether [now] lists a group [swept] did not: a chat this device just
     * joined. Its first messages and its name land within the next minute
     * and may bring no push wake (the device subscribes to the group's pushes
     * only once it holds the routing secret). Walk 7 (2026-11-20): a joined
     * group read "No messages yet" for over 15 s on iOS; the same long idle
     * pause applies here.
     */
    public fun groupsAppeared(swept: Set<String>, now: Collection<String>): Boolean =
        now.any { it !in swept }

    /**
     * The end of the pause between two sweeps: a push wake newer than
     * [wakeBefore], or a group in [groups] that [swept] did not hold. A new
     * group opens the wake window like a wake does, so it is swept at once
     * and the sweep stays short for [WAKE_WINDOW_MS].
     */
    public suspend fun awaitWakeOrNewGroup(
        wakeBefore: Long,
        swept: Set<String>,
        groups: Flow<List<String>>,
        nowMs: () -> Long = System::currentTimeMillis,
    ) {
        merge(
            lastWakeAtMs.filter { it > wakeBefore }.map { },
            groups.filter { groupsAppeared(swept, it) }.map { noteWake(nowMs()) },
        ).first()
    }

    /** Whether a wake's window is open at [nowMs]. */
    public fun wakeWindowOpen(nowMs: Long = System.currentTimeMillis()): Boolean {
        val at = lastWake.value
        return at > 0 && nowMs - at in 0 until WAKE_WINDOW_MS
    }
}
