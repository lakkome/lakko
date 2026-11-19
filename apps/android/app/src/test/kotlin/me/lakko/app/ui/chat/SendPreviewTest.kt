// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.ArticleRun

/** ADR-0092 / attachments UX 6, 11 — the audio clock, selection styles. */
class SendPreviewTest {

    @Test
    fun `the audio clock reads minutes and hours`() {
        assertEquals("0:00", formatAudioClock(0))
        assertEquals("3:05", formatAudioClock(185_000))
        assertEquals("1:02:03", formatAudioClock(3_723_000))
    }

    @Test
    fun `bold on a selection makes runs and toggles back`() {
        val on = toggleSpan(emptyList(), 2 until 5)
        assertEquals(listOf(2..4), on)
        val block = EditBlock(EditKind.TEXT, "abcdefg", boldSpans = on)
        val runs = runsOf(block).map { it as ArticleRun.Text }
        assertEquals(listOf("ab", "cde", "fg"), runs.map { it.text })
        assertEquals(listOf(false, true, false), runs.map { it.bold })
        assertTrue(toggleSpan(on, 2 until 5).isEmpty())
    }

    @Test
    fun `spans follow an edit before them and drop deleted characters`() {
        // "abcdefg" bold c..e; insert "XY" at 0 → bold moves by two.
        assertEquals(listOf(4..6), shiftSpans("abcdefg", "XYabcdefg", listOf(2..4)))
        // delete "cd" → only "e" stays bold, now at 2.
        assertEquals(listOf(2..2), shiftSpans("abcdefg", "abefg", listOf(2..4)))
    }
}
