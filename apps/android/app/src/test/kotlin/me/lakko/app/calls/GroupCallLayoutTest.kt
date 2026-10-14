// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.CallGrid
import uniffi.lakko_ffi_uniffi.StageCandidate

/**
 * `docs/ux/group-call-grid.md` sections 3 to 6 — this client's side of the
 * group layout: which layout shows, the order of the tiles, the pages, the
 * strip, and what the shared rules are asked.
 *
 * The test list is the web client's (`group-call-layout.test.ts`). Where the
 * web test runs the real rule over wasm, the test here runs
 * [FakeGroupCallRules] and checks the WIRING: the host JVM cannot load the
 * native library, and the rule's own answers are tested in
 * `crates/lakko-ui-rules/src/call_group.rs`.
 */
class GroupCallLayoutTest {
    private val me = person("me", isLocal = true)

    // ---- The grid ------------------------------------------------------------

    @Test
    fun `the grid is the shared rule's answer for the count and the measured area`() {
        val rules = FakeGroupCallRules(
            grid = { _, _, _ ->
                CallGrid(cols = 2u, rows = 3u, tileWidth = 179u, tileHeight = 181u, gap = 8u, perPage = 6u, pages = 2u)
            },
        )
        val grid = gridLayout(rules, count = 8, width = 366, height = 560)
        assertEquals(listOf(Triple(8u, 366u, 560u)), rules.gridQuestions)
        assertEquals(GridLayout(cols = 2, rows = 3, tileWidth = 179, tileHeight = 181, gap = 8, perPage = 6, pages = 2), grid)
    }

    @Test
    fun `an area not measured yet asks for nothing negative`() {
        val rules = FakeGroupCallRules()
        gridLayout(rules, count = -1, width = -5, height = 0)
        assertEquals(listOf(Triple(0u, 0u, 0u)), rules.gridQuestions)
    }

    @Test
    fun `a full row starts at the edge and a short last row is centred`() {
        val grid = GridLayout(cols = 2, rows = 2, tileWidth = 100, tileHeight = 80, gap = 10, perPage = 6, pages = 1)
        // Three tiles: two in the first row, one centred under them.
        assertEquals(TilePlace(0, 0), tilePlace(grid, shown = 3, index = 0))
        assertEquals(TilePlace(110, 0), tilePlace(grid, shown = 3, index = 1))
        assertEquals(TilePlace(55, 90), tilePlace(grid, shown = 3, index = 2))
        // The box is every column wide and as many rows tall as are in use.
        assertEquals(StageSize(210, 170), gridBoxSize(grid, shown = 3))
        assertEquals(StageSize(210, 80), gridBoxSize(grid, shown = 2))
        assertEquals(StageSize(0, 0), gridBoxSize(grid, shown = 0))
    }

    // ---- The order of the tiles ----------------------------------------------

    @Test
    fun `everyone else keeps the order they joined and this device's own tile is last`() {
        val people = listOf(me, person("a"), person("b"))
        assertEquals(listOf("a", "b", "me"), tileOrder(people).map { it.id })
    }

    @Test
    fun `the own tile stays on the first page when the call spills over`() {
        val people = listOf(me) + listOf("a", "b", "c", "d", "e", "f", "g").map { person(it) }
        val first = gridPage(people, perPage = 6, page = 0)
        assertEquals(2, first.pages)
        assertEquals(listOf("a", "b", "c", "d", "e", "me"), first.tiles.map { it.id })
        assertEquals(listOf("f", "g"), gridPage(people, perPage = 6, page = 1).tiles.map { it.id })
    }

    @Test
    fun `a page that emptied falls back to the last page that still has people`() {
        val people = listOf(me, person("a"), person("b"))
        assertEquals(0, gridPage(people, perPage = 6, page = 3).page)
        val many = listOf(me) + (1..9).map { person("p$it") }
        // Ten people at six a page: two pages, and page 7 is the last one.
        assertEquals(1, gridPage(many, perPage = 6, page = 7).page)
    }

    @Test
    fun `no page cap and an empty call draw nothing`() {
        assertEquals(GridPage(emptyList(), 0, 0), gridPage(listOf(me), perPage = 0, page = 0))
        assertEquals(GridPage(emptyList(), 0, 0), gridPage(emptyList(), perPage = 6, page = 0))
    }

    // ---- Which layout shows --------------------------------------------------

    private val three = listOf(me, person("a"), person("b"))

    @Test
    fun `the grid shows until the person chooses otherwise`() {
        assertEquals(GroupLayoutKind.GRID, groupLayoutKind(null, null, three))
        assertEquals(GroupLayoutKind.FOCUS, groupLayoutKind(GroupLayoutKind.FOCUS, null, three))
    }

    @Test
    fun `focus shows while someone is pinned, whatever was chosen`() {
        assertEquals(GroupLayoutKind.FOCUS, groupLayoutKind(GroupLayoutKind.GRID, "a", three))
    }

