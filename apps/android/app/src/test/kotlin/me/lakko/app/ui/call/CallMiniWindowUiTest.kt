// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasClickAction
import androidx.compose.ui.test.hasContentDescription
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.unit.dp
import me.lakko.app.calls.CallBarModel
import me.lakko.app.calls.callBarMicControl
import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import uniffi.lakko_ffi_uniffi.CallBarStatus

/**
 * `docs/ux/calls.md` section 4.5.1 — the collapsed call's floating window,
 * drawn. It replaced the bar across the top of the app (2026-11-09) and keeps
 * every behaviour the bar had: a tap brings the call back, the one mute button
 * mutes without bringing it back, there is no End, the line carries the state
 * or the time and never both, and the state is a live region.
 *
 * The real [CallMiniWindowFace] over a [CallBarModel] built here, with a plain
 * box for the picture: the picture is a WebRTC surface or an avatar, and
 * neither is what is under test.
 *
 * ## Not reached, named rather than approximated
 *
 *  - **Whether there is a window at all** is the shared `call_bar` rule over
 *    UniFFI, which needs `liblakko_ffi_uniffi.so`; there is no host-JVM copy.
 *  - **Dragging, the corner and the tuck** are the pure functions in
 *    `me.lakko.app.calls.CallMiniWindow`, covered by `CallMiniWindowTest`; the
 *    gesture wiring around them needs a device.
 *  - **The system picture-in-picture window** is a platform window; its spec
 *    is covered by `CallPictureInPictureTest`.
 */
@RunWith(RobolectricTestRunner::class)
class CallMiniWindowUiTest {
    init {
        // ADR-0061: the words under test are the English catalog's.
        EnglishForTests.install()
    }

    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private var expands = 0
    private var pictureComposed = false
    private var insetComposed = false
    private val muteToggles = mutableListOf<Boolean>()

    @Composable
    private fun WindowUnderTest(
        model: CallBarModel,
        shownName: String = SHOWN,
        peopleBeyond: Int = 0,
        withInset: Boolean = false,
        compact: Boolean = false,
    ) {
        MaterialTheme {
            Box(Modifier.size(width = 140.dp, height = 187.dp).testTag(WINDOW)) {
                CallMiniWindowFace(
                    model = model,
                    shownName = shownName,
                    peopleBeyond = peopleBeyond,
                    insetAtStart = true,
                    onExpand = { expands += 1 },
                    onToggleMute = { on -> muteToggles += on },
                    picture = {
                        pictureComposed = true
                        Box(Modifier)
                    },
                    ownInset = if (withInset) {
                        {
                            insetComposed = true
                            Box(Modifier)
                        }
                    } else {
                        null
                    },
                    compact = compact,
                )
            }
        }
    }

    private fun model(
        status: CallBarStatus,
        micOn: Boolean = true,
        showsLock: Boolean = false,
        cameraSending: Boolean = false,
    ) = CallBarModel(
        who = WHO,
        status = status,
        showsLock = showsLock,
        cameraSending = cameraSending,
        mic = callBarMicControl(micOn = micOn, ringing = status == CallBarStatus.Calling),
        micOn = micOn,
    )

    private fun assertNotOnTheWindow(vararg text: String) {
        for (t in text) compose.onNodeWithText(t).assertDoesNotExist()
    }

    // ----- what it shows ----------------------------------------------------

    @Test
    fun aConnectingCallShowsTheStateWordAndNoClock() {
        compose.setContent { WindowUnderTest(model(status = CallBarStatus.Connecting)) }

        compose.onNodeWithText(CONNECTING, useUnmergedTree = true).assertExists()
        assertNotOnTheWindow(ELAPSED_CLOCK, CALLING, RECONNECTING)
    }

    @Test
    fun aConnectedCallShowsTheRunningTimeAndNoStateWord() {
        compose.setContent { WindowUnderTest(model(status = CallBarStatus.Elapsed(ELAPSED_MS))) }

        compose.onNodeWithText(ELAPSED_CLOCK, useUnmergedTree = true).assertExists()
        assertNotOnTheWindow(CONNECTING, CALLING, RECONNECTING, CONNECTED_WORD)
    }

    @Test
    fun theWindowNamesThePersonShownAndNotTheCallsLabel() {
        // Section 4.5.1: "The call's label is not printed in the window: the
        // picture and the name say who it is with, and the label stays the
        // window's spoken name."
        compose.setContent {
            WindowUnderTest(model(status = CallBarStatus.Elapsed(ELAPSED_MS)), peopleBeyond = 16)
        }

        compose.onNodeWithText(SHOWN, useUnmergedTree = true).assertExists()
        compose.onNodeWithText("+16", useUnmergedTree = true).assertExists()
        compose.onNodeWithText(WHO, useUnmergedTree = true).assertDoesNotExist()
        compose.onNode(hasContentDescription(WHO, substring = true)).assertExists()
    }

    @Test
    fun noPlusWhenNobodyIsBeyondThePersonShown() {
        compose.setContent { WindowUnderTest(model(status = CallBarStatus.Elapsed(ELAPSED_MS))) }

        compose.onNodeWithText("+0", useUnmergedTree = true).assertDoesNotExist()
    }

    @Test
    fun theOwnCameraInsetIsDrawnOnlyWhenGiven() {
        compose.setContent { WindowUnderTest(model(status = CallBarStatus.Connecting), withInset = true) }
        compose.waitForIdle()
        assertEquals(true, insetComposed)
        assertEquals(true, pictureComposed)
    }

