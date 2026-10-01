// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.en
import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.CallBarPhase
import uniffi.lakko_ffi_uniffi.CallBarStatus

/**
 * The collapsed call bar (`docs/ux/calls.md` section 4.5), host side.
 *
 * ## What this can and cannot cover
 *
 * It covers everything about the bar that is not a composable: the two phase
 * translations, the wording, and the mute button's state. It cannot cover how
 * the bar LOOKS or that back collapses it — there is no Robolectric and no
 * compose-ui-test on this client, so no test here can press anything. That is
 * exactly why the translations were pulled out of the composable into pure
 * functions: what a test can reach is the part most likely to be wrong quietly.
 *
 * The two decisions with no platform in them —
 * [uniffi.lakko_ffi_uniffi.callBarVisible] and
 * [uniffi.lakko_ffi_uniffi.callBarStatus] — are NOT tested here. They are FFI
 * calls needing `liblakko_ffi_uniffi.so`, which the host JVM does not load, and
 * they already have 6 property tests in `crates/lakko-ui-rules/src/call_bar.rs`.
 * Their Kotlin TYPES are used freely below, which is fine: an enum and a sealed
 * class from the bindings are ordinary classes and touch no native library.
 */
class CollapsedCallBarTest {
    init {
        // ADR-0061: the words under test are the English catalog's.
        EnglishForTests.install()
    }


    // ---- The 1x1 session's state, as the bar's rule sees it -------------------

    /**
     * The bar and the call screen must never disagree about what a call is
     * doing, so this pins the mapping against [CallUiState]'s own — the one the
     * screen reads — on every state where both are shown.
     *
     * Not a restatement of the mapping: it asserts the AGREEMENT, which is the
     * property that would actually break if either side were edited alone.
     */
    @Test
    fun `the bar and the call screen agree on what every state means`() {
        val expectedWords = mapOf(
            CallSession.WebCallState.Idle to "Calling...",
            CallSession.WebCallState.Outgoing to "Calling...",
            CallSession.WebCallState.Connecting to "Connecting...",
        )
        for ((state, words) in expectedWords) {
            val barPhase = callBarPhaseOf(state, accepted = false)
            assertEquals(
                "$state reads differently on the bar than on the call screen",
                words,
                callBarStatusText(statusFor(barPhase)),
            )
        }
    }

    /**
     * Section 4.5: "A call that is only ringing does not collapse." An incoming
     * call is answered or declined, so there is nothing to put away — and only
     * [CallBarPhase.RINGING] tells the rule that, which is why this mapping
     * splits where the call screen's does not.
     */
    @Test
    fun `an unanswered incoming call is RINGING and an answered one is not`() {
        assertEquals(
            CallBarPhase.RINGING,
            callBarPhaseOf(CallSession.WebCallState.Ringing, accepted = false),
        )
        assertEquals(
            CallBarPhase.CONNECTING,
            callBarPhaseOf(CallSession.WebCallState.Ringing, accepted = true),
        )
    }

    /**
     * Section 4.5: "When a collapsed call ends, the bar goes away." The hang-up
     * IS the end for the bar, where the call screen keeps reading `Connected`
     * through `Ending` so its own timer does not stop mid-hang-up.
     */
    @Test
    fun `a call being hung up and a finished call are both over as far as the bar is concerned`() {
        for (state in listOf(CallSession.WebCallState.Ending, CallSession.WebCallState.Done)) {
            for (accepted in listOf(false, true)) {
                assertEquals(
                    "$state should leave no bar",
                    CallBarPhase.ENDED,
                    callBarPhaseOf(state, accepted = accepted),
                )
            }
        }
    }

    /** A live call is a live call whichever way it was placed. */
    @Test
    fun `connected is connected`() {
        assertEquals(
            CallBarPhase.CONNECTED,
            callBarPhaseOf(CallSession.WebCallState.Connected, accepted = true),
        )
        assertEquals(
            CallBarPhase.CONNECTED,
            callBarPhaseOf(CallSession.WebCallState.Connected, accepted = false),
        )
    }

