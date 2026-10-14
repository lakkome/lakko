// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.LayerDisplayCounters
import uniffi.lakko_ffi_uniffi.LayerDisplayInterface
import uniffi.lakko_ffi_uniffi.ShownLayer
import uniffi.lakko_ffi_uniffi.TileVideoQuality

/**
 * The Android wiring around the shared layer rule (`LayerSwitch.kt`).
 *
 * The rule itself is Rust and is tested there; the host JVM cannot load the
 * native library, so these tests put a scripted [LayerDisplayInterface] in its
 * place and check what the tile does with the answers: which frames it draws,
 * what it tells the rule and when, and what reaches the log.
 */
class LayerSwitchTest {

    /** A stand-in for the shared rule: answers what it is told to, and
     *  records every call with the timestamp it was given. */
    private class ScriptedRule : LayerDisplayInterface {
        var answer: ShownLayer = ShownLayer.FULL
        var toDrain: LayerDisplayCounters = counters()
        var destroyed = false
        val calls = mutableListOf<String>()

        private fun said(call: String): ShownLayer {
            if (destroyed) throw IllegalStateException("LayerDisplay object has already been destroyed")
            calls += call
            return answer
        }

        override fun counters(): LayerDisplayCounters = toDrain
        override fun drain(): LayerDisplayCounters {
            if (destroyed) throw IllegalStateException("LayerDisplay object has already been destroyed")
            calls += "drain"
            return toDrain
        }
        override fun fullFrame(nowMs: ULong): ShownLayer = said("full@$nowMs")
        override fun lightFrame(nowMs: ULong): ShownLayer = said("light@$nowMs")
        override fun resync(nowMs: ULong): ShownLayer = said("resync@$nowMs")
        override fun setLightAvailable(available: Boolean, nowMs: ULong): ShownLayer =
            said("available=$available@$nowMs")
        override fun shown(): ShownLayer = answer
        override fun tick(nowMs: ULong): ShownLayer = said("tick@$nowMs")
    }

    private companion object {
        fun counters(
            switches: Long = 0,
            lightMs: Long = 0,
            stallMax: Long = 0,
            stallTotal: Long = 0,
        ) = LayerDisplayCounters(
            lightSwitches = switches.toULong(),
            lightMs = lightMs.toULong(),
            stallMaxMs = stallMax.toULong(),
            stallTotalMs = stallTotal.toULong(),
        )

        /** What the shared `addLayerDisplayCounters` does: sums, and the
         *  longer of the longest stalls. Only the ledger's bookkeeping is
         *  under test here, so the sum is supplied. */
        fun plus(a: LayerDisplayCounters, b: LayerDisplayCounters) = LayerDisplayCounters(
            lightSwitches = a.lightSwitches + b.lightSwitches,
            lightMs = a.lightMs + b.lightMs,
            stallMaxMs = maxOf(a.stallMaxMs, b.stallMaxMs),
            stallTotalMs = a.stallTotalMs + b.stallTotalMs,
        )
    }

    // ---- the driver ------------------------------------------------------

    @Test
    fun `while the rule shows the full track only heavy frames are drawn`() {
        val rule = ScriptedRule()
        var now = 1_000L
        val driver = LayeredTileDriver(rule, clock = { now })

        assertTrue(driver.heavyFrame())
        now = 1_020L
        assertFalse(driver.lightFrame())
        assertEquals(listOf("full@1000", "light@1020"), rule.calls)
    }

    @Test
    fun `while the rule shows the light track only light frames are drawn`() {
        val rule = ScriptedRule().apply { answer = ShownLayer.LIGHT }
        val driver = LayeredTileDriver(rule, clock = { 5L })

        assertFalse(driver.heavyFrame())
        assertTrue(driver.lightFrame())
        assertEquals(ShownLayer.LIGHT, driver.shown())
    }

    @Test
    fun `the tile says its frames are the light track's exactly when it draws them`() {
        val rule = ScriptedRule()
        val driver = LayeredTileDriver(rule, clock = { 0L })
        // No light track on the tile: never the light one, whatever the rule says.
        driver.setSources(hasHeavy = true, hasLight = false, heavyReplaced = false)
        rule.answer = ShownLayer.LIGHT
        assertFalse(driver.drawsLight())
        // A companion the rule shows.
        driver.setSources(hasHeavy = true, hasLight = true, heavyReplaced = false)
        assertTrue(driver.drawsLight())
        rule.answer = ShownLayer.FULL
        assertFalse(driver.drawsLight())
        // The heavy picture was replaced and has no frame yet: the light copy covers.
        driver.setSources(hasHeavy = true, hasLight = true, heavyReplaced = true)
        assertTrue(driver.drawsLight())
        driver.heavyFrame()
        assertFalse(driver.drawsLight())
        // The light track alone is the tile's one picture.
        driver.setSources(hasHeavy = false, hasLight = true, heavyReplaced = true)
        assertTrue(driver.drawsLight())
    }