    @Test
    fun noInsetWhenTheOwnCameraIsOff() {
        compose.setContent { WindowUnderTest(model(status = CallBarStatus.Connecting), withInset = false) }
        compose.waitForIdle()
        assertEquals(false, insetComposed)
        assertEquals(true, pictureComposed)
    }

    @Test
    fun theStateIsALiveRegion() {
        // Section 4.5.6 "Spoken": state changes announce as a live region, so
        // `Reconnecting...` is heard and not only seen.
        compose.setContent { WindowUnderTest(model(status = CallBarStatus.Reconnecting)) }

        compose.onNode(
            SemanticsMatcher.keyIsDefined(SemanticsProperties.LiveRegion)
                .and(hasContentDescription(RECONNECTING_SPOKEN, substring = true)),
        ).assertExists()
    }

    @Test
    fun theLiveRegionCarriesNoClock() {
        // A live region holding the running time would be read out every
        // second; it holds the state word alone.
        compose.setContent { WindowUnderTest(model(status = CallBarStatus.Elapsed(ELAPSED_MS))) }

        val live = compose.onAllNodes(SemanticsMatcher.keyIsDefined(SemanticsProperties.LiveRegion))
            .fetchSemanticsNodes()
        assertEquals(1, live.size)
        val said = live.single().config[SemanticsProperties.ContentDescription].joinToString()
        assertEquals(false, said.contains("42"))
    }

    // ----- tapping it brings the call back --------------------------------

    @Test
    fun tappingTheWindowBringsTheCallBack() {
        compose.setContent { WindowUnderTest(model(status = CallBarStatus.Elapsed(ELAPSED_MS))) }

        compose.onNode(hasContentDescription(RETURN, substring = true)).performClick()

        assertEquals("the window is not wired to bringing the call back", 1, expands)
        assertEquals("expanding the call also pressed mute", emptyList<Boolean>(), muteToggles)
    }

    @Test
    fun theMuteButtonMutesAndDoesNotBringTheCallBack() {
        compose.setContent {
            WindowUnderTest(model(status = CallBarStatus.Elapsed(ELAPSED_MS), micOn = true))
        }

        compose.onNodeWithContentDescription(MUTE).performClick()

        assertEquals("the mute button reported the wrong next state", listOf(false), muteToggles)
        assertEquals("pressing mute brought the call screen back", 0, expands)
    }

    @Test
    fun aMutedMicrophoneOffersUnmute() {
        compose.setContent {
            WindowUnderTest(model(status = CallBarStatus.Elapsed(ELAPSED_MS), micOn = false))
        }

        compose.onNodeWithContentDescription(UNMUTE).performClick()

        assertEquals(listOf(true), muteToggles)
    }

    // ----- there is no End on the window ------------------------------------

    @Test
    fun theWindowCarriesNoEndControl() {
        compose.setContent { WindowUnderTest(model(status = CallBarStatus.Elapsed(ELAPSED_MS))) }

        compose.onNodeWithText(END_LABEL).assertDoesNotExist()
        compose.onNodeWithContentDescription(END_SPOKEN).assertDoesNotExist()
        assertEquals(
            "the window has something to press beyond the return and the one mute button",
            2,
            compose.onAllNodes(hasClickAction()).fetchSemanticsNodes().size,
        )
    }

    @Test
    fun theWindowCarriesNoEndControlWhileItIsStillCalling() {
        compose.setContent { WindowUnderTest(model(status = CallBarStatus.Calling)) }

        compose.onNodeWithText(END_LABEL).assertDoesNotExist()
        compose.onNodeWithContentDescription(END_SPOKEN).assertDoesNotExist()
        compose.onNodeWithText(CALLING, useUnmergedTree = true).assertExists()
    }

    // ----- the system picture-in-picture face -------------------------------

    @Test
    fun theSystemWindowFaceHasNothingToPress() {
        // Section 4.5.4: the system window's controls are the system's, and
        // its one action is the microphone; the face itself takes no press.
        compose.setContent {
            WindowUnderTest(model(status = CallBarStatus.Elapsed(ELAPSED_MS)), compact = true)
        }

        assertEquals(0, compose.onAllNodes(hasClickAction()).fetchSemanticsNodes().size)
        compose.onNodeWithContentDescription(MUTE).assertDoesNotExist()
        compose.onNodeWithText(SHOWN, useUnmergedTree = true).assertExists()
        compose.onNodeWithTag(WINDOW).assertIsDisplayed()
    }

    private companion object {
        const val WINDOW = "call-mini-window"
        const val WHO = "Design review"
        const val SHOWN = "Anna"

        /** 42 s, which `formatCallDuration` renders as `0:42`. */
        val ELAPSED_MS: ULong = 42_000uL
        const val ELAPSED_CLOCK = "0:42"

        const val CONNECTING = "Connecting..."
        const val CALLING = "Calling..."
        const val RECONNECTING = "Reconnecting..."
        const val CONNECTED_WORD = "Connected"

        /** The start of the return target's spoken description. */
        const val RETURN = "Return to the call."

        const val MUTE = "Mute microphone"
        const val UNMUTE = "Unmute microphone"

        const val END_LABEL = "End"
        const val END_SPOKEN = "End call"
    }

    /** The spoken word for `Reconnecting...`, read from the catalog the test
     *  installed rather than restated. */
    private val RECONNECTING_SPOKEN: String
        get() = me.lakko.app.l10n.Localization.strings.value["call.status.reconnecting_spoken"]
}
