// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/calls.md` sections 4.1 and 4.1.1 — which controls exist and how each
 * one reads. Every fault section 4.1 names has a test here, so the row cannot
 * drift back into the shape the maintainer rejected.
 */
class CallControlsTest {

    private fun row(
        group: Boolean = false,
        videoCall: Boolean = false,
        micOn: Boolean = true,
        cameraOn: Boolean = false,
        speakerOn: Boolean = false,
        cameraBusy: Boolean = false,
        ringing: Boolean = false,
    ) = callControlRow(
        group = group,
        videoCall = videoCall,
        micOn = micOn,
        cameraOn = cameraOn,
        speakerOn = speakerOn,
        cameraBusy = cameraBusy,
        ringing = ringing,
    )

    private fun List<CallControl>.kinds() = map { it.kind }

    private fun List<CallControl>.of(kind: CallControlKind) = single { it.kind == kind }

    // ---- Which controls exist ------------------------------------------------

    @Test
    fun `a voice call shows speaker, mute and end`() {
        assertEquals(
            listOf(CallControlKind.SPEAKER, CallControlKind.MICROPHONE, CallControlKind.END),
            row(videoCall = false).kinds(),
        )
    }

    @Test
    fun `a video call adds the camera`() {
        assertEquals(
            listOf(
                CallControlKind.SPEAKER,
                CallControlKind.MICROPHONE,
                CallControlKind.CAMERA,
                CallControlKind.END,
            ),
            row(videoCall = true).kinds(),
        )
    }

    @Test
    fun `speaker is on every call`() {
        // It was on none of them: the control did not exist anywhere in the app,
        // although it is the one people reach for most on a voice call.
        listOf(
            row(videoCall = false),
            row(videoCall = true),
            row(group = true, videoCall = true),
        ).forEach { assertNotNull(it.firstOrNull { c -> c.kind == CallControlKind.SPEAKER }) }
    }

    @Test
    fun `a one to one call has no more menu`() {
        listOf(row(videoCall = false), row(videoCall = true)).forEach { controls ->
            assertNull(
                "a 1x1 call hides nothing behind an ellipsis",
                controls.firstOrNull { it.kind == CallControlKind.MORE },
            )
        }
    }

    @Test
    fun `a group call keeps the overflow but not at the expense of the row`() {
        val controls = row(group = true, videoCall = true)
        assertTrue(controls.kinds().contains(CallControlKind.MORE))
        // "but mute, camera, speaker and end stay in the row"
        listOf(
            CallControlKind.SPEAKER,
            CallControlKind.MICROPHONE,
            CallControlKind.CAMERA,
            CallControlKind.END,
        ).forEach { assertTrue(it.name, controls.kinds().contains(it)) }
    }

    @Test
    fun `end is always last`() {
        listOf(
            row(videoCall = false),
            row(videoCall = true),
            row(group = true),
        ).forEach { assertEquals(CallControlKind.END, it.last().kind) }
    }

    @Test
    fun `flip camera is never in the row`() {
        // Section 4.1 puts it on the self-view, "where the thing it changes is
        // visible". There is deliberately no kind for it here.
        assertFalse(CallControlKind.values().any { it.name.contains("FLIP") })
    }

    // ---- How each one reads --------------------------------------------------

    @Test
    fun `a live microphone is the quiet state and a muted one is the loud state`() {
        // The old row had this backwards: `active = local.micOn == false` filled
        // the button with the accent colour, so muted looked like live.
        assertEquals(
            CallControlEmphasis.OUTLINE,
            row(micOn = true).of(CallControlKind.MICROPHONE).emphasis,
        )
        assertEquals(
            CallControlEmphasis.SOLID,
            row(micOn = false).of(CallControlKind.MICROPHONE).emphasis,
        )
    }

    @Test
    fun `a sending camera is the quiet state and a stopped one is the loud state`() {
        assertEquals(
            CallControlEmphasis.OUTLINE,
            row(videoCall = true, cameraOn = true).of(CallControlKind.CAMERA).emphasis,
        )
        assertEquals(
            CallControlEmphasis.SOLID,
            row(videoCall = true, cameraOn = false).of(CallControlKind.CAMERA).emphasis,
        )
    }

