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
    fun `a group call is a grid even when this device alone has a camera on`() {
        assertEquals(
            CallStage.GRID,
            callStage(
                remoteParticipants = 2,
                remoteVideoPresent = false,
                localVideoPresent = true,
                groupCall = true,
            ),
        )
    }

    @Test
    fun `a group call with nobody else yet is still a group call`() {
        // Otherwise the tiles and the roster disappear until the second
        // person arrives, and come back when they do.
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
    fun `starting the camera on a voice call turns it into a video one`() {
        // Section 4.1: pressing the camera button mid-call "starts the camera,
        // tells the other side, and the screen becomes the video layout for
        // both". This device's half of that: the portrait becomes a picture.
        assertEquals(
            CallStage.LOCAL_VIDEO,
            callStage(remoteParticipants = 1, remoteVideoPresent = false, localVideoPresent = true),
        )
    }

    @Test
    fun `turning the camera off again returns the call to the portrait`() {
        assertEquals(
            CallStage.PORTRAIT,
            callStage(remoteParticipants = 1, remoteVideoPresent = false, localVideoPresent = false),
        )
    }

    @Test
    fun `the other person's picture outranks this device's own`() {
        // Once both are sending there is one main surface, and it belongs to
        // the person being spoken to.
        assertEquals(
            CallStage.REMOTE_VIDEO,
            callStage(remoteParticipants = 1, remoteVideoPresent = true, localVideoPresent = true),
        )
    }

    @Test
    fun `the person who has not started their camera has no self-view`() {
        // "their screen shows the incoming picture with their self-view absent"
        val stage = callStage(
            remoteParticipants = 1,
            remoteVideoPresent = true,
            localVideoPresent = false,
        )
        assertEquals(CallStage.REMOTE_VIDEO, stage)
        assertFalse(showsSelfViewPip(stage, localVideoPresent = false))
    }

    @Test
    fun `the self-view appears once both cameras are on`() {
        val stage = callStage(
            remoteParticipants = 1,
            remoteVideoPresent = true,
            localVideoPresent = true,
        )
        assertTrue(showsSelfViewPip(stage, localVideoPresent = true))
    }

    @Test
    fun `the only picture on screen is never also a self-view`() {
        // On LOCAL_VIDEO this device's own camera IS the main surface; a
        // rectangle showing the same camera again would be it twice over.
        assertFalse(
            showsSelfViewPip(CallStage.LOCAL_VIDEO, localVideoPresent = true),
        )
        assertFalse(showsSelfViewPip(CallStage.PORTRAIT, localVideoPresent = true))
        assertFalse(showsSelfViewPip(CallStage.GRID, localVideoPresent = true))
    }

    @Test
    fun `controls fade only on a connected video call`() {
        assertTrue(controlsMayAutoHide(CallStage.LOCAL_VIDEO, connected = true))
        // A call started from the Calls screen begins with one person in it,
        // and the only picture is this device's own camera. Hiding the row
        // there reveals nothing and takes away the only way out - found live
        // on 2026-09-23, where a freshly started call lost its End button five
        // seconds in.
        assertFalse(
            controlsMayAutoHide(CallStage.LOCAL_VIDEO, connected = true, alone = true))
        assertFalse(
            controlsMayAutoHide(CallStage.REMOTE_VIDEO, connected = true, alone = true))
        assertFalse(controlsMayAutoHide(CallStage.LOCAL_VIDEO, connected = false))
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
