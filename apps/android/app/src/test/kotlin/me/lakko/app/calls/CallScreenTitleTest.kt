// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

/**
 * What a call is titled, and what an unresolved participant is called.
 *
 * Both were observed wrong on one live group call: a three-person group named
 * "Trio" showed `01a0a985` as the screen title AND as the remote tile's label,
 * because the title was taken from the participant and the participant fell
 * back to the SFU account pseudonym.
 */
class CallScreenTitleTest {
    init {
        // ADR-0058: the words under test are the English catalog's.
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

    @Test
    fun `an unresolved participant is numbered, never given a pseudonym`() {
        assertEquals("Participant 1", unresolvedParticipantLabel(1))
        assertEquals("Participant 2", unresolvedParticipantLabel(2))
    }

    @Test
    fun `the participant label never contains hex-looking noise`() {
        // A pseudonym reads as an identifier the user should recognise and
        // cannot. The label must carry nothing of the sort.
        val label = unresolvedParticipantLabel(3)
        assertFalse(label, label.any { it.isDigit() && label.indexOf(it) < "Participant ".length })
        assertEquals("Participant 3", label)
    }

    @Test
    fun `a nonsense ordinal still produces a readable label`() {
        assertEquals("Participant 1", unresolvedParticipantLabel(0))
        assertEquals("Participant 1", unresolvedParticipantLabel(-4))
    }
}
