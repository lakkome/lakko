// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.chat.ConversationKind
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.ZoneId

/**
 * docs/ux/navigation.md sections 5 and 6.2 — the cross-conversation call index.
 *
 * Two requirements follow from the shape of the list rather than from any
 * platform, and both are what these tests are about: the index holds the PARTS
 * rather than the finished sentence, so it can be sorted and filtered on; and
 * reading it is a read rather than a walk of every transcript, so the codec has
 * to survive whatever a partial write or an older build left behind.
 */
class CallHistoryTest {
    init {
        // ADR-0061: the words under test are the English catalog's.
        EnglishForTests.install()
    }


    private val zone: ZoneId = ZoneId.of("UTC")

    private fun entry(
        id: String = "aa",
        kind: ConversationKind = ConversationKind.Chat,
        outcome: CallOutcome = CallOutcome.OUTGOING,
        video: Boolean = false,
        at: Long = 1_000L,
        duration: Long? = null,
    ) = CallHistoryEntry(id, kind, outcome, video, at, duration)

    // ---------------------------------------------------------------- codec

    @Test
    fun `a record round trips every part it carries`() {
        for (outcome in CallOutcome.entries) {
            for (kind in ConversationKind.entries) {
                val source = entry(
                    id = "0f1e",
                    kind = kind,
                    outcome = outcome,
                    video = outcome == CallOutcome.MISSED,
                    at = 1_700_000_000_000L,
                    duration = if (outcome == CallOutcome.INCOMING) 724L else null,
                )
                assertEquals(source, decodeCallHistoryEntry(encodeCallHistoryEntry(source)))
            }
        }
    }

    @Test
    fun `the index stores the parts, so the words come from the one function`() {
        // A row that stored "Missed call" could be sorted on neither the
        // outcome nor the duration. The words are derived, by the same
        // function the conversation timeline uses.
        val missed = entry(outcome = CallOutcome.MISSED)
        assertEquals("Missed call", callRowText(missed.toTranscriptEntry()))
        val connected = entry(outcome = CallOutcome.OUTGOING, duration = 724L)
        assertEquals("Outgoing call, 12:04", callRowText(connected.toTranscriptEntry()))
    }

    @Test
    fun `a line this build cannot read drops out rather than throwing`() {
        val good = encodeCallHistoryEntry(entry())
        assertNull(decodeCallHistoryEntry(""))
        assertNull(decodeCallHistoryEntry("v2" + good.substring(2)))
        assertNull(decodeCallHistoryEntry(good.substringBeforeLast('|')))
        assertNull(decodeCallHistoryEntry("$good|extra"))
        assertNull(decodeCallHistoryEntry(good.replace("OUTGOING", "SIDEWAYS")))
        assertNull(decodeCallHistoryEntry(good.replace("|Chat|", "|Sideways|")))
        assertNull(decodeCallHistoryEntry(good.replace("|1000|", "|later|")))
    }

    @Test
    fun `a conversation id is stored and read back lowercase`() {
        val decoded = decodeCallHistoryEntry(encodeCallHistoryEntry(entry(id = "AABB")))
        assertEquals("aabb", decoded?.conversationIdHex)
    }

    @Test
    fun `an empty conversation id is not a record`() {
        assertNull(decodeCallHistoryEntry(encodeCallHistoryEntry(entry(id = ""))))
    }

    @Test
    fun `a negative duration is refused rather than shown as a negative number`() {
        val good = encodeCallHistoryEntry(entry(duration = 10L))
        assertNull(decodeCallHistoryEntry(good.replace("|10", "|-10")))
    }

    // -------------------------------------------------------------- ordering

    @Test
    fun `the list is newest first regardless of which conversation a call was in`() {
        val rows = callHistoryOrdered(
            listOf(
                entry(id = "a", at = 100L),
                entry(id = "b", at = 300L),
                entry(id = "c", at = 200L),
            ),
        )
        assertEquals(listOf("b", "c", "a"), rows.map { it.conversationIdHex })
    }

