// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/calls.md` section 4 — which of the three shapes the call screen
 * draws, and when the controls are allowed to get out of the way.
 */
class CallScreenLayoutTest {

    @Test
    fun `a voice call is a portrait`() {
        assertEquals(
            CallStage.PORTRAIT,
            callStage(remoteParticipants = 1, remoteVideoPresent = false),
        )
    }

    @Test
    fun `the other person's video fills the screen`() {
        assertEquals(
            CallStage.REMOTE_VIDEO,
            callStage(remoteParticipants = 1, remoteVideoPresent = true),
        )
    }

    @Test
    fun `a call nobody has answered yet is still a portrait`() {
        // Section 4.1's `Calling` state: there is no roster and no video, and
        // the screen must still say who is being called rather than showing a
        // blank tile, which is what the old grid did.
        assertEquals(
            CallStage.PORTRAIT,
            callStage(remoteParticipants = 0, remoteVideoPresent = false),
        )
    }

    @Test
    fun `a group call keeps the grid whether or not cameras are on`() {
        // Section 9 leaves the group layout alone; a grid is what shows who is
        // in the room, camera or no camera.
        assertEquals(CallStage.GRID, callStage(remoteParticipants = 2, remoteVideoPresent = false))
        assertEquals(CallStage.GRID, callStage(remoteParticipants = 2, remoteVideoPresent = true))
        assertEquals(CallStage.GRID, callStage(remoteParticipants = 7, remoteVideoPresent = false))
    }

    @Test
    fun `a group call with nobody else yet is still a group call`() {
        // Otherwise the roster, the chat and the overflow button all disappear
        // until the second person arrives, and come back when they do.
        assertEquals(
            CallStage.GRID,
            callStage(remoteParticipants = 0, remoteVideoPresent = false, groupCall = true),
        )
        assertEquals(
            CallStage.GRID,
            callStage(remoteParticipants = 1, remoteVideoPresent = true, groupCall = true),
        )
    }

    @Test
    fun `a one to one call is never a grid`() {
        listOf(true, false).forEach { video ->
            assertNotEquals(
                CallStage.GRID,
                callStage(remoteParticipants = 1, remoteVideoPresent = video, groupCall = false),
            )
        }
    }

    @Test
    fun `controls fade only on a connected video call`() {
        assertTrue(controlsMayAutoHide(CallStage.REMOTE_VIDEO, connected = true))
        assertFalse(
            "not before the call is up",
            controlsMayAutoHide(CallStage.REMOTE_VIDEO, connected = false),
        )
    }

    @Test
    fun `controls never hide on a voice call`() {
        // "on a voice call they never hide" — section 4.2 gives the reason:
        // nothing to look at underneath, so hiding only makes the user tap.
        assertFalse(controlsMayAutoHide(CallStage.PORTRAIT, connected = true))
        assertFalse(controlsMayAutoHide(CallStage.PORTRAIT, connected = false))
    }

    @Test
    fun `controls never hide on a group call`() {
        assertFalse(controlsMayAutoHide(CallStage.GRID, connected = true))
    }

    @Test
    fun `the fade delay is the five seconds the spec names`() {
        assertEquals(5_000L, CALL_CONTROLS_AUTO_HIDE_MS)
    }
}
