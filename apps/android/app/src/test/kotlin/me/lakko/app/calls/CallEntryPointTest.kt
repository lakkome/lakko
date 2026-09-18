// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/calls.md` sections 3.1 and 3.2 — which surface each call button
 * opens. The rule this pins is the one the maintainer called out by name: a
 * voice call must not ask a question before dialling.
 */
class CallEntryPointTest {

    @Test
    fun `the phone button dials immediately`() {
        assertEquals(
            CallEntrySurface.DIAL_IMMEDIATELY,
            callEntrySurface(RequestedCallKind.VOICE, hasCamera = true),
        )
    }

    @Test
    fun `a voice call never opens a camera check even with a camera present`() {
        // The old screen showed the pre-join card with an empty preview box for
        // exactly this case, which is what made a voice call look like a
        // misconfigured video one.
        assertEquals(
            CallEntrySurface.DIAL_IMMEDIATELY,
            callEntrySurface(RequestedCallKind.VOICE, hasCamera = true),
        )
        assertEquals(
            CallEntrySurface.DIAL_IMMEDIATELY,
            callEntrySurface(RequestedCallKind.VOICE, hasCamera = false),
        )
    }

    @Test
    fun `the video button opens the camera check`() {
        assertEquals(
            CallEntrySurface.CAMERA_CHECK,
            callEntrySurface(RequestedCallKind.VIDEO, hasCamera = true),
        )
    }

    @Test
    fun `the video button without a camera places a voice call instead`() {
        assertEquals(
            CallEntrySurface.DIAL_IMMEDIATELY,
            callEntrySurface(RequestedCallKind.VIDEO, hasCamera = false),
        )
        val media = immediateDialMedia(RequestedCallKind.VIDEO, hasCamera = false)
        assertTrue("a call always carries an audio section", media.audio)
        assertFalse("nothing is offered that the device cannot send", media.video)
    }

    @Test
    fun `only the no-camera fallback says anything`() {
        // "says so in one line" — and a voice call behaving like a voice call
        // needs no explanation at all.
        assertNull(callEntryNotice(RequestedCallKind.VOICE, hasCamera = true))
        assertNull(callEntryNotice(RequestedCallKind.VOICE, hasCamera = false))
        assertNull(callEntryNotice(RequestedCallKind.VIDEO, hasCamera = true))
        assertEquals(
            NO_CAMERA_VOICE_FALLBACK_NOTICE,
            callEntryNotice(RequestedCallKind.VIDEO, hasCamera = false),
        )
    }

    @Test
    fun `the notice is one sentence and names no internals`() {
        val notice = NO_CAMERA_VOICE_FALLBACK_NOTICE
        assertEquals(
            "one sentence: $notice",
            1,
            notice.count { it == '.' },
        )
        // Design system section 5 — product copy names no mechanism.
        listOf("camera2", "webrtc", "capturer", "enumerator").forEach {
            assertFalse(it, notice.lowercase().contains(it))
        }
    }

    @Test
    fun `a video call with a camera dials with video once the check is passed`() {
        val media = immediateDialMedia(RequestedCallKind.VIDEO, hasCamera = true)
        assertTrue(media.audio)
        assertTrue(media.video)
    }

    @Test
    fun `a voice call never dials with video`() {
        listOf(true, false).forEach { camera ->
            val media = immediateDialMedia(RequestedCallKind.VOICE, hasCamera = camera)
            assertTrue(media.audio)
            assertFalse("camera present = $camera", media.video)
        }
    }
}
