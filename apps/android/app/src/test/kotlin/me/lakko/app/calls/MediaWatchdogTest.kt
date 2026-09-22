// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The call that outlived its peer by 45 minutes.
 *
 * Seen in the browser on 2026-09-21: a 1x1 call ran for three quarters of an
 * hour after the other side had left it, counting time the whole way. The
 * connection state never left CONNECTED - both ends were on one machine, so
 * the candidate pair stayed reachable after the peer stopped taking part. On
 * Android the same call would have run longer still: DISCONNECTED was ignored
 * outright, on the strength of an upper-layer watchdog that matches only
 * Outgoing, Connecting and an accepted Ringing.
 */
class MediaWatchdogTest {

    @Test
    fun `the first sample starts the clock`() {
        assertEquals(
            MediaWatchdog.InboundActivity(packets = 100, lastChangeAtMs = 5_000),
            MediaWatchdog.note(null, 100, 5_000),
        )
    }

    @Test
    fun `a moving count restarts the clock`() {
        val first = MediaWatchdog.note(null, 100, 5_000)
        assertEquals(
            MediaWatchdog.InboundActivity(packets = 140, lastChangeAtMs = 7_000),
            MediaWatchdog.note(first, 140, 7_000),
        )
    }

    @Test
    fun `a standing count leaves the clock alone`() {
        val first = MediaWatchdog.note(null, 100, 5_000)
        assertEquals(5_000L, MediaWatchdog.note(first, 100, 9_000).lastChangeAtMs)
    }

    @Test
    fun `a count that went backwards is activity, not silence`() {
        // A reset means the receivers were replaced (renegotiation, ICE
        // restart). Reading it as silence would end a call that is working.
        val first = MediaWatchdog.note(null, 90_000, 5_000)
        assertEquals(
            MediaWatchdog.InboundActivity(packets = 12, lastChangeAtMs = 6_000),
            MediaWatchdog.note(first, 12, 6_000),
        )
    }

    @Test
    fun `silence for the whole window ends the call`() {
        val a = MediaWatchdog.note(null, 100, 0)
        assertTrue(MediaWatchdog.hasGoneSilent(a, 30_000, 30_000))
        assertFalse(MediaWatchdog.hasGoneSilent(a, 29_999, 30_000))
    }

    @Test
    fun `nothing measured is never silent`() {
        assertFalse(MediaWatchdog.hasGoneSilent(null, 10_000_000, 30_000))
    }

    @Test
    fun `a timeout of zero disables it rather than firing instantly`() {
        val a = MediaWatchdog.note(null, 100, 0)
        assertFalse(MediaWatchdog.hasGoneSilent(a, 10_000_000, 0))
        assertFalse(MediaWatchdog.hasGoneSilent(a, 10_000_000, -1))
    }

    @Test
    fun `a trickle of media keeps a call alive indefinitely`() {
        // A muted microphone still sends RTP and a 1x1 call always carries an
        // audio section both ways, so any live call keeps this count moving.
        var a = MediaWatchdog.note(null, 0, 0)
        var t = 2_000L
        while (t <= 120_000L) {
            a = MediaWatchdog.note(a, t, t)
            assertFalse(MediaWatchdog.hasGoneSilent(a, t, 30_000))
            t += 2_000L
        }
    }

    @Test
    fun `the 45-minute call would have ended in the first minute`() {
        var a = MediaWatchdog.note(null, 500_000, 0)
        var endedAt: Long? = null
        var t = 2_000L
        while (t <= 45 * 60_000L) {
            a = MediaWatchdog.note(a, 500_000, t)
            if (endedAt == null && MediaWatchdog.hasGoneSilent(a, t, 30_000)) endedAt = t
            t += 2_000L
        }
        assertEquals(30_000L, endedAt)
    }

    @Test
    fun `the shipped windows are the ones the web uses`() {
        assertEquals(30_000L, CallSession.MEDIA_SILENCE_TIMEOUT_MS)
        assertEquals(10_000L, CallSession.ICE_DISCONNECTED_GRACE_MS)
    }
}
