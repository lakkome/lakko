// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import me.lakko.app.l10n.EnglishForTests
import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Box
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasClickAction
import androidx.compose.ui.test.hasContentDescription
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import me.lakko.app.calls.CallBarModel
import me.lakko.app.calls.callBarMicControl
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import uniffi.lakko_ffi_uniffi.CallBarStatus

/**
 * `docs/ux/calls.md` section 4.5 — the collapsed call bar, drawn.
 *
 * ## Why this file exists
 *
 * `me.lakko.app.calls.CollapsedCallBarTest` covers everything about the bar that
 * is not a composable, and said in as many words that it could not cover how the
 * bar LOOKS "because there is no Robolectric and no compose-ui-test on this
 * client". That was true when it was written; the harness landed afterwards, and
 * both `CollapsedCallBar` files carried the same sentence. This is the half those
 * comments described as out of reach.
 *
 * The bar is worth reaching for what it closed: an answered call owned the whole
 * app, and the system back did not even put it away, so a person on a call could
 * not look up the thing the call was about. Every DECISION about the bar is in
 * the shared rule; what is left on this side is whether the bar draws the answer,
 * and one wrong `if` in a composable is enough to lose it.
 *
 * ## What is reached
 *
 * The real [CollapsedCallBar] over a [CallBarModel] built here — built here
 * because building it is the one thing this composable does not do, and the pure
 * functions that build it are covered on the host side.
 *
 * ## What is not, named rather than approximated
 *
 *  - **Whether there is a bar at all.** Section 4.5's "no bar for a call that is
 *    only ringing in, one that is over, or one whose own screen is up" is
 *    `uniffi.lakko_ffi_uniffi.callBarVisible`, reached through
 *    `rememberCollapsedCallBar`. It is an FFI call needing
 *    `liblakko_ffi_uniffi.so`, and this project builds that library only for
 *    Android ABIs — there is no host-JVM copy, so no test in `src/test` can load
 *    it. The rule itself has property tests in
 *    `crates/lakko-ui-rules/src/call_bar.rs`; what has no test is the wiring from
 *    a phase to that call, and reaching it needs an instrumented `androidTest`
 *    suite on a device or emulator.
 *  - **The back key.** Back collapses because the call screen is a `Dialog` with
 *    `dismissOnBackPress = true` whose `onDismissRequest` is `host.collapse()` —
 *    a platform window path rather than anything a Robolectric test delivers.
 *    The half that can be asserted without the key is that a collapse never ENDS
 *    the call, and that is `OneToOneCallHostTest.collapsing a call puts it away
 *    and never ends it`, which asserts the slot, the state, the absence of a
 *    hang-up and the clock all untouched.
 *  - **The lock and the camera glyph.** Both are deliberately drawn with a null
 *    content description — section 4.5 gives the bar the lock but never the
 *    word, because a claim about encryption belongs where the safety number can
 *    be read — so neither reaches the semantics tree at all. A baseline image is
 *    the tool for those, alongside the ones in `MediaBaselineScreenshotTest`.
 */
@RunWith(RobolectricTestRunner::class)
class CollapsedCallBarUiTest {
    init {
        // ADR-0058: the words under test are the English catalog's.
        EnglishForTests.install()
    }


    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private var expands = 0
    private val muteToggles = mutableListOf<Boolean>()

    @Composable
    private fun BarUnderTest(model: CallBarModel) {
        MaterialTheme {
            Surface {
                Box(Modifier.testTag(BAR)) {
                    CollapsedCallBar(
                        model = model,
                        onExpand = { expands += 1 },
                        onToggleMute = { on -> muteToggles += on },
                    )
                }
            }
        }
    }

    /** A bar for a call with [status] on its line. */
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

    private fun assertNotOnTheBar(vararg text: String) {
        for (t in text) {
            compose.onNodeWithText(t).assertDoesNotExist()
        }
    }

    // ----- 1. exactly one thing on the status line -------------------------

    /**
     * Section 4 gives the line "the state **or** the running time and never two
     * at once", which is why the rule hands back a sum type rather than three
     * booleans. A sum type in the model does not by itself stop a composable
     * drawing a second line beside it, so both halves are read off the tree: the
     * one that belongs, and every one that does not.
     */
    @Test
    fun aConnectingCallShowsTheStateWordAndNoClock() {
        compose.setContent { BarUnderTest(model(status = CallBarStatus.Connecting)) }

        compose.onNodeWithText(CONNECTING).assertIsDisplayed()
        assertNotOnTheBar(ELAPSED_CLOCK, CALLING, RECONNECTING)
    }