    @Test
    fun `two calls in the same millisecond order the same way every time`() {
        // A list that reshuffles on a refresh reads as a bug and is impossible
        // to report.
        val zed = entry(id = "zz", at = 500L)
        val ann = entry(id = "aa", at = 500L)
        assertEquals(
            listOf("aa", "zz"),
            callHistoryOrdered(listOf(zed, ann)).map { it.conversationIdHex },
        )
        assertEquals(
            listOf("aa", "zz"),
            callHistoryOrdered(listOf(ann, zed)).map { it.conversationIdHex },
        )
    }

    @Test
    fun `a new call lands at the top and the oldest falls off the cap`() {
        val existing = (1..3).map { entry(id = "c$it", at = it * 10L) }
        val next = callHistoryWith(existing, entry(id = "new", at = 999L), max = 3)
        assertEquals(listOf("new", "c3", "c2"), next.map { it.conversationIdHex })
    }

    @Test
    fun `the cap is a cap and a zero cap keeps nothing`() {
        assertEquals(CALL_HISTORY_MAX, callHistoryOrdered((1..400).map { entry(at = it.toLong()) }).size)
        assertTrue(callHistoryOrdered(listOf(entry()), max = 0).isEmpty())
    }

    // ------------------------------------------------------------- day groups

    @Test
    fun `calls are grouped into days in the viewer's own zone`() {
        // 2026-09-23T00:30 in Berlin is still 2026-09-22 in UTC. A list that
        // files a late-night call under yesterday because UTC says so is wrong
        // in a way nobody would think to report.
        val berlin = ZoneId.of("Europe/Berlin")
        val lateNight = java.time.ZonedDateTime
            .of(2026, 9, 23, 0, 30, 0, 0, berlin)
            .toInstant()
            .toEpochMilli()
        val days = callHistoryByDay(listOf(entry(at = lateNight)), berlin)
        assertEquals(java.time.LocalDate.of(2026, 9, 23), days.single().day)
        val utcDays = callHistoryByDay(listOf(entry(at = lateNight)), ZoneId.of("UTC"))
        assertEquals(java.time.LocalDate.of(2026, 9, 22), utcDays.single().day)
    }

    @Test
    fun `each day is one run, in the order the list was already in`() {
        val day1 = 1_700_000_000_000L
        val day2 = day1 + 24 * 3_600_000L
        val ordered = callHistoryOrdered(
            listOf(
                entry(id = "a", at = day2 + 60_000L),
                entry(id = "b", at = day2),
                entry(id = "c", at = day1),
            ),
        )
        val days = callHistoryByDay(ordered, zone)
        assertEquals(2, days.size)
        assertEquals(listOf("a", "b"), days[0].rows.map { it.conversationIdHex })
        assertEquals(listOf("c"), days[1].rows.map { it.conversationIdHex })
    }

    @Test
    fun `an empty list groups into no days`() {
        assertTrue(callHistoryByDay(emptyList(), zone).isEmpty())
    }

    // ------------------------------------------------------------- the glyph

    @Test
    fun `a declined call is drawn like a missed one, because neither connected`() {
        assertEquals(CallDirectionGlyph.OUTGOING, callDirectionGlyph(CallOutcome.OUTGOING))
        assertEquals(CallDirectionGlyph.INCOMING, callDirectionGlyph(CallOutcome.INCOMING))
        assertEquals(CallDirectionGlyph.MISSED, callDirectionGlyph(CallOutcome.MISSED))
        assertEquals(CallDirectionGlyph.MISSED, callDirectionGlyph(CallOutcome.DECLINED))
    }

    @Test
    fun `the danger tint is exactly the outcomes where nobody spoke`() {
        assertTrue(callHistoryIsUnanswered(CallOutcome.MISSED))
        assertTrue(callHistoryIsUnanswered(CallOutcome.DECLINED))
        assertFalse(callHistoryIsUnanswered(CallOutcome.OUTGOING))
        assertFalse(callHistoryIsUnanswered(CallOutcome.INCOMING))
    }
}