    @Test
    fun `a tile presents nothing while the app is away or after it closed`() {
        val driver = LayeredTileDriver(ScriptedRule(), clock = { 0L })
        assertTrue(driver.presenting())
        driver.pause()
        assertFalse(driver.presenting())
        driver.resume()
        assertTrue(driver.presenting())
        driver.close()
        assertFalse(driver.presenting())
    }

    @Test
    fun `every frame of both tracks is reported, drawn or not`() {
        val rule = ScriptedRule()
        val driver = LayeredTileDriver(rule, clock = { 0L })
        repeat(3) { driver.heavyFrame() }
        repeat(2) { driver.lightFrame() }
        assertEquals(3, rule.calls.count { it.startsWith("full@") })
        assertEquals(2, rule.calls.count { it.startsWith("light@") })
    }

    @Test
    fun `the tick and the companion's coming and going reach the rule on the tile's clock`() {
        val rule = ScriptedRule()
        var now = 10L
        val driver = LayeredTileDriver(rule, clock = { now })

        driver.setSources(hasHeavy = true, hasLight = true, heavyReplaced = false)
        now = 60L
        driver.tick()
        now = 70L
        driver.setSources(hasHeavy = true, hasLight = false, heavyReplaced = false)

        assertEquals(listOf("available=true@10", "tick@60", "available=false@70"), rule.calls)
    }

    @Test
    fun `a tile with no heavy picture draws every light frame as its one picture`() {
        val rule = ScriptedRule()
        var now = 0L
        val driver = LayeredTileDriver(rule, clock = { now })

        driver.setSources(hasHeavy = false, hasLight = true, heavyReplaced = false)
        now = 40L
        assertTrue(driver.lightFrame())
        // The light track is not a companion here: the rule is told there is
        // none, and its frames are reported as the tile's picture.
        assertEquals(listOf("available=false@0", "full@40"), rule.calls)

        // In the background too: there is nothing else to draw.
        driver.pause()
        assertTrue(driver.lightFrame())
    }

    @Test
    fun `a replaced heavy picture starts the rule again and the light copy covers until its first frame`() {
        val rule = ScriptedRule()
        var now = 100L
        val driver = LayeredTileDriver(rule, clock = { now })
        driver.setSources(hasHeavy = true, hasLight = true, heavyReplaced = false)
        // A new tile is not a replaced picture: the rule alone decides.
        assertFalse(driver.lightFrame())

        now = 500L
        driver.setSources(hasHeavy = true, hasLight = true, heavyReplaced = true)
        assertTrue(rule.calls.containsAll(listOf("resync@500", "available=true@500")))
        // The rule still says full, but nothing of the new picture has come.
        now = 520L
        assertTrue(driver.lightFrame())
        // Its first frame ends the cover.
        now = 600L
        assertTrue(driver.heavyFrame())
        now = 620L
        assertFalse(driver.lightFrame())
    }

    @Test
    fun `the cover of a replaced picture ends after the time the rule itself waits`() {
        assertEquals(1_500L, FIRST_FRAME_GRACE_MS)
        val rule = ScriptedRule()
        var now = 0L
        val driver = LayeredTileDriver(rule, clock = { now })
        driver.setSources(hasHeavy = true, hasLight = true, heavyReplaced = true)

        now = FIRST_FRAME_GRACE_MS - 1
        driver.tick()
        assertTrue(driver.lightFrame())
        now = FIRST_FRAME_GRACE_MS
        driver.tick()
        // From here the rule decides, and it says full.
        assertFalse(driver.lightFrame())
    }

    @Test
    fun `a replaced picture with no light copy has nothing to cover with`() {
        val rule = ScriptedRule()
        val driver = LayeredTileDriver(rule, clock = { 0L })
        driver.setSources(hasHeavy = true, hasLight = false, heavyReplaced = true)
        assertFalse(driver.lightFrame())
        assertTrue(rule.calls.contains("available=false@0"))
    }

    @Test
    fun `in the background nothing is observed and the main track stays the drawn one`() {
        val rule = ScriptedRule().apply { answer = ShownLayer.LIGHT }
        var now = 100L
        val driver = LayeredTileDriver(rule, clock = { now })

        driver.pause()
        assertTrue(driver.heavyFrame())
        assertFalse(driver.lightFrame())
        driver.tick()
        assertTrue(rule.calls.isEmpty())

        // Back in the foreground: the tile starts again, then observes.
        now = 9_000L
        driver.resume()
        driver.heavyFrame()
        assertEquals(listOf("resync@9000", "full@9000"), rule.calls)
    }

