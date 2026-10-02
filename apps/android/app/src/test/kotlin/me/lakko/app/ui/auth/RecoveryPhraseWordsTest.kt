// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import kotlin.random.Random
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The pure helpers behind the recovery-phrase grid, the entry grid and the
 * three-word confirmation of docs/ux/client-redesign.md section 5.6.
 */
class RecoveryPhraseWordsTest {

    private val phrase = (1..RECOVERY_PHRASE_WORDS).map { "w$it" }

    @Test
    fun `grid rows read across, three columns by four rows on a phone`() {
        val rows = phraseGridRows(phrase)
        assertEquals(PHRASE_GRID_NARROW.rows, rows.size)
        assertTrue(rows.all { it.size == PHRASE_GRID_NARROW.columns })
        assertEquals(listOf(NumberedWord(1, "w1"), NumberedWord(2, "w2"), NumberedWord(3, "w3")), rows[0])
        assertEquals(listOf(NumberedWord(10, "w10"), NumberedWord(11, "w11"), NumberedWord(12, "w12")), rows[3])
        // Row-major: flattening the rows gives the phrase order back.
        assertEquals(phrase.indices.map { it + 1 }, rows.flatten().map { it.position })
    }

    @Test
    fun `a wide screen gets four columns by three rows in the same order`() {
        val rows = phraseGridRows(phrase, PHRASE_GRID_WIDE.columns)
        assertEquals(PHRASE_GRID_WIDE.rows, rows.size)
        assertEquals(listOf(1, 2, 3, 4), rows[0].map { it.position })
        assertEquals(listOf(9, 10, 11, 12), rows[2].map { it.position })
        assertEquals(phrase.indices.map { it + 1 }, rows.flatten().map { it.position })
    }

    @Test
    fun `the shape follows the window width`() {
        assertEquals(PHRASE_GRID_NARROW, phraseGridShape(360))
        assertEquals(PHRASE_GRID_NARROW, phraseGridShape(PHRASE_GRID_WIDE_MIN_DP - 1))
        assertEquals(PHRASE_GRID_WIDE, phraseGridShape(PHRASE_GRID_WIDE_MIN_DP))
        assertEquals(12, PHRASE_GRID_NARROW.columns * PHRASE_GRID_NARROW.rows)
        assertEquals(12, PHRASE_GRID_WIDE.columns * PHRASE_GRID_WIDE.rows)
    }

    @Test
    fun `a count that does not divide leaves the last row short`() {
        val rows = phraseGridRows(listOf("a", "b", "c", "d"))
        assertEquals(2, rows.size)
        assertEquals(listOf(NumberedWord(1, "a"), NumberedWord(2, "b"), NumberedWord(3, "c")), rows[0])
        assertEquals(listOf(NumberedWord(4, "d")), rows[1])
        assertTrue(phraseGridRows(emptyList()).isEmpty())
    }

    @Test
    fun `phrase words fold case and tolerate any whitespace`() {
        assertEquals(listOf("abandon", "about"), phraseWords("  Abandon\n\t about  "))
        assertTrue(phraseWords("   ").isEmpty())
    }

    @Test
    fun `pasted text fills the leading slots and pads the rest`() {
        val slots = slotsFromPasted("one two  three", count = 5)
        assertEquals(listOf("one", "two", "three", "", ""), slots)
        assertEquals(12, slotsFromPasted((1..30).joinToString(" ")).size)
        assertEquals("one two three", phraseFromSlots(slots))
        assertEquals("a b", phraseFromSlots(listOf("", " A", "", "b ", "")))
    }

    @Test
    fun `confirmation picks three distinct ascending positions within range`() {
        repeat(200) { seed ->
            val positions = pickConfirmationPositions(random = Random(seed))
            assertEquals(3, positions.size)
            assertEquals(positions.sorted(), positions)
            assertEquals(3, positions.toSet().size)
            assertTrue(positions.all { it in 1..12 })
        }
    }

    @Test
    fun `confirmation cannot ask for more positions than there are words`() {
        assertEquals(listOf(1, 2), pickConfirmationPositions(wordCount = 2, count = 3, random = Random(1)))
        assertTrue(pickConfirmationPositions(count = 0).isEmpty())
    }

    @Test
    fun `confirmation accepts trimmed case-folded answers`() {
        val verdict = checkConfirmation(
            words = phrase,
            positions = listOf(2, 7, 12),
            answers = mapOf(2 to " W2", 7 to "w7 ", 12 to "W12"),
        )
        assertEquals(ConfirmationVerdict.Confirmed, verdict)
    }

    @Test
    fun `confirmation names every wrong or empty position`() {
        val verdict = checkConfirmation(
            words = phrase,
            positions = listOf(2, 7, 12),
            answers = mapOf(2 to "w2", 7 to "nope", 12 to ""),
        )
        assertEquals(ConfirmationVerdict.Mismatch(listOf(7, 12)), verdict)
    }

    @Test
    fun `a position past the phrase is a mismatch not a crash`() {
        val verdict = checkConfirmation(listOf("a"), listOf(1, 2), mapOf(1 to "a", 2 to "b"))
        assertEquals(ConfirmationVerdict.Mismatch(listOf(2)), verdict)
    }
}
