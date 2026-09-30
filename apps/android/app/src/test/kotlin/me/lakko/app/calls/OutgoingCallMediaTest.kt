// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.EnglishForTests
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
 * caller had asked to make with the phone button.
 *
 * The pre-join itself is gone (section 3.1), so the "which way does the toggle
 * open" half of this moved to [CallEntryPointTest]. What is left here is the
 * other half, which outlived the card: what the caller chose, and what the
 * offer finally carries.
 */
class OutgoingCallMediaTest {
    init {
        // ADR-0058: the words under test are the English catalog's.
        EnglishForTests.install()
    }


    @Test
    fun `the phone button is a voice call and the video button a video one`() {
        assertEquals(RequestedCallKind.VOICE, requestedCallKind(video = false))
        assertEquals(RequestedCallKind.VIDEO, requestedCallKind(video = true))
    }

    @Test
    fun `a voice call offers no video`() {
        // The whole chain as the UI runs it now: button -> kind -> the media
        // the host dials with, no screen and no toggle in between.
        val kind = requestedCallKind(video = false)
        val media = immediateDialMedia(kind, hasCamera = true)
        assertEquals(CallMediaChoice(audio = true, video = false), media)
    }

    @Test
    fun `a video call offers video once the camera check is passed`() {
        val media = offeredCallMedia(microphoneOn = true, cameraOn = true)
        assertEquals(CallMediaChoice(audio = true, video = true), media)
    }

    @Test
    fun `the camera check's buttons decide what is offered, not the header button`() {
        // Section 3.2: the check's own microphone and camera buttons are what
        // the call is placed with. A video call whose camera is switched off
        // there is not a video call.
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
    fun `a mid-call renegotiation says which way the camera went`() {
        // The flag was hard-coded `video = true`, which was correct while the
        // only mid-call renegotiation was a camera turning ON. The same button
        // now turns it OFF by moving the video transceiver to `recvOnly` and
        // renegotiating, and an offer still claiming video would be the client
        // lying to its peer about its own media.
        assertEquals(CallMediaChoice(audio = true, video = true), renegotiationMedia(true))
        assertEquals(CallMediaChoice(audio = true, video = false), renegotiationMedia(false))
    }

    @Test
    fun `a renegotiation never drops the audio section`() {
        // The microphone's m-line is negotiated once at dial and stays, muted
        // or not, so Mute never costs a renegotiation.
        listOf(true, false).forEach { assertTrue(renegotiationMedia(it).audio) }
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
