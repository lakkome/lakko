// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test

/**
 * The pure keepalive rule of the SFU call session (`SfuKeepalive.kt`). The
 * constants are pinned because they are shared with the iOS and web clients.
 */
class SfuKeepaliveTest {

    private val t0 = 1_800_000_000_000L

    @Test
    fun `the first tick after the interval pings`() {
        assertEquals(
            SfuKeepaliveAction.PING,
            sfuKeepaliveAction(nowMs = t0 + SFU_KEEPALIVE_PING_INTERVAL_MS, lastInboundMs = t0, lastPingMs = t0),
        )
    }

    @Test
    fun `before the interval it waits`() {
        assertEquals(
            SfuKeepaliveAction.WAIT,
            sfuKeepaliveAction(nowMs = t0 + SFU_KEEPALIVE_PING_INTERVAL_MS - 1, lastInboundMs = t0, lastPingMs = t0),
        )
        assertEquals(SfuKeepaliveAction.WAIT, sfuKeepaliveAction(nowMs = t0, lastInboundMs = t0, lastPingMs = t0))
    }

    @Test
    fun `past the deadline the link is dead even when a ping is also due`() {
        val now = t0 + SFU_KEEPALIVE_DEADLINE_MS
        // A ping is due as well (the last one was long ago); DEAD wins.
        assertEquals(SfuKeepaliveAction.DEAD, sfuKeepaliveAction(nowMs = now, lastInboundMs = t0, lastPingMs = t0))
        // And still dead when a ping was sent only just now.
        assertEquals(SfuKeepaliveAction.DEAD, sfuKeepaliveAction(nowMs = now, lastInboundMs = t0, lastPingMs = now))
    }

    @Test
    fun `an inbound frame resets the deadline`() {
        val later = t0 + SFU_KEEPALIVE_DEADLINE_MS + SFU_KEEPALIVE_PING_INTERVAL_MS
        // Without the inbound frame this would be DEAD.
        assertEquals(SfuKeepaliveAction.DEAD, sfuKeepaliveAction(nowMs = later, lastInboundMs = t0, lastPingMs = t0))
        // A Pong shortly before `later` keeps the link alive: a ping is due, not a death.
        val freshInbound = later - 1_000L
        val action = sfuKeepaliveAction(nowMs = later, lastInboundMs = freshInbound, lastPingMs = t0)
        assertNotEquals(SfuKeepaliveAction.DEAD, action)
        assertEquals(SfuKeepaliveAction.PING, action)
        // And with the ping also recent, it simply waits.
        assertEquals(
            SfuKeepaliveAction.WAIT,
            sfuKeepaliveAction(nowMs = later, lastInboundMs = freshInbound, lastPingMs = freshInbound),
        )
    }

    @Test
    fun `the constants are the ones shared with iOS and web`() {
        assertEquals(15_000L, SFU_KEEPALIVE_PING_INTERVAL_MS)
        assertEquals(45_000L, SFU_KEEPALIVE_DEADLINE_MS)
        assertEquals(5_000L, SFU_KEEPALIVE_TICK_MS)
    }

    // ---- When a call ended -------------------------------------------------

    @Test
    fun `a call ends at the clock while the link was alive within the deadline`() {
        assertEquals(100_000L, callEndedAtMs(nowMs = 100_000L, lastInboundMs = 90_000L))
        assertEquals(100_000L, callEndedAtMs(nowMs = 100_000L, lastInboundMs = 100_000L - SFU_KEEPALIVE_DEADLINE_MS))
    }

    @Test
    fun `a call ends at the last proof of life once the deadline has passed`() {
        // The overnight case: the laptop slept at 22:58 and woke at 07:16; the
        // row must not say 8:20:53.
        val slept = 100_000L
        val woke = slept + 8L * 3_600_000L
        assertEquals(slept, callEndedAtMs(nowMs = woke, lastInboundMs = slept))
        val edge = 100_000L - SFU_KEEPALIVE_DEADLINE_MS - 1L
        assertEquals(edge, callEndedAtMs(nowMs = 100_000L, lastInboundMs = edge))
    }

    @Test
    fun `a call whose link never came up ends at the clock`() {
        assertEquals(100_000L, callEndedAtMs(nowMs = 100_000L, lastInboundMs = 0L))
    }
}