    @Test
    fun aConnectedCallShowsTheRunningTimeAndNoStateWord() {
        compose.setContent { BarUnderTest(model(status = CallBarStatus.Elapsed(ELAPSED_MS))) }

        compose.onNodeWithText(ELAPSED_CLOCK).assertIsDisplayed()
        // `Connected` is in the list because it is what the wording falls back
        // to when it is given no time: a bar showing it would mean the clock the
        // bar exists to carry had been lost, and the bar would look fine.
        assertNotOnTheBar(CONNECTING, CALLING, RECONNECTING, CONNECTED_WORD)
    }

    @Test
    fun theBarSaysWhoIsOnTheCallBesideTheOneLine() {
        // The other half of "who is on the call, then the state or the time". A
        // bar reading `0:42` and nothing else is a clock a person cannot
        // attribute to anybody.
        compose.setContent { BarUnderTest(model(status = CallBarStatus.Elapsed(ELAPSED_MS))) }

        compose.onNodeWithText(WHO).assertIsDisplayed()
    }

    // ----- 2. tapping it brings the call back ------------------------------

    @Test
    fun tappingTheBarBringsTheCallBack() {
        compose.setContent { BarUnderTest(model(status = CallBarStatus.Elapsed(ELAPSED_MS))) }

        compose.onNode(hasContentDescription(RETURN, substring = true)).performClick()

        assertEquals("the bar is not wired to bringing the call back", 1, expands)
        assertEquals("expanding the call also pressed mute", emptyList<Boolean>(), muteToggles)
    }

    @Test
    fun theMuteButtonMutesAndDoesNotBringTheCallBack() {
        // Why the bar is two touch targets and not one, asserted rather than
        // left to the comment that says so: a mute button nested inside the
        // expanding row would throw the call screen up every time a person
        // silenced themselves, which is the opposite of what putting the call
        // away was for.
        compose.setContent {
            BarUnderTest(model(status = CallBarStatus.Elapsed(ELAPSED_MS), micOn = true))
        }

        compose.onNodeWithContentDescription(MUTE).performClick()

        assertEquals("the mute button reported the wrong next state", listOf(false), muteToggles)
        assertEquals("pressing mute brought the call screen back", 0, expands)
    }

    // ----- 3. there is no End on the bar ----------------------------------

    /**
     * Section 4.5 is explicit: ending is the one irreversible control in a call,
     * the call screen sets it apart by twice the gap and paints it the only red
     * on the screen, and "a strip a thumb crosses on the way to a chat is the
     * wrong place for it".
     *
     * Asserted against the words an End control really carries wherever it IS
     * drawn — `callControlRow`'s own `label` and `spokenLabel` — rather than
     * against a guess, and then against the COUNT, so an unlabelled button that
     * crept in beside the one the bar is allowed is caught as well.
     */
    @Test
    fun theBarCarriesNoEndControl() {
        compose.setContent { BarUnderTest(model(status = CallBarStatus.Elapsed(ELAPSED_MS))) }

        compose.onNodeWithText(END_LABEL).assertDoesNotExist()
        compose.onNodeWithContentDescription(END_SPOKEN).assertDoesNotExist()
        assertEquals(
            "the bar has something to press beyond the return and the one mute button",
            2,
            compose.onAllNodes(hasClickAction()).fetchSemanticsNodes().size,
        )
    }

    @Test
    fun theBarCarriesNoEndControlWhileItIsStillCalling() {
        // The `Calling...` row of section 4.1's state table is where End is the
        // ONLY active control on the call screen, so it is the state a stray End
        // is likeliest to be drawn in. The bar still has none, and its mute
        // button is drawn-but-dead there rather than removed.
        compose.setContent { BarUnderTest(model(status = CallBarStatus.Calling)) }

        compose.onNodeWithText(END_LABEL).assertDoesNotExist()
        compose.onNodeWithContentDescription(END_SPOKEN).assertDoesNotExist()
        compose.onNodeWithText(CALLING).assertIsDisplayed()
    }

    private companion object {
        const val BAR = "collapsed-call-bar"
        const val WHO = "Anna"

        /** 42 s, which `formatCallDuration` renders as `0:42`. */
        val ELAPSED_MS: ULong = 42_000uL
        const val ELAPSED_CLOCK = "0:42"

        const val CONNECTING = "Connecting..."
        const val CALLING = "Calling..."
        const val RECONNECTING = "Reconnecting..."

        /** `callStatusText`'s word for a connected call carrying no time. */
        const val CONNECTED_WORD = "Connected"

        /** The tail of the expanding row's spoken description. */
        const val RETURN = "Return to the call."

        /** `callBarMicControl(micOn = true, ...)`'s spoken label. */
        const val MUTE = "Mute microphone"

        /** What an End control carries wherever it IS drawn — `callControlRow`. */
        const val END_LABEL = "End"
        const val END_SPOKEN = "End call"
    }
}
