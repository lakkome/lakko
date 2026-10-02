// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.en
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/calls.md` sections 4.1 and 4.1.1 — which controls exist and how each
 * one reads. Every fault section 4.1 names has a test here, so the row cannot
 * drift back into the shape the maintainer rejected.
 */
class CallControlsTest {
    init {
        EnglishForTests.install()
    }

    private fun row(
        micOn: Boolean = true,
        cameraOn: Boolean = false,
        cameraBusy: Boolean = false,
        ringing: Boolean = false,
        participants: ParticipantsControl = ParticipantsControl.ABSENT,
        participantsOpen: Boolean = false,
    ) = callControlRow(
        micOn = micOn,
        cameraOn = cameraOn,
        cameraBusy = cameraBusy,
        ringing = ringing,
        participants = participants,
        participantsOpen = participantsOpen,
    )

    private fun List<CallControl>.kinds() = map { it.kind }

    private fun List<CallControl>.of(kind: CallControlKind) = single { it.kind == kind }

    // ---- Which controls exist ------------------------------------------------

    @Test
    fun `a one-to-one row is microphone, camera, settings, end, whatever the call started as`() {
        val expected = listOf(
            CallControlKind.MICROPHONE,
            CallControlKind.CAMERA,
            CallControlKind.SETTINGS,
            CallControlKind.END,
        )
        assertEquals(expected, row(cameraOn = false).kinds())
        assertEquals(expected, row(cameraOn = true).kinds())
    }

    @Test
    fun `a group row adds participants before settings and nothing else`() {
        assertEquals(
            listOf(
                CallControlKind.MICROPHONE,
                CallControlKind.CAMERA,
                CallControlKind.PARTICIPANTS,
                CallControlKind.SETTINGS,
                CallControlKind.END,
            ),
            row(participants = ParticipantsControl.MENU).kinds(),
        )
        assertEquals(
            row(participants = ParticipantsControl.MENU).kinds(),
            row(participants = ParticipantsControl.LIST).kinds(),
        )
    }

    @Test
    fun `the row has no chat, no speaker and no overflow menu`() {
        // The chat control sent nothing anywhere; raise hand and share screen
        // changed a flag nothing read; the overflow menu held only those. The
        // sound's route is a rule and a Settings choice, not a button.
        val names = CallControlKind.entries.map { it.name }
        for (gone in listOf("CHAT", "MORE", "SPEAKER", "INVITE", "HAND", "SHARE")) {
            assertFalse(gone, names.any { it.contains(gone) })
        }
        assertEquals(5, CallControlKind.entries.size)
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
    fun `end is always last`() {
        listOf(
            row(cameraOn = false),
            row(cameraOn = true),
            row(participants = ParticipantsControl.MENU),
        ).forEach { assertEquals(CallControlKind.END, it.last().kind) }
    }

    @Test
    fun `flip camera is never in the row`() {
        // Section 4.1 puts it on the self-view, "where the thing it changes is
        // visible". There is deliberately no kind for it here.
        assertFalse(CallControlKind.entries.any { it.name.contains("FLIP") })
    }

    // ---- What Participants does ----------------------------------------------

    @Test
    fun `a one-to-one call has no participants control`() {
        assertEquals(
            ParticipantsControl.ABSENT,
            participantsControl(group = false, canInvite = false, othersPresent = true),
        )
    }

    @Test
    fun `a call that can hand out a link opens the menu, alone or not`() {
        assertEquals(
            ParticipantsControl.MENU,
            participantsControl(group = true, canInvite = true, othersPresent = false),
        )
        assertEquals(
            ParticipantsControl.MENU,
            participantsControl(group = true, canInvite = true, othersPresent = true),
        )
    }

    @Test
    fun `a call in a conversation opens the list directly, and has no control while alone`() {
        // A menu holding one item would be a second step for nothing; and
        // alone, the list would hold one name, the person's own.
        assertEquals(
            ParticipantsControl.LIST,
            participantsControl(group = true, canInvite = false, othersPresent = true),
        )
        assertEquals(
            ParticipantsControl.ABSENT,
            participantsControl(group = true, canInvite = false, othersPresent = false),
        )
    }

    @Test
    fun `the menu is participants, copy invite link, invite contacts, in that order`() {
        assertEquals(
            listOf(
                ParticipantsMenuItem.PARTICIPANTS,
                ParticipantsMenuItem.COPY_LINK,
                ParticipantsMenuItem.INVITE_CONTACTS,
            ),
            participantsMenuItems(othersPresent = true, hasConversations = true),
        )
    }

    @Test
    fun `alone in the call the menu is the two invite items`() {
        assertEquals(
            listOf(ParticipantsMenuItem.COPY_LINK, ParticipantsMenuItem.INVITE_CONTACTS),
            participantsMenuItems(othersPresent = false, hasConversations = true),
        )
    }

    @Test
    fun `someone with no conversations has no invite contacts item`() {
        assertEquals(
            listOf(ParticipantsMenuItem.COPY_LINK),
            participantsMenuItems(othersPresent = false, hasConversations = false),
        )
        assertEquals(
            listOf(ParticipantsMenuItem.PARTICIPANTS, ParticipantsMenuItem.COPY_LINK),
            participantsMenuItems(othersPresent = true, hasConversations = false),
        )
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
    fun `participants and settings open something and are never left switched on`() {
        val controls = row(participants = ParticipantsControl.LIST, participantsOpen = true)
        assertEquals(CallControlEmphasis.OUTLINE, controls.of(CallControlKind.PARTICIPANTS).emphasis)
        assertEquals(CallControlEmphasis.OUTLINE, controls.of(CallControlKind.SETTINGS).emphasis)
    }

    @Test
    fun `only end is red`() {
        val controls = row(participants = ParticipantsControl.MENU, cameraOn = true, micOn = false)
        assertEquals(
            listOf(CallControlKind.END),
            controls.filter { it.emphasis == CallControlEmphasis.DANGER }.kinds(),
        )
    }

    @Test
    fun `labels name the thing and never the state`() {
        assertEquals("End", row().of(CallControlKind.END).label.en)
        assertEquals("Settings", row().of(CallControlKind.SETTINGS).label.en)
        assertEquals(
            "Participants",
            row(participants = ParticipantsControl.MENU).of(CallControlKind.PARTICIPANTS).label.en,
        )
        // The same word whichever way the state is.
        assertEquals(
            row(micOn = true).of(CallControlKind.MICROPHONE).label.en,
            row(micOn = false).of(CallControlKind.MICROPHONE).label.en,
        )
        assertEquals("Microphone", row(micOn = false).of(CallControlKind.MICROPHONE).label.en)
        assertEquals(
            row(cameraOn = true).of(CallControlKind.CAMERA).label.en,
            row(cameraOn = false).of(CallControlKind.CAMERA).label.en,
        )
        assertEquals("Camera", row().of(CallControlKind.CAMERA).label.en)
    }

    @Test
    fun `every control carries a spoken label naming the resulting state`() {
        val controls = row(participants = ParticipantsControl.MENU, cameraOn = true)
        controls.forEach { assertTrue(it.kind.name, it.spokenLabel.en.isNotBlank()) }
        assertEquals("Mute microphone", row(micOn = true).of(CallControlKind.MICROPHONE).spokenLabel.en)
        assertEquals(
            "Unmute microphone",
            row(micOn = false).of(CallControlKind.MICROPHONE).spokenLabel.en,
        )
        assertEquals("Call settings", row().of(CallControlKind.SETTINGS).spokenLabel.en)
        assertEquals("End call", row().of(CallControlKind.END).spokenLabel.en)
    }

    @Test
    fun `participants says what a press does, the menu or showing and hiding the list`() {
        assertEquals(
            "Participants and invitations",
            row(participants = ParticipantsControl.MENU).of(CallControlKind.PARTICIPANTS).spokenLabel.en,
        )
        assertEquals(
            "Show participants",
            row(participants = ParticipantsControl.LIST).of(CallControlKind.PARTICIPANTS).spokenLabel.en,
        )
        assertEquals(
            "Hide participants",
            row(participants = ParticipantsControl.LIST, participantsOpen = true)
                .of(CallControlKind.PARTICIPANTS).spokenLabel.en,
        )
    }

    // ---- State ---------------------------------------------------------------

    @Test
    fun `while calling the microphone and the camera wait, and the rest is live`() {
        val controls = row(cameraOn = true, ringing = true)
        assertFalse(controls.of(CallControlKind.MICROPHONE).enabled)
        assertFalse(controls.of(CallControlKind.CAMERA).enabled)
        assertTrue("end must always work", controls.of(CallControlKind.END).enabled)
        // Settings is active from the first second of a call.
        assertTrue(controls.of(CallControlKind.SETTINGS).enabled)
    }

    @Test
    fun `a disabled control is still in the row`() {
        // "Disabled is 40 percent opacity, never a hidden button."
        assertEquals(row().kinds(), row(ringing = true).kinds())
    }

    @Test
    fun `once the call is up every control is active`() {
        row(participants = ParticipantsControl.MENU, cameraOn = true, ringing = false).forEach {
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
        row(participants = ParticipantsControl.MENU, cameraBusy = true)
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

    // ---- The group row -------------------------------------------------------

    @Test
    fun `a group row on a phone has no words and always fits in one line`() {
        // A row that wrapped into two lines of labelled circles took half a
        // phone screen (found live 2026-10-02).
        for (screenWidth in 300..599) {
            for (count in 4..6) {
                val available = screenWidth - 2 * GROUP_CONTROL_SIDE_MARGIN_DP
                val fit = fitGroupControlBar(available, screenHeightDp = 800f, count = count)
                assertFalse("$screenWidth/$count", fit.showLabels)
                assertTrue(
                    "$screenWidth dp, $count controls: ${fit.widthDp(count)} > $available",
                    fit.widthDp(count) <= available + 0.01f,
                )
                assertTrue(fit.diameterDp in 38f..52f)
                assertEquals(fit.diameterDp + 6f, fit.endDiameterDp, 0.001f)
            }
        }
    }

    @Test
    fun `the circles are sized from the width and the number of controls`() {
        // A 411 dp phone, five controls: the web client's own arithmetic,
        // (411 - 50 - 5 * 8) / 5.
        val five = fitGroupControlBar(411f - 24f, screenHeightDp = 800f, count = 5)
        assertEquals(52f, five.diameterDp, 0.001f)
        // A 360 dp phone, six controls: (360 - 50 - 48) / 6.
        val six = fitGroupControlBar(360f - 24f, screenHeightDp = 800f, count = 6)
        assertEquals((360f - 50f - 48f) / 6f, six.diameterDp, 0.001f)
        assertEquals(8f, six.gapDp, 0.001f)
        // Fewer controls on the same width are larger, up to the cap.
        val four = fitGroupControlBar(320f - 24f, screenHeightDp = 800f, count = 4)
        val fiveNarrow = fitGroupControlBar(320f - 24f, screenHeightDp = 800f, count = 5)
        assertTrue(four.diameterDp > fiveNarrow.diameterDp)
    }

    @Test
    fun `on a width too narrow for the smallest circles the gaps close up, not the row`() {
        val fit = fitGroupControlBar(300f - 24f, screenHeightDp = 800f, count = 6)
        assertEquals(38f, fit.diameterDp, 0.001f)
        assertTrue(fit.gapDp < 8f)
        assertTrue(fit.widthDp(6) <= 300f - 24f + 0.01f)
    }

    @Test
    fun `a phone on its side has the compact row too`() {
        val fit = fitGroupControlBar(800f - 24f, screenHeightDp = 400f, count = 5)
        assertFalse(fit.showLabels)
        assertEquals(52f, fit.diameterDp, 0.001f)
    }

    @Test
    fun `with room the group row keeps its words and its spec sizes`() {
        val fit = fitGroupControlBar(800f - 24f, screenHeightDp = 900f, count = 5)
        assertTrue(fit.showLabels)
        assertEquals(56f, fit.diameterDp, 0.001f)
        assertEquals(64f, fit.endDiameterDp, 0.001f)
    }
}
