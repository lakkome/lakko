// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.en
import java.io.IOException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The invite link of a live call — `docs/ux/call-links.md` sections 3, 4 and 5.
 *
 * The values the client fills in place of the four questions the deleted form
 * asked, who is offered a link at all, and what a failure says.
 */
class CallInviteLinkTest {

    // ---- Section 4: the values the link carries -------------------------------
    //
    // The expiry window and the use count are NOT tested here, and their
    // absence is the point. They live in `lakko_contacts::call_invite` and
    // arrive over the FFI, because they are fields of a signed capability and
    // the wire is what they have to agree with. Their bounds are `const`
    // assertions in Rust, so a value outside them fails the build rather than
    // a test - which is stronger than anything this file could assert, and is
    // what stops the drift that put twenty-four hours here while the web and
    // iOS carried twelve.
    //
    // Calling those wrappers from a JVM unit test would only prove the native
    // library is absent, which it is.

    @Test
    fun `the link carries video and has no label`() {
        // The two values this client really does decide: they are what this
        // product sends, not what the format constrains.
        assertTrue(CALL_INVITE_LINK_MEDIA_VIDEO)
        assertEquals("", CALL_INVITE_LINK_LABEL)
    }

    @Test
    fun `a fresh call id is sixteen bytes and not the same twice`() {
        val a = newCallId()
        val b = newCallId()
        assertEquals(CALL_ID_LEN, a.size)
        assertEquals(CALL_ID_LEN, b.size)
        assertFalse(a.contentEquals(b))
    }

    // ---- Section 3: who is offered the control --------------------------------

    @Test
    fun `only the host of a live call is offered the invite control`() {
        assertTrue(showsCallInvite(owner = true, phase = LinkCallPhase.LIVE))
        // A joiner's capability would name the joiner as the call's owner, and
        // send whoever followed it to elect a committer that device is not.
        assertFalse(showsCallInvite(owner = false, phase = LinkCallPhase.LIVE))
        // Nothing to invite anyone to until the call is up, and nothing after.
        for (phase in LinkCallPhase.entries.filter { it != LinkCallPhase.LIVE }) {
            assertFalse("$phase", showsCallInvite(owner = true, phase = phase))
        }
    }

    // ---- Section 5: what a failure says ---------------------------------------
    //
    // What `Invite` is doing in each state, and the line the call screen shows
    // for it, is `InCallInviteTest`.

    @Test
    fun `a failure is one sentence on what happened and one on what to do`() {
        for (failure in CallInviteFailure.entries) {
            val message = callInviteFailureMessage(failure).en
            assertTrue("$failure", message.endsWith("."))
            assertFalse("$failure", message.contains("!"))
            // Two sentences: what happened, and what to do about it.
            assertEquals("$failure", 2, message.count { it == '.' })
        }
    }

    @Test
    fun `a lost connection is told apart from a lost session and from anything else`() {
        // A missing session is found by reading it, never thrown, so it is
        // named directly and a thrown cause can never be mistaken for it.
        assertEquals(CallInviteFailure.NETWORK, classifyCallInviteFailure(IOException("no route")))
        assertEquals(CallInviteFailure.OTHER, classifyCallInviteFailure(IllegalStateException("?")))
        assertEquals(CallInviteFailure.OTHER, classifyCallInviteFailure(null))
        assertNotEquals(
            callInviteFailureMessage(CallInviteFailure.NO_SESSION),
            callInviteFailureMessage(CallInviteFailure.NETWORK),
        )
    }

    // ---- Section 2: New call --------------------------------------------------

    @Test
    fun `a started call says nothing and a refused one says one sentence`() {
        assertNull(newCallStartMessage(LinkCallStartOutcome.Started))
        assertNull(newCallStartMessage(LinkCallStartOutcome.AlreadyInThisCall))
        // A busy slot already carries a finished sentence.
        assertEquals(
            callBusyMessage(CallKind.GROUP, CallVerb.START),
            newCallStartMessage(LinkCallStartOutcome.Busy(callBusyMessage(CallKind.GROUP, CallVerb.START))),
        )
        // A failure carries the host's own diagnostic, which names endpoints
        // and configuration; that belongs in the log, not on the screen.
        val failed = newCallStartMessage(
            LinkCallStartOutcome.Failed("This client has no SFU endpoint — set lakko.sfu.url"),
        )
        assertEquals("Could not start the call. Try again.", failed?.en)
    }

    @Test
    fun `being signed out is said in one sentence, not a form`() {
        assertTrue(NEW_CALL_SIGNED_OUT_MESSAGE.en.endsWith("."))
        assertFalse(NEW_CALL_SIGNED_OUT_MESSAGE.en.contains("!"))
    }
}
