// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The decisions behind ringing, none of which need Android or the FFI.
 *
 * Three of them are worth holding still, because each has a way of being wrong
 * that a user experiences directly:
 *
 *  - **offer / end -> ring / cancel.** A batch that carries both halves of a
 *    finished call must fall silent, not ring; an end with no offer must still
 *    cancel, because that is how a ring raised by an earlier wake stops.
 *  - **what the notification says.** The kind of call has to match what the
 *    caller chose (`docs/ux/calls.md` section 1), and a caller this device
 *    cannot name must not leak an id onto a locked screen.
 *  - **the accept / decline mapping.** A notification action fires from outside
 *    the app, so its extras are the only thing tying it to a call; a command
 *    that lost them would answer or refuse whichever call happened to be
 *    ringing.
 */
class IncomingCallRingTest {

    private val group = "aa".repeat(32)
    private val callA = "11".repeat(16)
    private val callB = "22".repeat(16)
    private val now = 1_800_000_000_000L

    // ---- offer / end -> ring / cancel ---------------------------------------

    @Test
    fun `an offer rings`() {
        val outcome = foldCallRingSignals(
            group,
            listOf(CallRingSignal.Offer(callA, video = false, offeredAtMs = now - 1_000, callerName = "Anna")),
            now,
        )
        assertEquals(
            IncomingCallRing(
                groupIdHex = group,
                callIdHex = callA,
                callerName = "Anna",
                video = false,
                offeredAtMs = now - 1_000,
            ),
            outcome.ring,
        )
        assertTrue(outcome.cancelled.isEmpty())
    }

    @Test
    fun `an end in the same batch cancels the offer instead of ringing`() {
        val outcome = foldCallRingSignals(
            group,
            listOf(
                CallRingSignal.Offer(callA, video = true, offeredAtMs = now - 500),
                CallRingSignal.End(callA),
            ),
            now,
        )
        assertNull("a call that is already over must not ring", outcome.ring)
        assertEquals(listOf(callA), outcome.cancelled)
    }

    @Test
    fun `an end with no offer still cancels — that is how an earlier ring stops`() {
        val outcome = foldCallRingSignals(group, listOf(CallRingSignal.End(callA)), now)
        assertNull(outcome.ring)
        assertEquals(listOf(callA), outcome.cancelled)
    }

    @Test
    fun `an end does not silence a different call`() {
        val outcome = foldCallRingSignals(
            group,
            listOf(
                CallRingSignal.End(callB),
                CallRingSignal.Offer(callA, video = false, offeredAtMs = now),
            ),
            now,
        )
        assertEquals(callA, outcome.ring?.callIdHex)
        assertEquals(listOf(callB), outcome.cancelled)
    }

    @Test
    fun `an offer re-delivered after its end does not resurrect the ring`() {
        // A catch-up fetch can hand the rows over in an order the journal did
        // not: the end is authoritative for the whole batch.
        val outcome = foldCallRingSignals(
            group,
            listOf(
                CallRingSignal.End(callA),
                CallRingSignal.Offer(callA, video = false, offeredAtMs = now),
            ),
            now,
        )
        assertNull(outcome.ring)
    }

    @Test
    fun `an offer past its ring window never rings`() {
        val outcome = foldCallRingSignals(
            group,
            listOf(CallRingSignal.Offer(callA, video = false, offeredAtMs = now - CALL_RING_WINDOW_MS - 1)),
            now,
        )
        assertNull("the caller gave up long ago", outcome.ring)
    }

    @Test
    fun `the newest surviving offer wins`() {
        val outcome = foldCallRingSignals(
            group,
            listOf(
                CallRingSignal.Offer(callA, video = false, offeredAtMs = now - 2_000),
                CallRingSignal.Offer(callB, video = true, offeredAtMs = now - 100),
            ),
            now,
        )
        assertEquals(callB, outcome.ring?.callIdHex)
        assertEquals(true, outcome.ring?.video)
    }

    @Test
    fun `an empty batch rings for nothing`() {
        val outcome = foldCallRingSignals(group, emptyList(), now)
        assertNull(outcome.ring)
        assertTrue(outcome.cancelled.isEmpty())
    }

    // ---- the ring window -----------------------------------------------------

    @Test
    fun `the window is measured from the offer, not from the wake`() {
        assertEquals(CALL_RING_WINDOW_MS - 20_000, callRingRemainingMs(now - 20_000, now))
        assertEquals(0L, callRingRemainingMs(now - CALL_RING_WINDOW_MS, now))
    }

    @Test
    fun `an unreadable or future stamp rings for the whole window`() {
        // Nothing to measure against: dropping the call would be worse than
        // ringing for the full window.
        assertEquals(CALL_RING_WINDOW_MS, callRingRemainingMs(null, now))
        // The caller's clock runs ahead; it is not trusted to extend the ring.
        assertEquals(CALL_RING_WINDOW_MS, callRingRemainingMs(now + 60_000, now))
    }

    // ---- what the notification says -----------------------------------------

    @Test
    fun `the body is the kind of call the caller chose`() {
        assertEquals("Voice call", callRingContent("Anna", video = false).body)
        assertEquals("Video call", callRingContent("Anna", video = true).body)
    }

    @Test
    fun `the title is the caller, or one word when this device cannot name them`() {
        assertEquals("Anna", callRingContent("Anna", video = false).title)
        assertEquals("Someone", callRingContent(null, video = false).title)
        assertEquals("Someone", callRingContent("   ", video = false).title)
    }

    // ---- the accept / decline mapping ---------------------------------------

    @Test
    fun `accept carries video only when the offer did`() {
        val accept = callRingCommand(CALL_RING_ACTION_ACCEPT, group, callA)
        assertEquals(CallRingAction.ACCEPT, accept?.action)
        assertEquals(true, accept?.withVideo)

        val audioOnly = callRingCommand(CALL_RING_ACTION_ACCEPT_AUDIO_ONLY, group, callA)
        assertEquals(CallRingAction.ACCEPT_AUDIO_ONLY, audioOnly?.action)
        assertEquals(false, audioOnly?.withVideo)
    }

    @Test
    fun `decline names the group so the End can be published`() {
        val decline = callRingCommand(CALL_RING_ACTION_DECLINE, group, callA)
        assertEquals(CallRingAction.DECLINE, decline?.action)
        assertEquals(group, decline?.groupIdHex)
        assertEquals(callA, decline?.callIdHex)
        assertEquals(false, decline?.withVideo)
    }

    @Test
    fun `a command missing its call, its group or its action is no command at all`() {
        assertNull(callRingCommand(CALL_RING_ACTION_ACCEPT, group, null))
        assertNull(callRingCommand(CALL_RING_ACTION_ACCEPT, group, "  "))
        assertNull(callRingCommand(CALL_RING_ACTION_DECLINE, null, callA))
        assertNull(callRingCommand(null, group, callA))
        assertNull(callRingCommand("android.intent.action.VIEW", group, callA))
        // Showing the ring screen is not answering it.
        assertNull(callRingCommand(CALL_RING_ACTION_SHOW, group, callA))
    }

    // ---- the call id round trip ---------------------------------------------

    @Test
    fun `a call id survives the trip through an intent extra`() {
        val bytes = ByteArray(16) { (it * 7).toByte() }
        assertEquals(bytes.toList(), callIdBytesFromHex(callIdHexOf(bytes))?.toList())
    }

    @Test
    fun `a malformed call id yields nothing rather than a wrong one`() {
        assertNull(callIdBytesFromHex(""))
        assertNull(callIdBytesFromHex("abc"))
        assertNull(callIdBytesFromHex("zz"))
    }
}
