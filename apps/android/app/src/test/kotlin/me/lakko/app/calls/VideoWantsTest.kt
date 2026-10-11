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
 * What this device asks the SFU for (`VideoWants.kt`, section 7.6 of
 * `docs/ux/group-call-grid.md`, ADR-0070 step 5). The shared rule is Rust and
 * tested there; the host JVM cannot load it, so a [FakeGroupCallRules] stands
 * in and these tests check what the client asks it and what it does with the
 * answer. The test list follows the web client's `video-wants.test.ts`.
 */
class VideoWantsTest {

    private val main = TileVideoQuality.MAIN
    private val middle = TileVideoQuality.MIDDLE
    private val light = TileVideoQuality.LIGHT

    // ---- the order of the qualities -------------------------------------------

    @Test
    fun `main is over middle is over light`() {
        assertEquals(main, higherQuality(main, light))
        assertEquals(middle, higherQuality(light, middle))
        assertEquals(light, lowerQuality(main, light))
        assertEquals(middle, lowerQuality(middle, main))
        assertEquals(light, higherQuality(light, light))
        assertEquals(listOf("main", "middle", "light"), listOf(main, middle, light).map { it.wireName })
    }

    // ---- the tile's width ------------------------------------------------------

    @Test
    fun `a tile's width is asked about in physical pixels`() {
        // 200 dp on a 3x phone is 600 pixels; on a 1x screen 200.
        assertEquals(600, physicalTileWidth(200, 3f))
        assertEquals(200, physicalTileWidth(200, 1f))
        assertEquals(263, physicalTileWidth(100, 2.625f))
        // A density the platform does not give is read as 1; no width is 0.
        assertEquals(200, physicalTileWidth(200, 0f))
        assertEquals(200, physicalTileWidth(200, Float.NaN))
        assertEquals(0, physicalTileWidth(0, 3f))
        assertEquals(0, physicalTileWidth(-5, 3f))
    }

    // ---- asking the rule -------------------------------------------------------

    @Test
    fun `the rule is asked with the people, the width in pixels and whether the tile is large`() {
        val rules = FakeGroupCallRules(
            tileQuality = { people, width, large ->
                when {
                    people <= 2u -> main
                    large && width >= 960u -> main
                    width < 240u -> light
                    else -> middle
                }
            },
        )
        assertEquals(main, tileQualityOf(rules, 2, 100, false))
        assertEquals(light, tileQualityOf(rules, 5, 239, false))
        assertEquals(middle, tileQualityOf(rules, 5, 600, false))
        assertEquals(middle, tileQualityOf(rules, 5, 600, true))
        assertEquals(main, tileQualityOf(rules, 5, 1_080, true))
        assertEquals(
            listOf(
                Triple(2u, 100u, false),
                Triple(5u, 239u, false),
                Triple(5u, 600u, false),
                Triple(5u, 600u, true),
                Triple(5u, 1_080u, true),
            ),
            rules.tileQualityQuestions.toList(),
        )
    }

    @Test
    fun `while the rule cannot be asked a tile shows the main picture`() {
        val throwing = FakeGroupCallRules(tileQuality = { _, _, _ -> throw IllegalStateException("not loaded") })
        assertEquals(main, tileQualityOf(throwing, 5, 100, false))
        val unlinked = FakeGroupCallRules(tileQuality = { _, _, _ -> throw UnsatisfiedLinkError("no library") })
        assertEquals(main, tileQualityOf(unlinked, 5, 100, false))
        // Nonsense in is clamped, not passed on.
        val rules = FakeGroupCallRules()
        tileQualityOf(rules, -1, -20, false)
        assertEquals(Triple(0u, 0u, false), rules.tileQualityQuestions.single())
    }

    // ---- from the rule's answer to what is asked and shown ------------------------

    @Test
    fun `a tile of a person with no light copy asks for middle in place of light`() {
        assertEquals(light, askedQuality(light, hasLight = true))
        assertEquals(middle, askedQuality(light, hasLight = false))
        assertEquals(main, askedQuality(main, hasLight = false))
        assertEquals(middle, askedQuality(middle, hasLight = true))
    }

