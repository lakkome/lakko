// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.TileVideoQuality

/**
 * What the SFU gives and the picture a tile therefore shows
 * (`VideoGiven.kt`, ADR-0070 step 6). The test list follows the web client's
 * `video-given.test.ts`.
 */
class VideoGivenTest {

    private val main = TileVideoQuality.MAIN
    private val middle = TileVideoQuality.MIDDLE
    private val light = TileVideoQuality.LIGHT

    @Test
    fun `a tile shows the lower of what it wants and what the SFU gives`() {
        assertEquals(middle, effectiveQuality(main, middle))
        assertEquals(light, effectiveQuality(main, light))
        assertEquals(light, effectiveQuality(middle, light))
        // The SFU never raises what a tile shows above what it asked for.
        assertEquals(middle, effectiveQuality(middle, main))
        assertEquals(light, effectiveQuality(light, main))
        assertEquals(main, effectiveQuality(main, main))
    }

    @Test
    fun `with nothing said about a person the tile shows what it wants`() {
        for (wanted in listOf(main, middle, light)) assertEquals(wanted, effectiveQuality(wanted, null))
    }

    @Test
    fun `a person with no light copy keeps the heavy picture the tile wants when given light`() {
        assertEquals(light, shownQuality(main, light, hasLight = true))
        assertEquals(main, shownQuality(main, light, hasLight = false))
        assertEquals(middle, shownQuality(middle, light, hasLight = false))
        // A tile that itself wanted light of them asks for middle.
        assertEquals(middle, shownQuality(light, null, hasLight = false))
        assertEquals(middle, shownQuality(main, middle, hasLight = false))
    }

    @Test
    fun `the heavy sink follows what is given, and is on nothing when the light copy is shown alone`() {
        assertEquals("M", heavyTrackForGiven(main, null, "M", "m", hasLight = true))
        assertEquals("m", heavyTrackForGiven(main, middle, "M", "m", hasLight = true))
        assertEquals("M", heavyTrackForGiven(main, middle, "M", null, hasLight = true))
        assertNull(heavyTrackForGiven(main, light, "M", "m", hasLight = true))
        // Nothing else to show: the paused heavy picture stays.
        assertEquals("M", heavyTrackForGiven(main, light, "M", "m", hasLight = false))
        assertEquals("m", heavyTrackForGiven(middle, main, "M", "m", hasLight = true))
    }

    @Test
    fun `each frame replaces the whole set, and a person who leaves is forgotten`() {
        val book = VideoGivenBook()
        assertNull(book.givenOf("a"))
        assertEquals(0, book.size())
        book.replace(listOf("a" to middle, "b" to light, "a" to light))
        // Named twice: the later entry.
        assertEquals(light, book.givenOf("a"))
        assertEquals(2, book.size())
        book.replace(listOf("b" to main))
        // Not named any more: back to nothing said.
        assertNull(book.givenOf("a"))
        assertEquals(mapOf("b" to main), book.snapshot())
        assertTrue(book.removePeer("b"))
        assertFalse(book.removePeer("b"))
        book.replace(listOf("c" to light))
        book.clear()
        assertEquals(0, book.size())
    }

    @Test
    fun `a snapshot is a copy`() {
        val book = VideoGivenBook()
        book.replace(listOf("a" to light))
        val snapshot = book.snapshot()
        book.replace(emptyList())
        assertEquals(mapOf("a" to light), snapshot)
    }

    @Test
    fun `only the people whose shown picture changed are named, in the order of the wants`() {
        val wants = linkedMapOf("a" to main, "b" to middle, "c" to light, "d" to main)
        val before = mapOf("a" to main, "b" to main)
        val after = mapOf("a" to light, "b" to middle, "c" to light, "d" to middle)
        assertEquals(
            listOf(
                // main -> light.
                VideoGivenChange("a", main, light),
                // `b` wanted middle and is given middle after main: no change.
                // `c` wants light either way: no change.
                // `d` had nothing said and is now given less than it wants.
                VideoGivenChange("d", main, middle),
            ),
            videoGivenChanges(wants, { before[it] }, { after[it] }),
        )
        // A person the new frame stops naming is back to what is wanted.
        assertEquals(
            listOf(VideoGivenChange("a", main, null)),
            videoGivenChanges(wants, { after[it] }, { mapOf("d" to middle)[it] }),
        )
    }

    @Test
    fun `the given line names each changed person, and there is none when nobody changed`() {
        assertNull(describeGivenChanges("call video given", emptyList()))
        assertEquals(
            "call video given peer=a want=main given=light | peer=d want=main given=-",
            describeGivenChanges(
                "call video given",
                listOf(VideoGivenChange("a", main, light), VideoGivenChange("d", main, null)),
            ),
        )
    }
}
