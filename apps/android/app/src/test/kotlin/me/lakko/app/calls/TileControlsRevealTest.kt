// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * The pin control of a group call (`docs/ux/group-call-grid.md` section
 * 6.2): a tap on a tile only brings its controls up, and only the pin button
 * pins. The reveal's test list follows the web client's
 * `tile-controls-reveal.test.ts`.
 */
class TileControlsRevealTest {

    private class Harness {
        val timers = FakeCallTimers()
        val changes = ArrayList<String?>()
        val reveal = TileControlsReveal(timers) { changes += it }
    }

    @Test
    fun `a tap brings one tile's controls up for three seconds`() {
        assertEquals(3_000L, TILE_CONTROLS_REVEAL_MS)
        val h = Harness()
        h.reveal.tap("a")
        assertEquals("a", h.reveal.tileId)
        h.timers.advance(2_999)
        assertEquals("a", h.reveal.tileId)
        h.timers.advance(1)
        assertNull(h.reveal.tileId)
        assertEquals(listOf("a", null), h.changes.toList())
    }

    @Test
    fun `a second tap on the same tile starts the time again`() {
        val h = Harness()
        h.reveal.tap("a")
        h.timers.advance(2_000)
        h.reveal.tap("a")
        h.timers.advance(2_999)
        assertEquals("a", h.reveal.tileId)
        h.timers.advance(1)
        assertNull(h.reveal.tileId)
        // Nothing was reported for the second tap: the same tile is shown.
        assertEquals(listOf("a", null), h.changes.toList())
    }

    @Test
    fun `one tile at a time`() {
        val h = Harness()
        h.reveal.tap("a")
        h.reveal.tap("b")
        assertEquals("b", h.reveal.tileId)
        assertEquals(listOf<String?>("a", "b"), h.changes.toList())
        // Only the second tap's time runs.
        h.timers.advance(3_000)
        assertEquals(listOf("a", "b", null), h.changes.toList())
    }

    @Test
    fun `a press anywhere but the shown tile hides its controls`() {
        val h = Harness()
        h.reveal.tap("a")
        // On the tile itself (its pin button included): they stay.
        h.reveal.pressBegan("a")
        assertEquals("a", h.reveal.tileId)
        // On another tile.
        h.reveal.pressBegan("b")
        assertNull(h.reveal.tileId)
        h.reveal.tap("a")
        // On no tile at all.
        h.reveal.pressBegan(null)
        assertNull(h.reveal.tileId)
        // With nothing shown a press does nothing.
        h.reveal.pressBegan("b")
        assertEquals(listOf("a", null, "a", null), h.changes.toList())
        assertEquals(0, h.timers.pending)
    }

    @Test
    fun `hiding stops the time, and a disposed reveal reports nothing more`() {
        val h = Harness()
        h.reveal.tap("a")
        h.reveal.hide()
        assertEquals(0, h.timers.pending)
        h.reveal.tap("b")
        h.reveal.dispose()
        h.timers.advance(10_000)
        assertNull(h.reveal.tileId)
        assertEquals(listOf("a", null, "b"), h.changes.toList())
    }

    // ---- what the pin button does ----------------------------------------------

    private class Call(var pinnedId: String? = null, var layout: GroupLayoutKind? = null) {
        val told = ArrayList<String>()

        fun press(id: String) = togglePin(
            pinnedId = pinnedId,
            id = id,
            setLayoutChoice = {
                layout = it
                told += "layout=$it"
            },
            pin = {
                pinnedId = it
                told += "pin=$it"
            },
        )
    }

    @Test
    fun `pinning from the grid switches to Focus first, then pins`() {
        val call = Call()
        call.press("a")
        assertEquals(listOf("layout=FOCUS", "pin=a"), call.told.toList())
        assertEquals("a", call.pinnedId)
    }

    @Test
    fun `unpinning leaves the layout as it is`() {
        val call = Call(pinnedId = "a", layout = GroupLayoutKind.FOCUS)
        call.press("a")
        assertEquals(listOf("pin=null"), call.told.toList())
        assertNull(call.pinnedId)
        assertEquals(GroupLayoutKind.FOCUS, call.layout)
        // And with the pin gone the layout on screen is still Focus.
        val people = listOf(person("local", isLocal = true), person("a"), person("b"))
        assertEquals(GroupLayoutKind.FOCUS, groupLayoutKind(call.layout, call.pinnedId, people))
    }

    @Test
    fun `one pin at a time, pinning someone else moves the pin`() {
        val call = Call(pinnedId = "a", layout = GroupLayoutKind.FOCUS)
        call.press("b")
        assertEquals("b", call.pinnedId)
        assertEquals(listOf("layout=FOCUS", "pin=b"), call.told.toList())
    }
}