    @Test
    fun `the heavy sink is on the track of the quality shown`() {
        assertEquals("M", heavyTrackFor(main, "M", "m"))
        assertEquals("m", heavyTrackFor(middle, "M", "m"))
        // No middle track: the SFU forwards the main one for `middle`.
        assertEquals("M", heavyTrackFor(middle, "M", null))
        // The light picture alone: no heavy track at all.
        assertNull(heavyTrackFor(light, "M", "m"))
        assertNull(heavyTrackFor<String>(main, null, "m"))
    }

    @Test
    fun `the light copy covers a replaced heavy picture only while there is one`() {
        assertTrue(coversHeavySwap(awaitingFirstHeavyFrame = true, hasLight = true))
        assertFalse(coversHeavySwap(awaitingFirstHeavyFrame = true, hasLight = false))
        assertFalse(coversHeavySwap(awaitingFirstHeavyFrame = false, hasLight = true))
    }

    // ---- the book ---------------------------------------------------------------

    @Test
    fun `a person no tile shows wants the light picture, and one shown wants their highest tile`() {
        val book = VideoWantsBook()
        book.setPeers(listOf("a", "b", "c"))
        assertEquals(4, book.people())
        val grid = Any()
        val stage = Any()
        assertTrue(book.setTile(grid, "a", middle))
        assertTrue(book.setTile(stage, "a", main))
        assertTrue(book.setTile(Any(), "b", light))
        assertEquals(linkedMapOf("a" to main, "b" to light, "c" to light), book.wants())
        // The same report again changes nothing.
        assertFalse(book.setTile(stage, "a", main))
        // The stage tile goes: the grid tile's want is what is left.
        assertTrue(book.setTile(stage, "a", null))
        assertFalse(book.setTile(stage, "a", null))
        assertEquals(middle, book.qualityOf("a"))
    }

    @Test
    fun `the set is everyone in the call, in the order they joined`() {
        val book = VideoWantsBook()
        book.setPeers(listOf("b", "a"))
        book.addPeer("c")
        book.removePeer("b")
        assertEquals(listOf("a", "c"), book.wants().keys.toList())
        assertEquals(3, book.people())
        // A tile of someone who is not in the call is in no set.
        book.setTile(Any(), "gone", main)
        assertEquals(listOf("a", "c"), book.wants().keys.toList())
        book.clear()
        assertTrue(book.wants().isEmpty())
        assertEquals(1, book.people())
    }

    @Test
    fun `past the protocol's bound the main entries are left out first, then the last to join`() {
        val wants = linkedMapOf("a" to main, "b" to light, "c" to middle, "d" to main, "e" to light)
        assertEquals(wants.toList(), videoWantsFrame(wants))
        // Left out is `main` anyway: nothing is lost.
        assertEquals(listOf("b" to light, "c" to middle, "e" to light), videoWantsFrame(wants, max = 3))
        assertEquals(listOf("b" to light, "c" to middle), videoWantsFrame(wants, max = 2))
        assertEquals(64, VIDEO_WANTS_MAX)
    }

    @Test
    fun `the wants line names every person and what is asked of them`() {
        assertEquals(
            "call video wants p1=main p2=light",
            describeVideoWants(listOf("p1" to main, "p2" to light)),
        )
    }

    // ---- the sender ---------------------------------------------------------------

    private class Wire {
        val frames = ArrayList<List<Pair<String, TileVideoQuality>>>()
        var open = true
        val send: (List<Pair<String, TileVideoQuality>>) -> Boolean = { frame ->
            if (open) frames += frame
            open
        }
    }

    @Test
    fun `the reports of one pass become one frame with the whole set`() {
        val timers = FakeCallTimers()
        val book = VideoWantsBook().apply { setPeers(listOf("a", "b")) }
        val wire = Wire()
        val sender = VideoWantsSender(book, wire.send, timers)
        book.setTile(Any(), "a", main)
        sender.changed()
        book.setTile(Any(), "b", middle)
        sender.changed()
        assertTrue(wire.frames.isEmpty())
        timers.advance(0)
        assertEquals(listOf(listOf("a" to main, "b" to middle)), wire.frames.toList())
    }

