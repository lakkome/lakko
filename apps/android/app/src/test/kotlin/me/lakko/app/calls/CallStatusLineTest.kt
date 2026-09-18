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

    @Suppress("LongParameterList")
    private fun status(
        calling: Boolean = false,
        connecting: Boolean = false,
        connected: Boolean = false,
        reconnecting: Boolean = false,
        ended: Boolean = false,
        elapsedSecs: Long? = null,
    ) = callStatusText(calling, connecting, connected, reconnecting, ended, elapsedSecs)

    @Test
    fun `each state has its word`() {
        assertEquals("Ringing...", status())
        assertEquals("Calling...", status(calling = true))
        assertEquals("Connecting...", status(connecting = true))
        assertEquals("Reconnecting...", status(reconnecting = true))
        assertEquals("Call ended", status(ended = true))
    }

    @Test
    fun `a dialled call that nobody has answered says Calling`() {
        // Section 4.1's state table separates the two waits. Every outgoing
        // state used to read `Connecting...`, so a phone that was merely
        // ringing claimed the media was already being set up.
        assertEquals("Calling...", status(calling = true))
    }

    @Test
    fun `connecting outranks calling`() {
        // Once the callee has picked up the call really is being set up, and
        // that is the more specific fact.
        assertEquals("Connecting...", status(calling = true, connecting = true))
    }

    @Test
    fun `a connected call never falls back to Calling`() {
        assertEquals("12:04", status(calling = true, connected = true, elapsedSecs = 724))
        assertEquals("Call ended", status(calling = true, ended = true, elapsedSecs = null))
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
    fun `an ended call reports how long it lasted`() {
        // Section 4.1's state table: "`Call ended` and the duration". The clock
        // stops — the screen does not keep counting while it dismisses itself —
        // but the number it stopped at is the last thing the line says.
        assertEquals("Call ended, 12:04", status(connected = true, ended = true, elapsedSecs = 724))
    }

    @Test
    fun `a call that never connected ends with no number`() {
        // Never pretend (section 1): an unanswered call has no duration, and
        // "Call ended, 0:00" would be an invented one.
        assertEquals("Call ended", status(calling = true, ended = true, elapsedSecs = null))
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
