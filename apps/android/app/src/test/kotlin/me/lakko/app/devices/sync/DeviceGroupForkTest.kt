// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The fork rule of [DeviceGroupFork]: when a drained DeviceGroup reader that
 * is behind the DS chain counts as forked.
 */
class DeviceGroupForkTest {

    private val grace = DeviceGroupFork.FORK_GRACE_MS

    @Test
    fun an_absent_latest_epoch_is_in_sync() {
        assertEquals(
            ForkVerdict.InSync,
            DeviceGroupFork.forkVerdict(3u, drained = true, latestEpoch = null, suspectSince = 10, nowMs = 10 + grace * 5),
        )
    }

    @Test
    fun a_latest_epoch_at_or_below_the_local_one_is_in_sync() {
        assertEquals(ForkVerdict.InSync, DeviceGroupFork.forkVerdict(3u, true, 3u, null, 0))
        assertEquals(ForkVerdict.InSync, DeviceGroupFork.forkVerdict(3u, true, 2u, null, 0))
    }

    @Test
    fun a_fetch_that_was_not_drained_is_in_sync_even_when_behind() {
        assertEquals(ForkVerdict.InSync, DeviceGroupFork.forkVerdict(3u, false, 9u, 0, grace * 10))
    }

    @Test
    fun a_fetch_with_rows_is_never_drained() {
        assertFalse(DeviceGroupFork.isDrained(messageCount = 1, hasMore = false))
        assertFalse(DeviceGroupFork.isDrained(messageCount = 0, hasMore = true))
        assertFalse(DeviceGroupFork.isDrained(messageCount = 4, hasMore = true))
        assertTrue(DeviceGroupFork.isDrained(messageCount = 0, hasMore = false))
    }

    @Test
    fun the_first_drained_poll_behind_starts_the_suspicion() {
        assertEquals(ForkVerdict.Suspect(1_000), DeviceGroupFork.forkVerdict(3u, true, 4u, null, 1_000))
    }

    @Test
    fun the_suspicion_holds_within_the_grace_and_forks_past_it() {
        val since = 1_000L
        assertEquals(ForkVerdict.Suspect(since), DeviceGroupFork.forkVerdict(3u, true, 4u, since, since + 1))
        assertEquals(ForkVerdict.Suspect(since), DeviceGroupFork.forkVerdict(3u, true, 4u, since, since + grace))
        assertEquals(ForkVerdict.Forked, DeviceGroupFork.forkVerdict(3u, true, 4u, since, since + grace + 1))
    }

    @Test
    fun any_in_sync_poll_clears_the_timer() {
        // A caller keeps `since` only while the verdict is Suspect; replay a
        // sequence the way the poll loop does.
        var since: Long? = null
        fun step(drained: Boolean, latest: ULong?, now: Long): ForkVerdict {
            val v = DeviceGroupFork.forkVerdict(3u, drained, latest, since, now)
            since = (v as? ForkVerdict.Suspect)?.since
            return v
        }
        assertEquals(ForkVerdict.Suspect(0), step(true, 4u, 0))
        assertEquals(ForkVerdict.InSync, step(false, 4u, grace / 2))
        // The suspicion restarted, so the grace counts again from here.
        assertEquals(ForkVerdict.Suspect(grace), step(true, 4u, grace))
        assertEquals(ForkVerdict.Suspect(grace), step(true, 4u, grace * 2))
        assertEquals(ForkVerdict.Forked, step(true, 4u, grace * 2 + 1))
    }
}
