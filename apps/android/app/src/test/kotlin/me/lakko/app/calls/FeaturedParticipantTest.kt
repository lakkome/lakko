// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * `docs/ux/calls.md` section 4.4 — which tile a group call draws large. The
 * same rule as the web `featuredParticipant` and the iOS
 * `CallScreenPresentation.featured`, pinned here so the three screens cannot
 * drift back to each picking someone else.
 */
class FeaturedParticipantTest {

    private fun tile(
        id: String,
        isLocal: Boolean = false,
        camOn: Boolean = false,
        speaking: Boolean = false,
    ) = CallUiState.Participant(
        id = id,
        displayName = id,
        isLocal = isLocal,
        micOn = true,
        camOn = camOn,
        handRaised = false,
        quality = CallUiState.QualityTier.Good,
        speaking = speaking,
        screenSharing = false,
    )

    private val me = tile("me", isLocal = true, camOn = true)

    @Test
    fun `nobody is featured when no one else is sending video`() {
        // The defect: the first remote held the big slot with an avatar in it
        // while other cameras ran in small tiles. With no remote camera the
        // honest layout is an equal grid, the local camera notwithstanding.
        val roster = listOf(me, tile("a"), tile("b"), tile("c"))
        assertNull(featuredParticipant(roster, null))
        assertEquals(listOf("me", "a", "b", "c"), galleryParticipants(roster, null).map { it.id })
    }

    @Test
    fun `the first remote with a camera is featured, in roster order`() {
        val roster = listOf(me, tile("a"), tile("b", camOn = true), tile("c", camOn = true))
        val featured = featuredParticipant(roster, null)
        assertEquals("b", featured?.id)
        assertEquals(listOf("me", "a", "c"), galleryParticipants(roster, featured).map { it.id })
    }

    @Test
    fun `a speaking remote with a camera outranks an earlier silent one`() {
        val roster = listOf(me, tile("a", camOn = true), tile("b", camOn = true, speaking = true))
        assertEquals("b", featuredParticipant(roster, null)?.id)
    }

    @Test
    fun `a speaker without a camera is never featured`() {
        assertEquals(
            "b",
            featuredParticipant(listOf(me, tile("a", speaking = true), tile("b", camOn = true)), null)?.id,
        )
        assertNull(featuredParticipant(listOf(me, tile("a", speaking = true)), null))
    }

    @Test
    fun `the local tile is never featured while anyone else is present`() {
        assertNull(featuredParticipant(listOf(me, tile("a")), null))
    }

    @Test
    fun `the local tile is featured when alone`() {
        assertEquals("me", featuredParticipant(listOf(me), null)?.id)
        assertEquals("solo", featuredParticipant(listOf(tile("solo", isLocal = true)), null)?.id)
    }

    @Test
    fun `a pin wins over the camera rule`() {
        val roster = listOf(me, tile("a"), tile("b", camOn = true))
        assertEquals("a", featuredParticipant(roster, "a")?.id)
    }

    @Test
    fun `a pin naming nobody in the roster is ignored`() {
        val roster = listOf(me, tile("a"), tile("b", camOn = true))
        assertEquals("b", featuredParticipant(roster, "gone")?.id)
    }

    @Test
    fun `the slot passes to the next camera when the featured one stops`() {
        val before = listOf(me, tile("a", camOn = true), tile("b", camOn = true))
        val after = listOf(me, tile("a"), tile("b", camOn = true))
        assertEquals("a", featuredParticipant(before, null)?.id)
        assertEquals("b", featuredParticipant(after, null)?.id)
    }

    @Test
    fun `an empty roster features nobody`() {
        assertNull(featuredParticipant(emptyList(), null))
        assertEquals(emptyList<CallUiState.Participant>(), galleryParticipants(emptyList(), null))
    }
}