    /**
     * No state may fall through to a phase that means "no call": a 1x1 call that
     * is genuinely running and reports [CallBarPhase.IDLE] would be a live call
     * with no bar and no way back to it.
     */
    @Test
    fun `no session state is ever mistaken for no call at all`() {
        for (state in CallSession.WebCallState.entries) {
            for (accepted in listOf(false, true)) {
                assertNotEquals(
                    "$state (accepted=$accepted) claimed there is no call",
                    CallBarPhase.IDLE,
                    callBarPhaseOf(state, accepted = accepted),
                )
            }
        }
    }

    // ---- The hosted calls' phase ---------------------------------------------

    /**
     * The group (SFU) and call-link calls. Every arm, because a phase that fell
     * through to a default would be a bar frozen on one word for the whole call.
     */
    @Test
    fun `every hosted call phase maps to its own`() {
        assertEquals(CallBarPhase.CALLING, callBarPhaseOf(CallUiState.CallPhase.Calling))
        assertEquals(CallBarPhase.CONNECTING, callBarPhaseOf(CallUiState.CallPhase.Connecting))
        assertEquals(CallBarPhase.CONNECTED, callBarPhaseOf(CallUiState.CallPhase.Connected))
        assertEquals(
            CallBarPhase.RECONNECTING,
            callBarPhaseOf(CallUiState.CallPhase.Reconnecting("lost")),
        )
        assertEquals(CallBarPhase.ENDED, callBarPhaseOf(CallUiState.CallPhase.Ended(0L)))
    }

    // ---- The one line --------------------------------------------------------

    /**
     * Section 4.5 asks for "the words of the status line", so the bar's three
     * state words must be the call screen's own and not a second copy of them.
     */
    @Test
    fun `the bar uses the call screen's own words`() {
        assertEquals(
            callStatusText(true, false, false, false, false, null),
            callBarStatusText(CallBarStatus.Calling),
        )
        assertEquals(
            callStatusText(false, true, false, false, false, null),
            callBarStatusText(CallBarStatus.Connecting),
        )
        assertEquals(
            callStatusText(false, false, false, true, false, null),
            callBarStatusText(CallBarStatus.Reconnecting),
        )
    }

    /** Section 4.5's own examples, verbatim: `Calling...`, `Connecting...`,
     *  `Reconnecting...`, `12:04`. */
    @Test
    fun `the line reads exactly what the spec writes`() {
        assertEquals("Calling...", callBarStatusText(CallBarStatus.Calling))
        assertEquals("Connecting...", callBarStatusText(CallBarStatus.Connecting))
        assertEquals("Reconnecting...", callBarStatusText(CallBarStatus.Reconnecting))
        assertEquals(
            "12:04",
            callBarStatusText(CallBarStatus.Elapsed(((12 * 60 + 4) * 1_000).toULong())),
        )
    }

    /**
     * The rule carries milliseconds because "how long it has been is not a
     * presentation decision"; turning them into `m:ss` is. A part-second must
     * not round the clock forward — a bar reading `0:01` before one second has
     * passed is a clock the person can watch being wrong.
     */
    @Test
    fun `milliseconds are floored to the second, never rounded up`() {
        assertEquals("0:00", callBarStatusText(CallBarStatus.Elapsed(0uL)))
        assertEquals("0:00", callBarStatusText(CallBarStatus.Elapsed(999uL)))
        assertEquals("0:01", callBarStatusText(CallBarStatus.Elapsed(1_000uL)))
        assertEquals("0:01", callBarStatusText(CallBarStatus.Elapsed(1_999uL)))
        assertEquals("1:00:00", callBarStatusText(CallBarStatus.Elapsed(3_600_000uL)))
    }

    /** A running time far past anything real must still be a time and not a
     *  crash: the rule's field is a full `u64`. */
    @Test
    fun `an absurd running time still formats`() {
        val text = callBarStatusText(CallBarStatus.Elapsed(ULong.MAX_VALUE))
        assertTrue("an absurd duration produced $text", text.contains(":"))
    }

