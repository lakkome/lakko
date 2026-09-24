// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A muted conversation's call is OFFERED, not thrown and not hidden
 * (`docs/ux/calls.md` section 2.1.1, decided 2026-09-23).
 *
 * ## The rule this replaced
 *
 * The first reading of "a muted chat never rings" was "show nothing at all",
 * and this client implemented it: the presentation verdict was `NONE` and every
 * surface drew nothing, so a call from a muted contact could not be answered,
 * only called back. Muting a conversation had quietly become half-blocking the
 * person in it. The refusal to interrupt is unchanged — no ringtone, no
 * vibration, no screen that takes over, all still asserted in
 * `IncomingCallSurfaceTest` and in `crates/lakko-ui-rules/src/ring.rs` — and
 * what is new is the obligation that comes with it: the call must stay
 * answerable for the whole window on two surfaces that interrupt nothing.
 *
 * ## What is asserted here
 *
 * [quietCallOffer] is the whole condition of both of those surfaces: the panel
 * at the top of the conversation and that chat's row in the conversation list.
 * So what these tests cover is exactly when an offer exists, which conversation
 * it belongs to, and that it agrees with the overlay's verdict — the last
 * because the two surfaces are mutually exclusive by construction only if they
 * are reading one rule.
 *
 * What is NOT asserted here: what a muted offer means (that is the shared rule
 * in Rust), and how long it lives. Nothing in this file counts down, because
 * nothing in the offer does: the host's RING timer ends the call and clears the
 * slot both surfaces read from, which is the same reading that files the missed
 * call.
 */
class QuietCallOfferTest {

    private val groupA = "aa".repeat(32)
    private val groupB = "bb".repeat(32)

    private fun offer(
        state: CallSession.WebCallState = CallSession.WebCallState.Ringing,
        incoming: Boolean = true,
        accepted: Boolean = false,
        muted: Boolean = true,
        video: Boolean = false,
        groupIdHex: String = groupA,
    ): QuietCallOffer? = quietCallOffer(
        groupIdHex = groupIdHex,
        state = state,
        incoming = incoming,
        accepted = accepted,
        muted = muted,
        video = video,
    )

    // ---- when there is something to offer -----------------------------------

    @Test
    fun `a muted call that is still ringing is offered, so it can still be answered`() {
        assertEquals(QuietCallOffer(groupIdHex = groupA, video = false), offer())
    }

    @Test
    fun `the offer says which kind of call it is, so the surfaces can say so too`() {
        // The panel and the row both print `Voice call` or `Video call`, and the
        // Accept they offer has to answer the call that was actually made: a
        // video offer accepted as audio-only is a different answer, and it is
        // the one the third button and the permission gate exist for.
        assertTrue(offer(video = true)?.video == true)
        assertFalse(offer(video = false)?.video == true)
    }

    // ---- when there is not -------------------------------------------------

    @Test
    fun `an unmuted ringing call is not offered quietly - it rings`() {
        // Not a nicety: an offer here would put a panel in the chat UNDER the
        // full-screen ring screen, which is the same call twice.
        assertNull(offer(muted = false))
        assertEquals(
            OneToOneCallPresentation.RINGING_SCREEN,
            oneToOneCallPresentation(CallSession.WebCallState.Ringing, accepted = false, muted = false),
        )
    }

    @Test
    fun `an answered muted call is not offered - it is a call now`() {
        // Accepting moves the call to the in-call screen; leaving the panel up
        // would offer Accept for a call the person is already in.
        assertNull(offer(accepted = true))
    }

    @Test
    fun `a muted call that is no longer ringing is not offered in any state`() {
        for (state in CallSession.WebCallState.entries) {
            if (state == CallSession.WebCallState.Ringing) continue
            assertNull("state $state must not be offered quietly", offer(state = state))
        }
    }

    @Test
    fun `an outgoing call is never offered, muted or not`() {
        // There is nobody to accept, and `muted` is meaningless for a call this
        // device placed - nobody is being interrupted by it.
        assertNull(offer(incoming = false))
        assertNull(offer(incoming = false, muted = false))
    }

    // ---- which conversation it belongs to (the list-row condition) ----------

    @Test
    fun `the offer belongs to its own conversation and to no other`() {
        // This IS the list-row affordance's condition: the row of the ringing
        // chat carries Accept and Decline, and every other row carries nothing.
        // A row that got this wrong would answer somebody else's call.
        val quiet = requireNotNull(offer(groupIdHex = groupA))
        assertTrue(quiet.isFor(groupA))
        assertFalse(quiet.isFor(groupB))
        assertFalse(quiet.isFor(""))
    }

    @Test
    fun `the conversation is matched by identity, not by spelling`() {
        // Both sides are hex spellings of one group id, produced by different
        // code paths (the call host's routing and the list's own row ids). A
        // conversation with a live call is not the place to discover that one
        // of them capitalises.
        val quiet = offer(groupIdHex = groupA.uppercase())!!
        assertTrue(quiet.isFor(groupA))
        assertTrue(quiet.isFor(groupA.uppercase()))
    }

    // ---- one rule behind both surfaces --------------------------------------

    @Test
    fun `an offer exists exactly when the overlay draws nothing for an incoming call`() {
        // The two surfaces are mutually exclusive only because they read one
        // verdict. Asserted over every combination rather than at the one point
        // the current code happens to reach, because a future condition added to
        // either side would be caught here and nowhere else.
        for (state in CallSession.WebCallState.entries) {
            for (accepted in listOf(false, true)) {
                for (muted in listOf(false, true)) {
                    val presentation = oneToOneCallPresentation(state, accepted, muted)
                    val quiet = offer(state = state, accepted = accepted, muted = muted)
                    assertEquals(
                        "state=$state accepted=$accepted muted=$muted",
                        presentation == OneToOneCallPresentation.QUIET_OFFER,
                        quiet != null,
                    )
                }
            }
        }
    }

    // ---- the call-to-values mapping -----------------------------------------

    @Test
    fun `the call's own fields reach the offer unswapped`() {
        // The extension is the one place a call becomes six loose values, and
        // the two that matter are both booleans: handing `remoteOffersVideo`
        // where `muted` belongs would offer every unmuted video call quietly and
        // never offer a muted voice one, without failing to compile.
        val session = OneToOneCallHostTest.FakeSession(ByteArray(16) { 7 })
        val call = OneToOneCall(
            groupIdHex = groupA,
            session = session,
            incoming = true,
            remoteOffersVideo = true,
            accepted = false,
            muted = true,
        )
        assertEquals(
            QuietCallOffer(groupIdHex = groupA, video = true),
            call.quietOffer(CallSession.WebCallState.Ringing),
        )
        // And the same call once it is answered offers nothing.
        assertNull(call.copy(accepted = true).quietOffer(CallSession.WebCallState.Ringing))
        // ... nor while it is merely muted and connected.
        assertNull(call.quietOffer(CallSession.WebCallState.Connected))
    }

    @Test
    fun `an unmuted call carries no quiet offer through the extension either`() {
        val session = OneToOneCallHostTest.FakeSession(ByteArray(16) { 7 })
        val call = OneToOneCall(
            groupIdHex = groupA,
            session = session,
            incoming = true,
            remoteOffersVideo = false,
            muted = false,
        )
        assertNull(call.quietOffer(CallSession.WebCallState.Ringing))
    }
}
