// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/media-messages.md` — the composer's recorder stands aside while a
 * call HOLDS the microphone and camera, and a ringing offer holds neither.
 *
 * ## What these tests prove, and what they deliberately do not
 *
 * The rule is `crates/lakko-ui-rules/src/capture.rs`, and what it decides — a
 * ringing offer frees the recorder, an answered call does not — is asserted
 * there. Restating it here would be a second copy of the paragraph the shared
 * crate exists to have one of, and a copy that drifted would keep passing.
 *
 * These tests cover the WIRING instead, which is where the defect lived: the two
 * facts this client derives and hands over, and that the rule's answer comes
 * back untouched. [STAND_IN_RULE] stands in for the core the way
 * [TEST_RING_WINDOW_MS] stands in for the ring window, and for the same reason:
 * this suite does not load `liblakko_ffi_uniffi.so`, so calling the real rule
 * from here would only prove the library is absent. It drifting from the core
 * would not make any of these tests lie — it would only mean they exercised a
 * different rule, while the production call site in `ui/chat/ChatScreen.kt`
 * passes `::callHoldsCapture`.
 */
class CallCaptureTest {

    /**
     * The shared rule, for the host JVM suite only. NOT its home — see the
     * class docs.
     */
    private companion object {
        val STAND_IN_RULE: (Boolean, Boolean) -> Boolean = { callIsLive, ring -> callIsLive && !ring }
    }

    /** Records what the rule was asked, and answers whatever it is told to. */
    private class SpyRule(private val answer: Boolean) : (Boolean, Boolean) -> Boolean {
        val asked = mutableListOf<Pair<Boolean, Boolean>>()
        override fun invoke(callIsLive: Boolean, ring: Boolean): Boolean {
            asked += callIsLive to ring
            return answer
        }
    }

    private fun call(
        incoming: Boolean = true,
        accepted: Boolean = false,
        muted: Boolean = false,
    ) = OneToOneCall(
        groupIdHex = "aa11",
        session = OneToOneCallHostTest.FakeSession(ByteArray(16) { 3 }),
        incoming = incoming,
        remoteOffersVideo = false,
        accepted = accepted,
        muted = muted,
    )

    // ---- what the rule is asked ---------------------------------------------

    @Test
    fun `a ringing incoming call is reported as a call that holds nothing`() {
        val rule = SpyRule(false)
        callOwnsCapture(
            occupant = CallKind.ONE_TO_ONE,
            call = call(),
            state = CallSession.WebCallState.Ringing,
            holdsCapture = rule,
        )
        assertEquals(listOf(true to true), rule.asked)
    }

    @Test
    fun `a muted conversation's quiet offer is reported the same way`() {
        // The state the defect was found in. Muting withholds the interruption,
        // not the microphone: the quiet offer holds exactly as little as the
        // loud ring does.
        val rule = SpyRule(false)
        callOwnsCapture(
            occupant = CallKind.ONE_TO_ONE,
            call = call(muted = true),
            state = CallSession.WebCallState.Ringing,
            holdsCapture = rule,
        )
        assertEquals(listOf(true to true), rule.asked)
    }

    @Test
    fun `an answered call is reported as a call that holds capture`() {
        val rule = SpyRule(true)
        callOwnsCapture(
            occupant = CallKind.ONE_TO_ONE,
            call = call(accepted = true),
            state = CallSession.WebCallState.Ringing,
            holdsCapture = rule,
        )
        assertEquals(listOf(true to false), rule.asked)
    }

    @Test
    fun `a live call in ANOTHER chat still blocks this chat's recorder`() {
        // `docs/ux/calls.md` section 4.5 — a call collapsed into the bar keeps
        // running while the person walks into a different conversation, and the
        // microphone is just as busy there. The screen's own 1x1 read answers
        // for THIS conversation only, so it is null here; the fact handed over
        // is the app-wide slot, which is why the first argument is
        // `callIsLive` and not "a call in this chat".
        for (occupant in CallKind.entries) {
            val rule = SpyRule(true)
            val blocked = callOwnsCapture(
                occupant = occupant,
                call = null,
                state = CallSession.WebCallState.Done,
                holdsCapture = rule,
            )
            assertEquals("$occupant elsewhere is still a live call", listOf(true to false), rule.asked)
            assertTrue("$occupant elsewhere holds the microphone", blocked)
            assertTrue(
                "$occupant elsewhere blocks under the shared rule too",
                callOwnsCapture(occupant, null, CallSession.WebCallState.Done, STAND_IN_RULE),
            )
        }
    }

