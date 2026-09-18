// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/calls.md` section 3 — the caller's choice reaches the offer.
 *
 * ## The defect these pin
 *
 * Both chat-header buttons produced the same call. The pre-join's camera toggle
 * was hard-coded on, so the phone button dialed `video = true`, the offer
 * carried a video flag and the callee's ring read `Video call` for a call the
 * caller had asked to make with the phone button. The first test here is
 * literally that: the voice button must not open the camera.
 */
class OutgoingCallMediaTest {

    @Test
    fun `the phone button opens the pre-join with the camera off`() {
        assertFalse(prejoinCameraDefault(requestedCallKind(video = false)))
    }

    @Test
    fun `the video button opens the pre-join with the camera on`() {
        assertTrue(prejoinCameraDefault(requestedCallKind(video = true)))
    }

    @Test
    fun `a voice call offers no video`() {
        // The whole chain as the UI runs it: button -> pre-join default ->
        // untouched toggles -> the media flags the offer carries.
        val kind = requestedCallKind(video = false)
        val media = offeredCallMedia(
            microphoneOn = true,
            cameraOn = prejoinCameraDefault(kind),
        )
        assertEquals(CallMediaChoice(audio = true, video = false), media)
    }

    @Test
    fun `a video call offers video`() {
        val kind = requestedCallKind(video = true)
        val media = offeredCallMedia(
            microphoneOn = true,
            cameraOn = prejoinCameraDefault(kind),
        )
        assertEquals(CallMediaChoice(audio = true, video = true), media)
    }

    @Test
    fun `the pre-join toggles decide what is offered, not the button`() {
        // Section 3: "the toggles there change the call that is actually
        // placed". A voice call whose camera the user switches on IS a video
        // call and says so; a video call joined with the camera off is not.
        assertTrue(offeredCallMedia(microphoneOn = true, cameraOn = true).video)
        assertFalse(offeredCallMedia(microphoneOn = true, cameraOn = false).video)
    }

    @Test
    fun `a muted join still offers an audio section`() {
        // The track is added muted so the user can unmute mid-call without a
        // renegotiation; what the flag reports is the switch, not a guess.
        val media = offeredCallMedia(microphoneOn = false, cameraOn = false)
        assertFalse(media.audio)
        assertFalse(media.video)
    }

    @Test
    fun `the word for each kind is the word the callee is told`() {
        assertEquals("Voice call", requestedCallKindLabel(RequestedCallKind.VOICE))
        assertEquals("Video call", requestedCallKindLabel(RequestedCallKind.VIDEO))
        // Identical to what the ring notification says for the same offer, so
        // the caller's screen and the callee's cannot drift apart.
        assertEquals(
            callRingContent(callerName = "Anna", video = false).body,
            requestedCallKindLabel(RequestedCallKind.VOICE),
        )
        assertEquals(
            callRingContent(callerName = "Anna", video = true).body,
            requestedCallKindLabel(RequestedCallKind.VIDEO),
        )
    }
}
