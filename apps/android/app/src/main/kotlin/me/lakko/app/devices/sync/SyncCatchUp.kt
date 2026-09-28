// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

/**
 * The two small decisions of ADR-0058 section 5 and UX spec section 6 that
 * live on this side of the FFI, kept pure so a host JVM test can run them.
 *
 * Whether THIS device is the one that re-sends is not here: that is
 * `shouldSendSnapshot` in `lakko-sync`, one rule for three clients. What is
 * here is noticing that the DeviceGroup grew and naming who was a member
 * before it did, which is the input that rule takes.
 */
internal object SyncCatchUp {

    /** The longest the growth re-send waits, so a new device's own first
     *  writes are not raced (ADR-0058 section 5.1). */
    const val MAX_RESEND_DELAY_MS: Long = 30_000

    /**
     * The leaves that were members BEFORE a growth, or `null` when the
     * DeviceGroup did not gain a leaf between [before] and [after].
     *
     * `null` for [before] means this device has not observed a member set yet
     * (the handle was just opened), which is never a growth: every leaf would
     * otherwise look new.
     *
     * A removal alone is not a growth, and a removal plus an add in one step
     * is: the new leaf still has nothing, whatever else changed.
     */
    fun priorLeavesIfGrown(before: Set<UInt>?, after: Set<UInt>): List<UInt>? {
        if (before == null) return null
        if (after.none { it !in before }) return null
        return before.sorted()
    }

    /** A delay in `[0, MAX_RESEND_DELAY_MS]` from a uniform `[0, 1)` draw. */
    fun resendDelayMs(draw: Double): Long {
        val clamped = draw.coerceIn(0.0, 1.0)
        return (clamped * MAX_RESEND_DELAY_MS).toLong().coerceIn(0L, MAX_RESEND_DELAY_MS)
    }
}

/**
 * "Something this device wrote has not reached the account's other devices
 * yet" - the one quiet line of UX spec section 6.
 *
 * A write that failed to publish (offline, the DeviceGroup not open yet)
 * raises it; the next successful catch-up publish lowers it. The catch-up is
 * the whole folded state with every original stamp, so it carries every write
 * that failed, however many, and a restart in between loses nothing as long
 * as the flag itself survived - which is why every change is handed to
 * [persist].
 */
internal class PendingSyncFlag(initial: Boolean, private val persist: (Boolean) -> Unit) {
    @Volatile
    var pending: Boolean = initial
        private set

    private fun raise() {
        if (pending) return
        pending = true
        persist(true)
    }

    /** The catch-up publish went out whole. A flush that started before a
     *  newer failure must not clear it, so the caller passes the generation
     *  it read when the flush started. */
    @Synchronized
    fun onFlushed(startedAtGeneration: Long) {
        if (!pending || startedAtGeneration != generation) return
        pending = false
        persist(false)
    }

    /** Bumped on every failure, so a flush can tell whether a failure landed
     *  while it was in flight. */
    @Volatile
    var generation: Long = 0
        private set

    /** A publish failed; the write is only on this device. */
    @Synchronized
    fun recordFailure() {
        generation += 1
        raise()
    }
}