    @Test
    fun `a tile created in the background observes nothing until it is resumed`() {
        val rule = ScriptedRule()
        val driver = LayeredTileDriver(rule, clock = { 1L }, observing = false)
        driver.heavyFrame()
        driver.tick()
        assertTrue(rule.calls.isEmpty())
        driver.resume()
        driver.tick()
        assertEquals(listOf("resync@1", "tick@1"), rule.calls)
    }

    @Test
    fun `closing looks at the clock once more, hands over the last counts and releases the rule`() {
        val rule = ScriptedRule().apply { toDrain = counters(switches = 2, lightMs = 400) }
        var released = 0
        val driver = LayeredTileDriver(rule, clock = { 77L }, release = { released++ })

        val last = driver.close()

        assertEquals(counters(switches = 2, lightMs = 400), last)
        assertEquals(listOf("tick@77", "drain"), rule.calls)
        assertEquals(1, released)

        // Closed: nothing reaches the rule, and the main track is the drawn one.
        rule.answer = ShownLayer.LIGHT
        assertTrue(driver.heavyFrame())
        assertFalse(driver.lightFrame())
        driver.tick()
        driver.setSources(hasHeavy = true, hasLight = true, heavyReplaced = true)
        driver.resume()
        assertNull(driver.drain())
        assertNull(driver.close())
        assertEquals(ShownLayer.FULL, driver.shown())
        assertEquals(listOf("tick@77", "drain"), rule.calls)
        assertEquals(1, released)
    }

    @Test
    fun `a frame that arrives while the rule is being released draws the main track`() {
        val rule = ScriptedRule().apply { answer = ShownLayer.LIGHT; destroyed = true }
        val driver = LayeredTileDriver(rule, clock = { 0L })
        assertTrue(driver.heavyFrame())
        assertFalse(driver.lightFrame())
        assertNull(driver.drain())
    }

    // ---- the ledger ------------------------------------------------------

    @Test
    fun `the ledger adds up the tiles that drew one peer, per interval and in total`() {
        val ledger = ShownLayerLedger(plus = ::plus)
        ledger.add("alice", counters(switches = 1, lightMs = 300, stallMax = 200, stallTotal = 200))
        ledger.add("alice", counters(switches = 1, lightMs = 100, stallMax = 350, stallTotal = 350))
        ledger.add("bob", counters())

        val first = ledger.takeInterval()
        assertEquals(listOf("alice", "bob"), first.keys.toList())
        assertEquals(counters(2, 400, 350, 550), first["alice"])
        assertEquals(counters(), first["bob"])

        // A new interval starts empty; a peer no tile reported on is absent.
        ledger.add("alice", counters(switches = 1, lightMs = 50, stallMax = 160, stallTotal = 160))
        val second = ledger.takeInterval()
        assertEquals(listOf("alice"), second.keys.toList())
        assertEquals(counters(1, 50, 160, 160), second["alice"])
        assertTrue(ledger.takeInterval().isEmpty())

        // The total runs from the start of the call.
        assertEquals(counters(3, 450, 350, 710), ledger.totals()["alice"])
        assertEquals(counters(), ledger.totals()["bob"])

        ledger.reset()
        assertTrue(ledger.totals().isEmpty())
        assertTrue(ledger.takeInterval().isEmpty())
    }

    // ---- the log line ----------------------------------------------------

    @Test
    fun `the shown line carries the web client's field names`() {
        assertEquals(
            "call media interval shown peer=p518db want=middle given=light light_switches=2 light_ms=1340" +
                " stall_max_ms=410 stall_total_ms=560",
            describeShown(
                "call media interval",
                "p518db",
                counters(2, 1340, 410, 560),
                want = TileVideoQuality.MIDDLE,
                given = TileVideoQuality.LIGHT,
            ),
        )
        // The SFU has said nothing about the person.
        assertEquals(
            "call media total shown peer=p want=main given=- light_switches=0 light_ms=0" +
                " stall_max_ms=0 stall_total_ms=0",
            describeShown("call media total", "p", counters(), want = TileVideoQuality.MAIN),
        )
    }

    @Test
    fun `one shown line per peer, in the interval and in the total`() {
        val byPeer = linkedMapOf(
            "a" to counters(1, 10, 20, 30),
            "b" to counters(),
        )
        assertEquals(
            listOf(
                "call media total shown peer=a want=main given=middle" +
                    " light_switches=1 light_ms=10 stall_max_ms=20 stall_total_ms=30",
                "call media total shown peer=b want=light given=-" +
                    " light_switches=0 light_ms=0 stall_max_ms=0 stall_total_ms=0",
            ),
            describeShownPeers(
                MEDIA_COUNTERS_TOTAL_LABEL,
                byPeer,
                wantOf = { if (it == "a") TileVideoQuality.MAIN else TileVideoQuality.LIGHT },
                givenOf = { if (it == "a") TileVideoQuality.MIDDLE else null },
            ),
        )
        assertEquals("call media interval", MEDIA_COUNTERS_INTERVAL_LABEL)
    }
}
