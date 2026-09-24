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


    // ---- Whose clock the running time is (docs/ux/calls.md section 4.5) -------

    /**
     * The defect this pins, measured on an emulator before the fix: the
     * collapsed bar read `0:10`, the call was expanded, and four seconds later
     * the call screen announced "in call, 5 seconds". Section 4.5 says
     * collapsing leaves "the media, the keys and the timer … untouched", so a
     * screen that restarts its clock states something false about the call.
     *
     * Expressed on the seam the screen actually uses: given the call's own
     * connect moment and a view that opened twelve minutes later, the line must
     * read the CALL's age, and reading the VIEW's age must be a different and
     * visibly wrong answer.
     */
    @Test
    fun `a view opened mid-call counts from the call's connect, not from its own start`() {
        val callConnectedAt = 1_000L
        val twelveMinutesIn = callConnectedAt + 12 * 60_000L
        val viewOpenedAt = twelveMinutesIn

        val origin = callClockOrigin(
            callConnectedSinceMs = callConnectedAt,
            viewStateConnectedSinceMs = viewOpenedAt,
        )
        assertEquals("the view's own moment won", callConnectedAt, origin)
        assertEquals(720L, callElapsedSecs(origin, twelveMinutesIn))
        assertEquals("12:00", formatCallDuration(callElapsedSecs(origin, twelveMinutesIn)!!))

        // What the defect produced, kept here as the contrast: counting from the
        // view's own moment reads a twelve-minute call as brand new.
        assertEquals(0L, callElapsedSecs(viewOpenedAt, twelveMinutesIn))
    }

    /**
     * The property the maintainer measured: two views of ONE call, opened at
     * different moments, must agree about that call. A collapse and an expand is
     * exactly this — the second view is a fresh composable over the same call.
     */
    @Test
    fun `two views of one call opened at different moments agree on its elapsed`() {
        val callConnectedAt = 0L
        // The bar, ten seconds in.
        val bar = callClockOrigin(callConnectedAt, viewStateConnectedSinceMs = 10_000L)
        // The screen, expanded four seconds after that.
        val screen = callClockOrigin(callConnectedAt, viewStateConnectedSinceMs = 14_000L)

        assertEquals(10L, callElapsedSecs(bar, 10_000L))
        assertEquals(14L, callElapsedSecs(screen, 14_000L))
        // And at any single instant the two surfaces read identically, which is
        // the thing a person can hold both of at once.
        assertEquals(callElapsedSecs(bar, 14_000L), callElapsedSecs(screen, 14_000L))
    }

    /**
     * A view's own latch is the FALLBACK and only that, so a screen with no call
     * behind it (the mock design route) still has a clock.
     */
    @Test
    fun `a view with no call behind it falls back to its own latch`() {
        assertEquals(500L, callClockOrigin(callConnectedSinceMs = null, viewStateConnectedSinceMs = 500L))
        assertEquals(null, callClockOrigin(callConnectedSinceMs = null, viewStateConnectedSinceMs = null))
    }

    /** A call that has not connected has no running time, so the line keeps its
     *  state word instead of showing a clock at zero. */
    @Test
    fun `a call that has not connected has no running time`() {
        assertEquals(null, callElapsedSecs(null, 5_000L))
        assertEquals("Connected", status(connected = true, elapsedSecs = callElapsedSecs(null, 5_000L)))
    }

    /** Whole seconds, floored: a clock must never read ahead of itself. */
    @Test
    fun `the elapsed is floored to whole seconds`() {
        assertEquals(0L, callElapsedSecs(0L, 999L))
        assertEquals(1L, callElapsedSecs(0L, 1_000L))
        assertEquals(1L, callElapsedSecs(0L, 1_999L))
    }

    /** A host clock corrected backwards mid-call reads `0:00`, not a negative
     *  number that [formatCallDuration] would turn into no duration at all. */
    @Test
    fun `a clock that moved backwards does not take the duration away`() {
        assertEquals(0L, callElapsedSecs(10_000L, 4_000L))
        assertEquals("0:00", formatCallDuration(callElapsedSecs(10_000L, 4_000L)!!))
    }
}