    /**
     * The invariant the sum type exists to hold, checked on this side of the
     * boundary too: section 4 says the line carries the state **or** the running
     * time and never two at once. A line that said `Connecting... 0:04` is the
     * exact failure three booleans in a composable would have produced.
     */
    @Test
    fun `the line never says a state word and a time together`() {
        val statuses = listOf(
            CallBarStatus.Calling,
            CallBarStatus.Connecting,
            CallBarStatus.Reconnecting,
            CallBarStatus.Elapsed(64_000uL),
        )
        for (status in statuses) {
            val text = callBarStatusText(status)
            val hasWord = text.endsWith("...")
            val hasTime = text.contains(":")
            assertNotEquals("$status produced both or neither: $text", hasWord, hasTime)
        }
    }

    /** The ended state has no line on the bar, because the bar is gone by then
     *  (section 4.5). Nothing may reach it claiming a call has ended. */
    @Test
    fun `no bar line ever announces that the call ended`() {
        val statuses = listOf(
            CallBarStatus.Calling,
            CallBarStatus.Connecting,
            CallBarStatus.Reconnecting,
            CallBarStatus.Elapsed(0uL),
            CallBarStatus.Elapsed(90_000uL),
        )
        for (status in statuses) {
            assertFalse(
                "$status leaked the ended wording onto the bar",
                callBarStatusText(status).contains("ended"),
            )
        }
    }

    // ---- The one control ----------------------------------------------------

    /**
     * Section 4.1.1's rule, which the bar must not invert: **off is the loud
     * state**. A muted microphone is the SOLID circle; a live one is the quiet
     * outline. The defect this rule was written against was a filled button
     * meaning a live microphone, so a muted one looked live.
     */
    @Test
    fun `a muted microphone is the loud button and a live one is the quiet one`() {
        assertEquals(
            CallControlEmphasis.OUTLINE,
            callBarMicControl(micOn = true, ringing = false).emphasis,
        )
        assertEquals(
            CallControlEmphasis.SOLID,
            callBarMicControl(micOn = false, ringing = false).emphasis,
        )
    }

    /** It IS the call screen's control, not a lookalike: same arm, same object. */
    @Test
    fun `the bar's mute button is the call screen's mute button`() {
        for (micOn in listOf(false, true)) {
            for (ringing in listOf(false, true)) {
                val fromScreen = callControlRow(
                    group = false,
                    micOn = micOn,
                    cameraOn = false,
                    speakerOn = false,
                    ringing = ringing,
                ).first { it.kind == CallControlKind.MICROPHONE }
                assertEquals(
                    "micOn=$micOn ringing=$ringing",
                    fromScreen,
                    callBarMicControl(micOn = micOn, ringing = ringing),
                )
            }
        }
    }

    /**
     * Section 4.1's state table: on the `Calling...` row "only End is active",
     * and the bar has no End. So the bar's one control is dead on that row and
     * live everywhere else — drawn either way, never hidden.
     */
    @Test
    fun `mute is dead while the call is still ringing out and live once it is not`() {
        assertFalse(callBarMicControl(micOn = true, ringing = true).enabled)
        assertTrue(callBarMicControl(micOn = true, ringing = false).enabled)
    }

    /** Section 8 — the spoken label names the action and the state it produces,
     *  from the same place the call screen's does. */
    @Test
    fun `the mute button says what pressing it will do`() {
        assertEquals(
            "Mute microphone",
            callBarMicControl(micOn = true, ringing = false).spokenLabel.en,
        )
        assertEquals(
            "Unmute microphone",
            callBarMicControl(micOn = false, ringing = false).spokenLabel.en,
        )
    }

    /**
     * The bar's status for a phase, without the FFI: mirrors what
     * `call_bar_status` answers so the wording assertions above can run on the
     * host JVM. Only the three phases whose words are asserted are covered.
     */
    private fun statusFor(phase: CallBarPhase): CallBarStatus = when (phase) {
        CallBarPhase.CONNECTING -> CallBarStatus.Connecting
        CallBarPhase.RECONNECTING -> CallBarStatus.Reconnecting
        CallBarPhase.CONNECTED -> CallBarStatus.Elapsed(0uL)
        else -> CallBarStatus.Calling
    }
}
