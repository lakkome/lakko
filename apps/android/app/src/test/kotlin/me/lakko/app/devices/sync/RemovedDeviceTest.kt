// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The removed-device rules of [RemovedDevice] (docs/ux/second-device.md
 * section 3.1): which DeviceGroup reads stop the poll, which allow the fork
 * repair, and the screen's copy. Mirror of the web `removed-device.test.ts`.
 *
 * The host JVM suite has no native library, so the handle's `isActive()` is
 * stood in by a lambda; the real FFI read is what the repository passes.
 */
class RemovedDeviceTest {

    private val active: () -> Boolean = { true }
    private val inactive: () -> Boolean = { false }
    private val unreadable: () -> Boolean = { throw IllegalStateException("internal lock poisoned") }

    @Test
    fun a_failed_read_is_unknown_rather_than_removed() {
        assertEquals(true, RemovedDevice.readActive(active))
        assertEquals(false, RemovedDevice.readActive(inactive))
        assertNull(RemovedDevice.readActive(unreadable))
    }

    @Test
    fun an_inactive_group_is_removed_and_stops_the_poll_loop() {
        val read = RemovedDevice.readActive(inactive)
        assertTrue(RemovedDevice.isRemoved(read))
        assertEquals(PollStep.StopRemoved, RemovedDevice.pollStepAfterBatch(read))
    }

    @Test
    fun an_active_group_is_not_removed_and_keeps_polling() {
        val read = RemovedDevice.readActive(active)
        assertFalse(RemovedDevice.isRemoved(read))
        assertEquals(PollStep.Continue, RemovedDevice.pollStepAfterBatch(read))
    }

    @Test
    fun an_unreadable_group_never_shows_the_removed_screen() {
        val read = RemovedDevice.readActive(unreadable)
        assertFalse(RemovedDevice.isRemoved(read))
        assertEquals(PollStep.Continue, RemovedDevice.pollStepAfterBatch(read))
    }

    @Test
    fun the_fork_repair_runs_only_on_a_group_known_to_be_active() {
        assertTrue(RemovedDevice.forkRepairAllowed(RemovedDevice.readActive(active)))
        assertFalse(RemovedDevice.forkRepairAllowed(RemovedDevice.readActive(inactive)))
        assertFalse(RemovedDevice.forkRepairAllowed(RemovedDevice.readActive(unreadable)))
    }

    @Test
    fun cancellation_is_not_swallowed_as_an_unreadable_group() {
        val cancelled: () -> Boolean = { throw kotlinx.coroutines.CancellationException("cancelled") }
        try {
            RemovedDevice.readActive(cancelled)
            throw AssertionError("expected the cancellation to propagate")
        } catch (_: kotlinx.coroutines.CancellationException) {
            // expected
        }
    }

    @Test
    fun uses_the_spec_copy_exactly() {
        assertEquals("This device was removed", RemovedDevice.TITLE)
        assertEquals(
            "It was removed from your account on another device. New messages no longer reach it. " +
                "Sign out, then sign in again if you want to use it.",
            RemovedDevice.BODY,
        )
        assertEquals("Sign out", RemovedDevice.SIGN_OUT)
    }
}
