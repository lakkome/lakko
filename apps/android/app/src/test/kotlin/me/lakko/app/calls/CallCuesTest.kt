// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.CallBarPhase

/**
 * What a call sounds like (`docs/ux/calls.md` section 3.4): whether the
 * outgoing tone plays, and when the two short cues do.
 *
 * Whether the tone plays is the shared rule
 * `lakko-ui-rules::call_group::ringback_plays`, tested in the core. The host
 * JVM cannot load the native library, so the tests of the tone here run a
 * stand-in ([FakeGroupCallRules]) and check the WIRING: that the rule is
 * asked, about the right kind of call and the right phase, and that its
 * answer is the answer.
 */
class CallCuesTest {
    init {
        EnglishForTests.install()
    }

    private val calling = CallUiState.CallPhase.Calling
    private val connecting = CallUiState.CallPhase.Connecting
    private val connected = CallUiState.CallPhase.Connected
    private val ended = CallUiState.CallPhase.Ended(endedAtMs = 1L)

    /** The rule as the core writes it, restated so the wiring has something
     *  to carry: only a one-to-one call, and only until it is answered. */
    private fun ruleLike() = FakeGroupCallRules(
        ringback = { groupCall, phase ->
            !groupCall && (phase == CallBarPhase.PREPARING || phase == CallBarPhase.CALLING)
        },
    )

    // ---- The outgoing tone ---------------------------------------------------

    @Test
    fun `a group call never starts the ringback, in any phase`() {
        val rules = ruleLike()
        val phases = listOf(calling, connecting, connected, CallUiState.CallPhase.Reconnecting("x"), ended)
        for (phase in phases) {
            assertFalse("$phase", ringbackShouldPlay(rules, groupCall = true, phase = phase))
        }
        // The rule was asked every time, and every time about a group call.
        assertEquals(phases.size, rules.ringbackQuestions.size)
        assertTrue(rules.ringbackQuestions.all { (groupCall, _) -> groupCall })
    }

    @Test
    fun `a one-to-one call this device placed rings until it is answered`() {
        val rules = ruleLike()
        assertTrue(ringbackShouldPlay(rules, groupCall = false, phase = calling))
        assertFalse(ringbackShouldPlay(rules, groupCall = false, phase = connecting))
        assertFalse(ringbackShouldPlay(rules, groupCall = false, phase = connected))
        assertFalse(ringbackShouldPlay(rules, groupCall = false, phase = ended))
    }

    @Test
    fun `the device that answered a one-to-one call hears no ringback`() {
        // An answered call is `Connecting` on the answering device from its
        // first frame (the session's `Ringing` maps there), and the rule is
        // asked about that, never about `Calling`.
        val rules = ruleLike()
        val state = CallUiState()
        state.setPhase(connecting)
        assertFalse(ringbackShouldPlay(rules, groupCall = false, phase = state.phase.value))
        assertEquals(listOf(false to CallBarPhase.CONNECTING), rules.ringbackQuestions)
    }

    @Test
    fun `the rule is asked about the phase the call bar reads`() {
        val rules = FakeGroupCallRules()
        ringbackShouldPlay(rules, groupCall = false, phase = calling)
        ringbackShouldPlay(rules, groupCall = false, phase = connected)
        ringbackShouldPlay(rules, groupCall = true, phase = ended)
        assertEquals(
            listOf(false to CallBarPhase.CALLING, false to CallBarPhase.CONNECTED, true to CallBarPhase.ENDED),
            rules.ringbackQuestions,
        )
    }

    @Test
    fun `the rule's answer is the answer`() {
        // Nothing here second-guesses the core: were it to say a group call
        // rings, this client would ring.
        val always = FakeGroupCallRules(ringback = { _, _ -> true })
        assertTrue(ringbackShouldPlay(always, groupCall = true, phase = connected))
    }

    // ---- The status line -----------------------------------------------------