    @Test
    fun `an unchanged set is not sent again, unless it is asked for`() {
        val timers = FakeCallTimers()
        val book = VideoWantsBook().apply { setPeers(listOf("a")) }
        val wire = Wire()
        val sender = VideoWantsSender(book, wire.send, timers)
        sender.changed()
        timers.advance(0)
        sender.changed()
        timers.advance(1_000)
        assertEquals(1, wire.frames.size)
        // After a join, or once media flows: the same set, said again.
        sender.resend()
        timers.advance(1_000)
        assertEquals(2, wire.frames.size)
    }

    @Test
    fun `two frames are never closer than the minimum interval`() {
        assertEquals(200L, VIDEO_WANTS_MIN_INTERVAL_MS)
        val timers = FakeCallTimers()
        val book = VideoWantsBook().apply { setPeers(listOf("a")) }
        val wire = Wire()
        val sender = VideoWantsSender(book, wire.send, timers)
        val tile = Any()
        sender.changed()
        timers.advance(0)
        book.setTile(tile, "a", main)
        sender.changed()
        timers.advance(199)
        assertEquals(1, wire.frames.size)
        // What is sent is the set as it stands when the time comes.
        book.setTile(tile, "a", middle)
        sender.changed()
        timers.advance(1)
        assertEquals(listOf("a" to middle), wire.frames.last())
        assertEquals(2, wire.frames.size)
    }

    @Test
    fun `a frame that could not go is tried again at the next call, and an empty set is never sent`() {
        val timers = FakeCallTimers()
        val book = VideoWantsBook()
        val wire = Wire().apply { open = false }
        val sender = VideoWantsSender(book, wire.send, timers)
        // Nobody else in the call: nothing to ask for.
        sender.resend()
        timers.advance(0)
        book.setPeers(listOf("a"))
        sender.changed()
        timers.advance(0)
        assertTrue(wire.frames.isEmpty())
        // The publish offer is out now.
        wire.open = true
        sender.changed()
        timers.advance(0)
        assertEquals(listOf(listOf("a" to light)), wire.frames.toList())
    }

    @Test
    fun `after a reset nothing pending is sent and the next use starts afresh`() {
        val timers = FakeCallTimers()
        val book = VideoWantsBook().apply { setPeers(listOf("a")) }
        val wire = Wire()
        val sender = VideoWantsSender(book, wire.send, timers)
        sender.changed()
        sender.reset()
        timers.advance(1_000)
        assertTrue(wire.frames.isEmpty())
        assertEquals(0, timers.pending)
        sender.changed()
        timers.advance(0)
        assertEquals(1, wire.frames.size)
    }

    // ---- a settled width --------------------------------------------------------------

    @Test
    fun `the first width is used at once and a later one only after it stood still`() {
        assertEquals(300L, TILE_WIDTH_SETTLE_MS)
        val timers = FakeCallTimers()
        val applied = ArrayList<Int>()
        val settled = SettledWidth(timers) { applied += it }
        settled.observe(600)
        assertEquals(listOf(600), applied)
        // A layout change in steps: only where it comes to rest is used.
        settled.observe(500)
        timers.advance(100)
        settled.observe(400)
        timers.advance(299)
        assertEquals(listOf(600), applied)
        timers.advance(1)
        assertEquals(listOf(600, 400), applied)
        // Back to what is applied: nothing is pending, nothing is applied.
        settled.observe(390)
        settled.observe(400)
        timers.advance(1_000)
        assertEquals(listOf(600, 400), applied)
    }

    @Test
    fun `a tile that is gone applies nothing`() {
        val timers = FakeCallTimers()
        val applied = ArrayList<Int>()
        val settled = SettledWidth(timers) { applied += it }
        settled.observe(600)
        settled.observe(300)
        settled.dispose()
        timers.advance(1_000)
        assertEquals(listOf(600), applied)
    }
}
