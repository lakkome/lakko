// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.en
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
        micOn: Boolean = true,
        cameraOn: Boolean = false,
        speakerOn: Boolean = false,
        cameraBusy: Boolean = false,
        ringing: Boolean = false,
    ) = callControlRow(
        group = group,
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
    fun `the row is the same four buttons whatever the call started as`() {
        val expected = listOf(
            CallControlKind.SPEAKER,
            CallControlKind.MICROPHONE,
            CallControlKind.CAMERA,
            CallControlKind.END,
        )
        assertEquals(expected, row(cameraOn = false).kinds())
        assertEquals(expected, row(cameraOn = true).kinds())
    }

    @Test
    fun `a voice call has a camera button`() {
        // Section 4.1: "People start a call with their voice and decide to show
        // something a minute later; every messenger allows that, and taking it
        // away to keep the row short would be a feature lost to tidiness."
        // Pressing it is what turns the call into a video one, so a row without
        // it would strand a voice call with no way in.
        assertNotNull(
            row(cameraOn = false).firstOrNull { it.kind == CallControlKind.CAMERA },
        )
    }

    @Test
    fun `the camera button survives being turned off again`() {
        // "Turning the camera off again returns the call to the portrait" — the
        // portrait, not a row with one fewer button, or the user could never
        // turn it back on.
        assertEquals(row(cameraOn = true).kinds(), row(cameraOn = false).kinds())
    }

    @Test
    fun `speaker is on every call`() {
        // It was on none of them: the control did not exist anywhere in the app,
        // although it is the one people reach for most on a voice call.
        listOf(
            row(cameraOn = false),
            row(cameraOn = true),
            row(group = true, cameraOn = true),
        ).forEach { assertNotNull(it.firstOrNull { c -> c.kind == CallControlKind.SPEAKER }) }
    }

    @Test
    fun `a one to one call has no more menu`() {
        listOf(row(cameraOn = false), row(cameraOn = true)).forEach { controls ->
            assertNull(
                "a 1x1 call hides nothing behind an ellipsis",
                controls.firstOrNull { it.kind == CallControlKind.MORE },
            )
        }
    }

    @Test
    fun `a group call keeps the overflow but not at the expense of the row`() {
        val controls = row(group = true, cameraOn = true)
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
            row(cameraOn = false),
            row(cameraOn = true),
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
            row(cameraOn = true).of(CallControlKind.CAMERA).emphasis,
        )
        assertEquals(
            CallControlEmphasis.SOLID,
            row(cameraOn = false).of(CallControlKind.CAMERA).emphasis,
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
        val controls = row(group = true, cameraOn = true, micOn = false, speakerOn = true)
        assertEquals(
            listOf(CallControlKind.END),
            controls.filter { it.emphasis == CallControlEmphasis.DANGER }.kinds(),
        )
    }

    @Test
    fun `labels name the action and never the state`() {
        assertEquals("Speaker", row().of(CallControlKind.SPEAKER).label.en)
        assertEquals("End", row().of(CallControlKind.END).label.en)
        // "The label ... says what the button does, not what the state is": the
        // same word whichever way the state is.
        assertEquals(
            row(micOn = true).of(CallControlKind.MICROPHONE).label.en,
            row(micOn = false).of(CallControlKind.MICROPHONE).label.en,
        )
        assertEquals("Mute", row(micOn = false).of(CallControlKind.MICROPHONE).label.en)
        assertEquals(
            row(cameraOn = true).of(CallControlKind.CAMERA).label.en,
            row(cameraOn = false).of(CallControlKind.CAMERA).label.en,
        )
        assertEquals("Camera", row().of(CallControlKind.CAMERA).label.en)
    }

    @Test
    fun `every control carries a spoken label naming the resulting state`() {
        val controls = row(group = true, cameraOn = true)
        controls.forEach { assertTrue(it.kind.name, it.spokenLabel.en.isNotBlank()) }
        assertEquals("Mute microphone", row(micOn = true).of(CallControlKind.MICROPHONE).spokenLabel.en)
        assertEquals(
            "Unmute microphone",
            row(micOn = false).of(CallControlKind.MICROPHONE).spokenLabel.en,
        )
        assertEquals("Turn on speaker", row(speakerOn = false).of(CallControlKind.SPEAKER).spokenLabel.en)
        assertEquals("Turn off speaker", row(speakerOn = true).of(CallControlKind.SPEAKER).spokenLabel.en)
        assertEquals("End call", row().of(CallControlKind.END).spokenLabel.en)
    }

    // ---- State ---------------------------------------------------------------

    @Test
    fun `while calling only end is active`() {
        val controls = row(group = true, cameraOn = true, ringing = true)
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
        assertEquals(row().kinds(), row(ringing = true).kinds())
    }

    @Test
    fun `once the call is up every control is active`() {
        row(group = true, cameraOn = true, ringing = false).forEach {
            assertTrue(it.kind.name, it.enabled)
        }
    }

    @Test
    fun `an acquiring camera is busy and untappable but end still works`() {
        val controls = row(cameraBusy = true)
        val camera = controls.of(CallControlKind.CAMERA)
        assertTrue(camera.busy)
        assertFalse(camera.enabled)
        assertEquals("Starting camera...", camera.spokenLabel.en)
        assertTrue(controls.of(CallControlKind.END).enabled)
        assertTrue(controls.of(CallControlKind.MICROPHONE).enabled)
    }

    @Test
    fun `nothing but the camera is ever busy`() {
        row(group = true, cameraBusy = true)
            .filter { it.kind != CallControlKind.CAMERA }
            .forEach { assertFalse(it.kind.name, it.busy) }
    }

    // ---- Fitting the row to the screen ---------------------------------------

    @Test
    fun `five controls fit a 411 dp phone with the end button on the screen`() {
        // The emulator that showed the defect: 1080 px at 420 dpi = 411 dp, the
        // bar inset 12 dp each side.
        val fit = fitCallControlBar(availableWidthDp = 411f - 24f, count = 5)
        val row = 5 * fit.columnDp + 5 * CALL_CONTROL_GAP_DP + 2 * CALL_CONTROL_BAR_PADDING_DP
        assertTrue("row $row must fit 387 dp", row <= 387f + 0.01f)
        assertTrue(fit.endDiameterDp <= fit.columnDp)
        assertTrue(fit.diameterDp >= CALL_CONTROL_MIN_DIAMETER_DP)
        assertTrue(fit.endDiameterDp > fit.diameterDp)
    }

    @Test
    fun `with room to spare the controls keep the spec sizes`() {
        val fit = fitCallControlBar(availableWidthDp = 800f, count = 5)
        assertEquals(CALL_CONTROL_DIAMETER_DP, fit.diameterDp, 0.001f)
        assertEquals(CALL_CONTROL_END_DIAMETER_DP, fit.endDiameterDp, 0.001f)
        assertEquals(CALL_CONTROL_END_DIAMETER_DP, fit.columnDp, 0.001f)
    }

    @Test
    fun `a narrow phone never drops a control under the minimum touch target`() {
        val fit = fitCallControlBar(availableWidthDp = 320f - 24f, count = 5)
        assertTrue(fit.diameterDp >= CALL_CONTROL_MIN_DIAMETER_DP)
        assertTrue(fit.endDiameterDp >= CALL_CONTROL_MIN_DIAMETER_DP)
    }

    @Test
    fun `the four-control row of a one-to-one call fits at spec sizes on a 411 dp phone`() {
        val fit = fitCallControlBar(availableWidthDp = 411f - 24f, count = 4)
        assertEquals(CALL_CONTROL_DIAMETER_DP, fit.diameterDp, 0.001f)
        assertEquals(CALL_CONTROL_END_DIAMETER_DP, fit.endDiameterDp, 0.001f)
    }
}