    @Test
    fun `a starting group call reads Connecting, never Calling`() {
        assertEquals("Connecting...", callStatusTextFor(groupCall = true, phase = calling, elapsedSecs = null))
        assertEquals("Connecting...", callStatusTextFor(groupCall = true, phase = connecting, elapsedSecs = null))
        assertEquals("Calling...", callStatusTextFor(groupCall = false, phase = calling, elapsedSecs = null))
        assertEquals("Connecting...", callStatusTextFor(groupCall = false, phase = connecting, elapsedSecs = null))
    }

    @Test
    fun `a starting group call is never Calling on the collapsed bar either`() {
        assertEquals(CallBarPhase.CONNECTING, groupCallBarPhaseOf(calling))
        assertEquals(CallBarPhase.CONNECTING, groupCallBarPhaseOf(connecting))
        assertEquals(CallBarPhase.CONNECTED, groupCallBarPhaseOf(connected))
        assertEquals(CallBarPhase.ENDED, groupCallBarPhaseOf(ended))
        // A one-to-one call keeps the distinction.
        assertEquals(CallBarPhase.CALLING, callBarPhaseOf(calling))
    }

    // ---- The two short cues --------------------------------------------------

    @Test
    fun `connecting is silent, connected plays the join cue, ended the leave cue`() {
        val cues = CallCueTracker()
        assertNull(cues.onPhase(calling))
        assertNull(cues.onPhase(connecting))
        assertEquals(CallCue.JOIN, cues.onPhase(connected))
        assertEquals(CallCue.LEAVE, cues.onPhase(ended))
    }

    @Test
    fun `a cue plays on the change only`() {
        val cues = CallCueTracker()
        assertEquals(CallCue.JOIN, cues.onPhase(connected))
        // The same phase observed again, for whatever reason, plays nothing.
        assertNull(cues.onPhase(connected))
        assertNull(cues.onPhase(connected))
        assertEquals(CallCue.LEAVE, cues.onPhase(ended))
        // A second `Ended` with another timestamp is still the same end.
        assertNull(cues.onPhase(CallUiState.CallPhase.Ended(endedAtMs = 2L)))
    }

    @Test
    fun `a screen that first appears on a connected call plays the join cue once`() {
        val cues = CallCueTracker()
        cues.mount(connected, listOf("a"))
        assertEquals(CallCue.JOIN, cues.onPhase(connected))
        assertNull(cues.onPhase(connected))
    }

    @Test
    fun `a screen brought back to a call plays nothing for what it finds`() {
        val cues = CallCueTracker()
        assertNull(cues.onPhase(connecting))
        // The call connected and two people came while the call was put away.
        cues.mount(connected, listOf("a", "b"))
        assertNull(cues.onPhase(connected))
        assertTrue(cues.onRoster(listOf("a", "b"), connected = true).isEmpty())
        // What happens from here on is heard.
        assertEquals(listOf(CallCue.LEAVE), cues.onRoster(listOf("a"), connected = true))
    }

    @Test
    fun `someone arriving and someone leaving each play their cue`() {
        val cues = CallCueTracker()
        cues.onPhase(connected)
        // The people already there when this device connected: the connected
        // cue covered them.
        assertTrue(cues.onRoster(listOf("a"), connected = true).isEmpty())
        assertEquals(listOf(CallCue.JOIN), cues.onRoster(listOf("a", "b"), connected = true))
        assertEquals(listOf(CallCue.LEAVE), cues.onRoster(listOf("b"), connected = true))
        // One leaves and another comes in the same reading: both are heard.
        assertEquals(listOf(CallCue.JOIN, CallCue.LEAVE), cues.onRoster(listOf("c"), connected = true))
        // The same roster again plays nothing.
        assertTrue(cues.onRoster(listOf("c"), connected = true).isEmpty())
    }

    @Test
    fun `a roster change before the call is connected is silent`() {
        val cues = CallCueTracker()
        assertTrue(cues.onRoster(listOf("a"), connected = false).isEmpty())
        assertTrue(cues.onRoster(listOf("a", "b"), connected = false).isEmpty())
    }

    @Test
    fun `a new call starts over`() {
        val state = CallUiState()
        assertEquals(CallCue.JOIN, state.cues.onPhase(connected))
        state.reset()
        assertEquals(CallCue.JOIN, state.cues.onPhase(connected))
    }
}
