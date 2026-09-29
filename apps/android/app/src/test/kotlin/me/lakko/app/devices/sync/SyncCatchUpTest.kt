// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0058 section 5 and UX spec chat-folders section 6 — noticing a
 * DeviceGroup growth, naming the members before it, the re-send delay, and the
 * "a write is still waiting" flag. Which prior member re-sends is
 * `shouldSendSnapshot` in `lakko-sync`, tested there.
 */
class SyncCatchUpTest {

    @Test
    fun a_new_leaf_is_a_growth_and_the_prior_members_are_the_old_set() {
        assertEquals(listOf(0u, 2u), SyncCatchUp.priorLeavesIfGrown(setOf(2u, 0u), setOf(0u, 2u, 3u)))
    }

    @Test
    fun no_new_leaf_is_no_growth() {
        assertNull(SyncCatchUp.priorLeavesIfGrown(setOf(0u, 1u), setOf(0u, 1u)))
        assertNull("a removal alone is not a growth", SyncCatchUp.priorLeavesIfGrown(setOf(0u, 1u), setOf(0u)))
    }

    @Test
    fun a_removal_and_an_add_in_one_step_is_a_growth() {
        assertEquals(listOf(0u, 1u), SyncCatchUp.priorLeavesIfGrown(setOf(0u, 1u), setOf(0u, 2u)))
    }

    @Test
    fun an_unread_member_set_is_never_a_growth() {
        assertNull(SyncCatchUp.priorLeavesIfGrown(null, setOf(0u, 1u)))
    }

    @Test
    fun the_resend_delay_stays_within_thirty_seconds() {
        assertEquals(0L, SyncCatchUp.resendDelayMs(0.0))
        assertEquals(15_000L, SyncCatchUp.resendDelayMs(0.5))
        assertEquals(30_000L, SyncCatchUp.resendDelayMs(1.0))
        assertEquals(30_000L, SyncCatchUp.resendDelayMs(7.0))
        assertEquals(0L, SyncCatchUp.resendDelayMs(-1.0))
    }

    @Test
    fun a_failed_publish_raises_the_flag_once_and_persists_it() {
        val persisted = mutableListOf<Boolean>()
        val flag = PendingSyncFlag(initial = false) { persisted += it }
        flag.recordFailure()
        flag.recordFailure()
        assertTrue(flag.pending)
        assertEquals(listOf(true), persisted)
    }

    @Test
    fun a_whole_flush_lowers_the_flag() {
        val persisted = mutableListOf<Boolean>()
        val flag = PendingSyncFlag(initial = false) { persisted += it }
        flag.recordFailure()
        flag.onFlushed(flag.generation)
        assertFalse(flag.pending)
        assertEquals(listOf(true, false), persisted)
    }

    @Test
    fun a_failure_during_a_flush_keeps_the_flag_up() {
        val flag = PendingSyncFlag(initial = false) {}
        flag.recordFailure()
        val started = flag.generation
        flag.recordFailure() // lands while the flush is in flight
        flag.onFlushed(started)
        assertTrue(flag.pending)
    }

    @Test
    fun the_flag_survives_a_restart() {
        val flag = PendingSyncFlag(initial = true) {}
        assertTrue(flag.pending)
        flag.onFlushed(flag.generation)
        assertFalse(flag.pending)
    }

    @Test
    fun the_hlc_never_mints_at_or_below_a_restored_stamp() {
        val ahead = (System.currentTimeMillis().toULong() + 60_000uL) * 1000uL + 5uL
        Hlc.advancePast(ahead)
        assertTrue(Hlc.mint() > ahead)
    }
}