    @Test
    fun `speaker is loud when it is engaged`() {
        // The deviation from the default route is what everyone nearby can
        // hear, so it is the state worth seeing at a glance.
        assertEquals(
            CallControlEmphasis.SOLID,
            row(speakerOn = true).of(CallControlKind.SPEAKER).emphasis,
        )
        assertEquals(
            CallControlEmphasis.OUTLINE,
            row(speakerOn = false).of(CallControlKind.SPEAKER).emphasis,
        )
    }

    @Test
    fun `only end is red`() {
        val controls = row(group = true, videoCall = true, micOn = false, speakerOn = true)
        assertEquals(
            listOf(CallControlKind.END),
            controls.filter { it.emphasis == CallControlEmphasis.DANGER }.kinds(),
        )
    }

    @Test
    fun `labels name the action and never the state`() {
        assertEquals("Speaker", row().of(CallControlKind.SPEAKER).label)
        assertEquals("End", row().of(CallControlKind.END).label)
        // "The label ... says what the button does, not what the state is": the
        // same word whichever way the state is.
        assertEquals(
            row(micOn = true).of(CallControlKind.MICROPHONE).label,
            row(micOn = false).of(CallControlKind.MICROPHONE).label,
        )
        assertEquals("Mute", row(micOn = false).of(CallControlKind.MICROPHONE).label)
        assertEquals(
            row(videoCall = true, cameraOn = true).of(CallControlKind.CAMERA).label,
            row(videoCall = true, cameraOn = false).of(CallControlKind.CAMERA).label,
        )
        assertEquals("Camera", row(videoCall = true).of(CallControlKind.CAMERA).label)
    }

    @Test
    fun `every control carries a spoken label naming the resulting state`() {
        val controls = row(group = true, videoCall = true)
        controls.forEach { assertTrue(it.kind.name, it.spokenLabel.isNotBlank()) }
        assertEquals("Mute microphone", row(micOn = true).of(CallControlKind.MICROPHONE).spokenLabel)
        assertEquals(
            "Unmute microphone",
            row(micOn = false).of(CallControlKind.MICROPHONE).spokenLabel,
        )
        assertEquals("Turn on speaker", row(speakerOn = false).of(CallControlKind.SPEAKER).spokenLabel)
        assertEquals("Turn off speaker", row(speakerOn = true).of(CallControlKind.SPEAKER).spokenLabel)
        assertEquals("End call", row().of(CallControlKind.END).spokenLabel)
    }

    // ---- State ---------------------------------------------------------------

    @Test
    fun `while calling only end is active`() {
        val controls = row(group = true, videoCall = true, ringing = true)
        controls.forEach {
            if (it.kind == CallControlKind.END) {
                assertTrue("end must always work", it.enabled)
            } else {
                assertFalse(it.kind.name, it.enabled)
            }
        }
    }

    @Test
    fun `a disabled control is still in the row`() {
        // "Disabled is 40 percent opacity, never a hidden button."
        assertEquals(row(videoCall = true).kinds(), row(videoCall = true, ringing = true).kinds())
    }

    @Test
    fun `once the call is up every control is active`() {
        row(group = true, videoCall = true, ringing = false).forEach {
            assertTrue(it.kind.name, it.enabled)
        }
    }

    @Test
    fun `an acquiring camera is busy and untappable but end still works`() {
        val controls = row(videoCall = true, cameraBusy = true)
        val camera = controls.of(CallControlKind.CAMERA)
        assertTrue(camera.busy)
        assertFalse(camera.enabled)
        assertEquals("Starting camera", camera.spokenLabel)
        assertTrue(controls.of(CallControlKind.END).enabled)
        assertTrue(controls.of(CallControlKind.MICROPHONE).enabled)
    }

    @Test
    fun `nothing but the camera is ever busy`() {
        row(group = true, videoCall = true, cameraBusy = true)
            .filter { it.kind != CallControlKind.CAMERA }
            .forEach { assertFalse(it.kind.name, it.busy) }
    }
}
