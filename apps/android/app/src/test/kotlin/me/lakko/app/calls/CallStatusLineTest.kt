// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/calls.md` section 4 item 1 — the call screen's status line, which is
 * what the safety number's strip of digits used to occupy.
 */
class CallStatusLineTest {

    private fun status(
        connecting: Boolean = false,
        connected: Boolean = false,
        reconnecting: Boolean = false,
        ended: Boolean = false,
        elapsedSecs: Long? = null,
    ) = callStatusText(connecting, connected, reconnecting, ended, elapsedSecs)

    @Test
    fun `each state has its word`() {
        assertEquals("Ringing...", status())
        assertEquals("Connecting...", status(connecting = true))
        assertEquals("Reconnecting...", status(reconnecting = true))
        assertEquals("Call ended", status(ended = true))
    }

    @Test
    fun `a connected call shows the running time`() {
        assertEquals("12:04", status(connected = true, elapsedSecs = 724))
        assertEquals("0:03", status(connected = true, elapsedSecs = 3))
    }

    @Test
    fun `a connected call with no measurement yet shows no invented clock`() {
        // Section 1: never pretend. A `0:00` the user watches sit still is
        // worse than a word.
        assertEquals("Connected", status(connected = true, elapsedSecs = null))
    }

    @Test
    fun `an ended call stops reporting a time`() {
        assertEquals("Call ended", status(connected = true, ended = true, elapsedSecs = 724))
    }

    @Test
    fun `reconnecting outranks connected`() {
        assertEquals("Reconnecting...", status(connected = true, reconnecting = true, elapsedSecs = 5))
    }

    @Test
    fun `the spoken label announces a duration rather than digits`() {
        val spoken = callStatusSpokenLabel("Anna", "12:04", 724)
        assertTrue(spoken, spoken.startsWith("Anna,"))
        assertTrue(spoken, spoken.contains("12 minutes 4 seconds"))
        assertTrue(spoken, !spoken.contains("12:04"))
    }

    @Test
    fun `a nameless call is still announced`() {
        assertEquals("Call, Ringing...", callStatusSpokenLabel(null, "Ringing...", null))
        assertEquals("Call, Ringing...", callStatusSpokenLabel("  ", "Ringing...", null))
    }
}