    @Test
    fun `an empty slot is handed to the rule rather than short-circuited here`() {
        val rule = SpyRule(false)
        callOwnsCapture(
            occupant = null,
            call = null,
            state = CallSession.WebCallState.Done,
            holdsCapture = rule,
        )
        assertEquals(listOf(false to false), rule.asked)
    }

    @Test
    fun `the answer is the rule's and never this client's own`() {
        // A ringing call IS a call in the slot, so anything re-deciding from the
        // occupant alone would answer `true` for the first of these - which is
        // exactly the defect.
        assertFalse(
            callOwnsCapture(
                CallKind.ONE_TO_ONE,
                call(),
                CallSession.WebCallState.Ringing,
                SpyRule(false),
            ),
        )
        assertTrue(
            callOwnsCapture(
                CallKind.ONE_TO_ONE,
                call(accepted = true),
                CallSession.WebCallState.Connected,
                SpyRule(true),
            ),
        )
    }

    // ---- the outcome, on the stand-in rule -----------------------------------

    @Test
    fun `a ringing incoming call leaves the recorder usable and a live one does not`() {
        assertFalse(
            "a ringing offer opens no microphone, so the recorder stays usable",
            callOwnsCapture(
                CallKind.ONE_TO_ONE,
                call(),
                CallSession.WebCallState.Ringing,
                STAND_IN_RULE,
            ),
        )
        assertTrue(
            "a live call owns the microphone",
            callOwnsCapture(
                CallKind.ONE_TO_ONE,
                call(accepted = true),
                CallSession.WebCallState.Connected,
                STAND_IN_RULE,
            ),
        )
    }

    // ---- the ring fact itself ------------------------------------------------

    @Test
    fun `an outgoing call is never an unanswered incoming ring`() {
        // The asymmetry the rule's flag is named for: the caller's microphone
        // opens as the call is placed, so a dialing call holds capture from its
        // first moment.
        assertFalse(
            unansweredIncomingRing(
                occupant = CallKind.ONE_TO_ONE,
                incoming = false,
                accepted = false,
                muted = false,
                state = CallSession.WebCallState.Ringing,
            ),
        )
        assertTrue(
            callOwnsCapture(
                CallKind.ONE_TO_ONE,
                call(incoming = false),
                CallSession.WebCallState.Outgoing,
                STAND_IN_RULE,
            ),
        )
    }

    @Test
    fun `a group or link call in the slot is never freed by a stale 1x1 read`() {
        for (occupant in listOf(CallKind.GROUP, CallKind.LINK)) {
            assertFalse(
                "$occupant owns the microphone outright",
                unansweredIncomingRing(
                    occupant = occupant,
                    incoming = true,
                    accepted = false,
                    muted = false,
                    state = CallSession.WebCallState.Ringing,
                ),
            )
        }
    }

    @Test
    fun `the ring fact agrees with the surfaces that draw the ring`() {
        // One reading of "has this been answered", so the recorder cannot think
        // a call is still being offered after the ring surfaces have stopped
        // drawing it.
        for (state in CallSession.WebCallState.entries) {
            for (accepted in listOf(false, true)) {
                for (muted in listOf(false, true)) {
                    val drawsARing =
                        oneToOneCallPresentation(state, accepted, muted) !=
                            OneToOneCallPresentation.IN_CALL_SCREEN
                    assertEquals(
                        "state=$state accepted=$accepted muted=$muted",
                        drawsARing,
                        unansweredIncomingRing(
                            occupant = CallKind.ONE_TO_ONE,
                            incoming = true,
                            accepted = accepted,
                            muted = muted,
                            state = state,
                        ),
                    )
                }
            }
        }
    }
}
