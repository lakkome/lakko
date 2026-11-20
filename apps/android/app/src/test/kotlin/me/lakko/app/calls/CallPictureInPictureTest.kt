// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/calls.md` section 4.5.4 — what the system picture-in-picture window
 * is for a call, as [pictureInPictureSpec] decides it. The platform calls
 * around it (`setPictureInPictureParams`, `enterPictureInPictureMode`) need a
 * device.
 */
class CallPictureInPictureTest {
    init {
        EnglishForTests.install()
    }

    @Test
    fun aCallThatCannotBePutAwayHasNoSystemWindow() {
        // Ringing, ended, no call: the `call_bar` rule says no, and so does
        // this.
        assertNull(pictureInPictureSpec(live = false, screenShare = false, micOn = true, micEnabled = true))
    }

    @Test
    fun aPersonIsUpright34() {
        val spec = pictureInPictureSpec(live = true, screenShare = false, micOn = true, micEnabled = true)
        assertEquals(PipAspect(3, 4), spec?.aspect)
    }

    @Test
    fun aShareLies169() {
        val spec = pictureInPictureSpec(live = true, screenShare = true, micOn = true, micEnabled = true)
        assertEquals(PipAspect(16, 9), spec?.aspect)
    }

    @Test
    fun theOneActionIsTheMicrophoneNamedAsTheCallScreenNamesIt() {
        assertEquals("Mute microphone", pictureInPictureMicLabel(micOn = true).render())
        assertEquals("Unmute microphone", pictureInPictureMicLabel(micOn = false).render())
    }

    @Test
    fun theActionIsDeadWhileStillCalling() {
        val calling = callBarMicControl(micOn = true, ringing = true)
        val spec = pictureInPictureSpec(live = true, screenShare = false, micOn = true, micEnabled = calling.enabled)
        assertFalse(spec?.micEnabled ?: true)
        val live = callBarMicControl(micOn = true, ringing = false)
        assertTrue(
            pictureInPictureSpec(live = true, screenShare = false, micOn = true, micEnabled = live.enabled)?.micEnabled
                ?: false,
        )
    }

    @Test
    fun theSpecIsEqualAcrossTicksSoTheParametersAreNotResetEverySecond() {
        // The window's clock changes every second; the spec carries nothing
        // that does, so `CallPictureInPicture.update` does not reach the
        // platform on every tick.
        val a = pictureInPictureSpec(live = true, screenShare = false, micOn = true, micEnabled = true)
        val b = pictureInPictureSpec(live = true, screenShare = false, micOn = true, micEnabled = true)
        assertEquals(a, b)
    }
}