    @Test
    fun `alone it is the grid, since there is nobody for a strip`() {
        assertEquals(GroupLayoutKind.GRID, groupLayoutKind(GroupLayoutKind.FOCUS, null, listOf(me)))
    }

    @Test
    fun `a pin on someone who has left, or on this device's own tile, is ignored`() {
        assertEquals(GroupLayoutKind.GRID, groupLayoutKind(null, "gone", three))
        assertEquals(GroupLayoutKind.GRID, groupLayoutKind(null, "me", three))
    }

    // ---- The stage -----------------------------------------------------------

    private val four = listOf(me, person("a"), person("b", camOn = true), person("c"))

    @Test
    fun `the stage rule is asked about everyone, with the pin as an index`() {
        val rules = FakeGroupCallRules(stage = { _, pinned -> pinned })
        val stage = stagePerson(rules, four, pinnedId = "c", speech = StageSpeech.None)
        assertEquals("c", stage?.id)
        val (candidates, pinned) = rules.stageQuestions.single()
        assertEquals(3u, pinned)
        assertEquals(
            listOf(
                StageCandidate(isLocal = true, cameraOn = false, speaking = false, lastSpokeAtMs = 0u),
                StageCandidate(isLocal = false, cameraOn = false, speaking = false, lastSpokeAtMs = 0u),
                StageCandidate(isLocal = false, cameraOn = true, speaking = false, lastSpokeAtMs = 0u),
                StageCandidate(isLocal = false, cameraOn = false, speaking = false, lastSpokeAtMs = 0u),
            ),
            candidates,
        )
    }

    @Test
    fun `the stage rule is told who holds the floor and who held it last`() {
        val rules = FakeGroupCallRules(stage = { _, _ -> 1u })
        val speech = StageSpeech(activeSpeakerId = "a", lastSpokeAtMs = mapOf("a" to 10L, "c" to 5L))
        assertEquals("a", stagePerson(rules, four, pinnedId = null, speech = speech)?.id)
        val (candidates, pinned) = rules.stageQuestions.single()
        assertNull(pinned)
        assertEquals(listOf(false, true, false, false), candidates.map { it.speaking })
        assertEquals(listOf<ULong>(0u, 10u, 0u, 5u), candidates.map { it.lastSpokeAtMs })
    }

    @Test
    fun `a pin on someone who is not in the call is not passed on`() {
        val rules = FakeGroupCallRules(stage = { _, _ -> 2u })
        assertEquals("b", stagePerson(rules, four, pinnedId = "gone", speech = StageSpeech.None)?.id)
        assertNull(rules.stageQuestions.single().second)
    }

    @Test
    fun `an empty call has no stage and asks nothing`() {
        val rules = FakeGroupCallRules()
        assertNull(stagePerson(rules, emptyList(), null, StageSpeech.None))
        assertTrue(rules.stageQuestions.isEmpty())
    }

    @Test
    fun `everyone but the stage is in the strip, own tile last`() {
        val stage = four.first { it.id == "a" }
        assertEquals(listOf("b", "c", "me"), stripPeople(four, stage).map { it.id })
        assertEquals(listOf("a", "b", "c", "me"), stripPeople(four, null).map { it.id })
    }

    @Test
    fun `the strip is small on a phone and larger on a wide screen`() {
        assertEquals(StripMetrics(tileWidth = 104, tileHeight = 78, gap = 8), stripMetrics(366))
        assertEquals(StripMetrics(tileWidth = 104, tileHeight = 78, gap = 8), stripMetrics(599))
        assertEquals(StripMetrics(tileWidth = 160, tileHeight = 90, gap = 10), stripMetrics(600))
    }

    @Test
    fun `the stage is one tile asked for the space under the strip`() {
        val rules = FakeGroupCallRules()
        val size = stageSize(rules, areaWidth = 366, areaHeight = 560)
        // One tile, the full width, the height less the strip and its gap.
        assertEquals(listOf(Triple(1u, 366u, (560 - 78 - 8).toUInt())), rules.gridQuestions)
        assertEquals(StageSize(366, 560 - 78 - 8), size)
        // An area shorter than the strip asks for no height, never a negative one.
        stageSize(rules, areaWidth = 366, areaHeight = 40)
        assertEquals(Triple(1u, 366u, 0u), rules.gridQuestions.last())
    }

    // ---- The connection on a tile --------------------------------------------

    @Test
    fun `a connection is drawn only when it is fair or worse`() {
        assertFalse(connectionIsWorthShowing(CallUiState.QualityTier.Excellent))
        assertFalse(connectionIsWorthShowing(CallUiState.QualityTier.Good))
        assertTrue(connectionIsWorthShowing(CallUiState.QualityTier.Fair))
        assertTrue(connectionIsWorthShowing(CallUiState.QualityTier.Poor))
        assertTrue(connectionIsWorthShowing(CallUiState.QualityTier.Bad))
    }
}
