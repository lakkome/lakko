// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * What a call is titled.
 *
 * Observed wrong on a live group call: a three-person group named "Trio"
 * showed `01a0a985` as the screen title, because the title was taken from the
 * participant and the participant fell back to the SFU account pseudonym. What
 * a tile is named is `CallTileNameTest`.
 */
class CallScreenTitleTest {
    init {
        // ADR-0061: the words under test are the English catalog's.
        EnglishForTests.install()
    }


    @Test
    fun `a group call is titled by its group`() {
        assertEquals(
            "Trio",
            callScreenTitle(groupName = "Trio", peerName = "Anna", groupCall = true),
        )
    }

    @Test
    fun `a group call is never titled by a participant`() {
        // The live defect exactly: the peer's label was an opaque pseudonym and
        // it became the title of the whole call.
        assertEquals(
            "Group call",
            callScreenTitle(groupName = null, peerName = "01a0a985", groupCall = true),
        )
        assertEquals(
            "Group call",
            callScreenTitle(groupName = "   ", peerName = "Anna", groupCall = true),
        )
    }

    @Test
    fun `a group call with no name says so plainly`() {
        val title = callScreenTitle(groupName = null, peerName = null, groupCall = true)
        assertEquals("Group call", title)
    }

    @Test
    fun `a one to one call is titled by the other person`() {
        assertEquals(
            "Anna",
            callScreenTitle(groupName = "ignored", peerName = "Anna", groupCall = false),
        )
    }

    @Test
    fun `a one to one call with no name falls back to a word, not an identifier`() {
        assertEquals("Call", callScreenTitle(groupName = null, peerName = null, groupCall = false))
        assertEquals("Call", callScreenTitle(groupName = null, peerName = "  ", groupCall = false))
    }
}
