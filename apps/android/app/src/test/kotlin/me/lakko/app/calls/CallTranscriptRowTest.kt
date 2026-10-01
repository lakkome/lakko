// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/calls.md` section 6 — what a finished call leaves in the
 * conversation.
 *
 * ## The defect these pin
 *
 * A finished call left no trace at all: the chat looked exactly as it had before
 * the call. These cover the two rules that decide the row — which of the four
 * outcomes a call had, and the words that outcome writes — and, just as much,
 * the honesty rule: a call that never connected is never given a duration.
 */
class CallTranscriptRowTest {
    init {
        // ADR-0061: the words under test are the English catalog's.
        EnglishForTests.install()
    }


    // ---- Outcome -----------------------------------------------------------

    @Test
    fun `a call this device placed is outgoing`() {
        assertEquals(
            CallOutcome.OUTGOING,
            callOutcome(incoming = false, answered = true, declinedHere = false, declinedByPeer = false),
        )
    }

    @Test
    fun `a call this device answered is incoming`() {
        assertEquals(
            CallOutcome.INCOMING,
            callOutcome(incoming = true, answered = true, declinedHere = false, declinedByPeer = false),
        )
    }

    @Test
    fun `an incoming call that was never answered is missed`() {
        assertEquals(
            CallOutcome.MISSED,
            callOutcome(incoming = true, answered = false, declinedHere = false, declinedByPeer = false),
        )
    }

    @Test
    fun `an outgoing call nobody answered is still an outgoing call`() {
        // From the caller's seat the call happened — they placed it. "Missed"
        // belongs to the side that was called.
        assertEquals(
            CallOutcome.OUTGOING,
            callOutcome(incoming = false, answered = true, declinedHere = false, declinedByPeer = false),
        )
    }

    @Test
    fun `a refusal is declined whichever way the call was going`() {
        // Declining here, on an incoming call.
        assertEquals(
            CallOutcome.DECLINED,
            callOutcome(incoming = true, answered = false, declinedHere = true, declinedByPeer = false),
        )
        // The other side declining an outgoing call.
        assertEquals(
            CallOutcome.DECLINED,
            callOutcome(incoming = false, answered = true, declinedHere = false, declinedByPeer = true),
        )
    }

    @Test
    fun `a deliberate refusal outranks a missed call`() {
        // Both are true of the same call — it was incoming and never answered,
        // AND it was declined. The user chose the second, so that is what the
        // row says; "Missed call" would read as an accident.
        assertEquals(
            CallOutcome.DECLINED,
            callOutcome(incoming = true, answered = false, declinedHere = true, declinedByPeer = false),
        )
    }

    // ---- Row text ----------------------------------------------------------

    @Test
    fun `a connected call reads with its duration`() {
        assertEquals(
            "Outgoing call, 12:04",
            callRowText(CallTranscriptEntry(CallOutcome.OUTGOING, video = false, durationSecs = 724)),
        )
        assertEquals(
            "Incoming call, 12:04",
            callRowText(CallTranscriptEntry(CallOutcome.INCOMING, video = true, durationSecs = 724)),
        )
    }

    @Test
    fun `a call that never connected is never given a duration`() {
        assertEquals(
            "Outgoing call",
            callRowText(CallTranscriptEntry(CallOutcome.OUTGOING, video = false, durationSecs = null)),
        )
        assertEquals(
            "Incoming call",
            callRowText(CallTranscriptEntry(CallOutcome.INCOMING, video = false, durationSecs = null)),
        )
    }

    @Test
    fun `missed and declined carry no duration even when one is handed in`() {
        // Neither ever connected, so a duration on one of them would be a
        // number the user could not account for.
        assertEquals(
            "Missed call",
            callRowText(CallTranscriptEntry(CallOutcome.MISSED, video = false, durationSecs = 30)),
        )
        assertEquals(
            "Declined",
            callRowText(CallTranscriptEntry(CallOutcome.DECLINED, video = true, durationSecs = 30)),
        )
    }

    @Test
    fun `video changes the icon, never the words`() {
        val voice = CallTranscriptEntry(CallOutcome.OUTGOING, video = false, durationSecs = 61)
        assertEquals(callRowText(voice), callRowText(voice.copy(video = true)))
    }

    // ---- Duration ----------------------------------------------------------

    @Test
    fun `a duration reads as minutes and seconds`() {
        assertEquals("0:00", formatCallDuration(0))
        assertEquals("0:11", formatCallDuration(11))
        assertEquals("1:00", formatCallDuration(60))
        assertEquals("12:04", formatCallDuration(724))
        assertEquals("59:59", formatCallDuration(3_599))
    }

    @Test
    fun `an hour or more grows an hours field`() {
        assertEquals("1:00:00", formatCallDuration(3_600))
        assertEquals("1:02:04", formatCallDuration(3_724))
        assertEquals("10:00:00", formatCallDuration(36_000))
    }

    @Test
    fun `a negative duration is no duration at all`() {
        assertNull(formatCallDuration(-1))
    }

    @Test
    fun `a call that never connected has no measurable duration`() {
        assertNull(callDurationSecs(connectedAtMs = null, endedAtMs = 1_000L))
    }

    @Test
    fun `a duration is measured from the moment media started`() {
        assertEquals(724L, callDurationSecs(connectedAtMs = 1_000L, endedAtMs = 1_000L + 724_000L))
        // Sub-second tails truncate rather than round up, so a call can never
        // report a second it did not last.
        assertEquals(0L, callDurationSecs(connectedAtMs = 1_000L, endedAtMs = 1_999L))
    }

    @Test
    fun `a clock that moved backwards yields no duration`() {
        assertNull(callDurationSecs(connectedAtMs = 5_000L, endedAtMs = 1_000L))
    }

    // ---- Spoken label ------------------------------------------------------

    @Test
    fun `the spoken label says the duration rather than spelling the digits`() {
        val spoken = callRowSpokenLabel(
            CallTranscriptEntry(CallOutcome.OUTGOING, video = false, durationSecs = 724),
        )
        assertTrue(spoken, spoken.contains("12 minutes 4 seconds"))
        assertTrue(spoken, !spoken.contains("12:04"))
        assertTrue(spoken, spoken.contains("Call back"))
    }

    @Test
    fun `the spoken label names the kind so nothing rests on the icon`() {
        assertTrue(
            callRowSpokenLabel(CallTranscriptEntry(CallOutcome.MISSED, video = true))
                .contains("video call", ignoreCase = true),
        )
        assertTrue(
            callRowSpokenLabel(CallTranscriptEntry(CallOutcome.MISSED, video = false)).contains("Call"),
        )
    }

    @Test
    fun `a one-second call is read in the singular`() {
        val spoken = callRowSpokenLabel(
            CallTranscriptEntry(CallOutcome.INCOMING, video = false, durationSecs = 1),
        )
        assertTrue(spoken, spoken.contains("1 second"))
        assertTrue(spoken, !spoken.contains("1 seconds"))
    }
}
