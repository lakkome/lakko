// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiCallEndReason
import uniffi.lakko_ffi_uniffi.FfiCallMediaFlags
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody
import java.time.Instant

/**
 * `docs/ux/calls.md` section 2.1 — "a muted chat never rings".
 *
 * No client implemented it. Mute was stored and drawn everywhere — the
 * conversation row, the chat list, the ADR-0050 pairing bundle — and not one
 * ring path read it, so a muted conversation rang with the device ringtone,
 * vibrated, and took over the screen exactly like an unmuted one.
 *
 * ## What is asserted here, and what is asserted elsewhere
 *
 * NOT what a muted offer means. That is `ringDecision` in
 * `crates/lakko-ui-rules/src/ring.rs`, asserted there for all three clients —
 * including the two facts this file must not quietly restate: that `SILENT` is
 * a different answer from `TOO_LATE`, and that neither of them rings. A Kotlin
 * copy of those assertions is the drift this whole arc exists to stop, and a
 * JVM unit test cannot call the core anyway (no native library).
 *
 * What is asserted here is the WIRING — this client's own half, which nothing
 * else covers:
 *
 *  - the poll path reads the flag and carries it onto the ring;
 *  - a store that fails to answer rings rather than going silent;
 *  - a muted call is still routed, still occupies the slot, and still lands in
 *    the history as a missed call, because `SILENT` withholds the interruption
 *    and not the call;
 *  - the offer's age is derived from the offer's own stamp, so the verdict and
 *    the ring deadline can never disagree.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class MutedCallRingTest {

    private val groupA = "aa".repeat(32)
    private val groupB = "bb".repeat(32)
    private val call1 = ByteArray(16) { 1 }
    private val t0 = 1_800_000_000_000L

    private fun iso(ms: Long): String = Instant.ofEpochMilli(ms).toString()

    private fun offer(callId: ByteArray, video: Boolean = false) =
        FfiCallSignalingBody.Offer(callId, "v=0", 1uL, FfiCallMediaFlags(audio = true, video = video))

    private class Harness(
        val host: OneToOneCallHost<OneToOneCallHostTest.FakeSession>,
        val port: OneToOneCallHostTest.FakePort,
        val ringer: OneToOneCallHostTest.FakeRinger,
    )

    private fun kotlinx.coroutines.test.TestScope.harness(
        foreground: Boolean = true,
    ): Harness {
        val port = OneToOneCallHostTest.FakePort()
        val host = OneToOneCallHost<OneToOneCallHostTest.FakeSession>(
            scope = backgroundScope,
            clock = { t0 },
            timing = OneToOneCallTiming(ringTimeoutMs = TEST_RING_WINDOW_MS),
            warn = { _, _ -> },
        )
        val ringer = OneToOneCallHostTest.FakeRinger()
        host.attach(port)
        host.attachRinger(ringer)
        host.setForeground(foreground)
        runCurrent()
        return Harness(host, port, ringer)
    }

    // ---- the poll path reads mute -------------------------------------------

    @Test
    fun `an offer in a muted conversation reaches the ringer marked muted`() = runTest {
        val h = harness()
        h.port.mutedGroups += groupA
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()

        assertEquals(1, h.ringer.started.size)
        assertTrue(
            "the ringer has to be told, or it cannot withhold the ring",
            h.ringer.started.single().muted,
        )
        assertEquals(listOf(groupA), h.port.mutedAsked)
    }

    @Test
    fun `an offer in an unmuted conversation is not marked muted`() = runTest {
        val h = harness()
        h.port.mutedGroups += groupB
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()

        assertFalse(h.ringer.started.single().muted)
    }

    @Test
    fun `a mute store that fails rings, rather than silencing the phone`() = runTest {
        // The direction matters. Losing a call to a store that would not open
        // is invisible: nothing rings, nothing is shown, and the person blames
        // the call. Ringing a chat they muted is merely annoying, and they can
        // see why it happened.
        val h = harness()
        h.port.muteThrows = true
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()

        assertEquals(1, h.ringer.started.size)
        assertFalse(h.ringer.started.single().muted)
    }

    // ---- muted is not dropped ------------------------------------------------

    @Test
    fun `a muted call still occupies the slot and can still be answered`() = runTest {
        // `SILENT` withholds the interruption, not the call. The host keeps
        // routing the offer exactly as it always did — which is also what
        // makes the missed-call record below possible.
        val h = harness()
        h.port.mutedGroups += groupA
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()

        assertEquals(1, h.port.created.size)
        assertEquals(groupA, h.host.active.value?.groupIdHex)
        assertTrue(h.host.hasCall)
    }

    @Test
    fun `a muted call nobody answered is recorded as missed, exactly like an unmuted one`() = runTest {
        // The whole reason `SILENT` is not `TOO_LATE` and not a dropped call:
        // a person who muted a conversation asked not to be interrupted by it,
        // not to be kept from finding out who tried to reach them.
        val h = harness(foreground = false)
        h.port.mutedGroups += groupA
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        h.host.dismissRingingOffer(callIdHexOf(call1))
        runCurrent()

        assertEquals(
            listOf(groupA to CallTranscriptEntry(CallOutcome.MISSED, video = false, durationSecs = null)),
            h.port.recorded,
        )
    }

    @Test
    fun `declining a muted call still tells the caller`() = runTest {
        val h = harness(foreground = false)
        h.port.mutedGroups += groupA
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        h.host.declineRinging(groupA, callIdHexOf(call1))
        runCurrent()

        assertTrue(
            "a muted call the person refuses is refused on the wire like any other",
            h.port.published.any { it.first == groupA && it.second.contains("${FfiCallEndReason.DECLINED}") },
        )
    }

    // ---- the age the shared rule is asked about ------------------------------

    /** The age the ringer hands the core, composed exactly as production does. */
    private fun ageOf(offeredAtMs: Long?): Long =
        callRingOfferAgeMs(callRingRemainingMs(offeredAtMs, t0, TEST_RING_WINDOW_MS), TEST_RING_WINDOW_MS)

    @Test
    fun `the offer's age and the ring deadline are two views of one number`() {
        // The core takes an age; this client derives it from the very reading
        // that drives the deadline, so a ring that still has time left can
        // never be judged over, or the reverse.
        for (age in listOf(0L, 1L, 20_000L, TEST_RING_WINDOW_MS - 1)) {
            val offeredAt = t0 - age
            assertEquals(age, ageOf(offeredAt))
            assertEquals(
                TEST_RING_WINDOW_MS,
                ageOf(offeredAt) + callRingRemainingMs(offeredAt, t0, TEST_RING_WINDOW_MS),
            )
        }
    }

    @Test
    fun `an unreadable or future stamp is age zero, so it rings for the whole window`() {
        // This client's own correction, and it is the ringing direction: a
        // wake that lost its stamp must not be judged as a call already over.
        assertEquals(0L, ageOf(null))
        assertEquals(0L, ageOf(t0 + 60_000))
    }

    @Test
    fun `an offer past the window is the whole window old, never more`() {
        // The age is what the core compares against the window, so it has to
        // saturate: a ten-minute-old offer and a forty-six-second-old one are
        // the same answer, and neither may overflow the comparison.
        assertEquals(TEST_RING_WINDOW_MS, ageOf(t0 - TEST_RING_WINDOW_MS))
        assertEquals(TEST_RING_WINDOW_MS, ageOf(t0 - 600_000))
    }

    @Test
    fun `the age is clamped even if the two numbers are handed over inconsistently`() {
        // Defence in depth: the core's window is a `u64`, so an age that went
        // negative would wrap to something enormous and silently read as
        // `TooLate` — a phone that stops ringing and says nothing.
        assertEquals(0L, callRingOfferAgeMs(TEST_RING_WINDOW_MS + 5_000, TEST_RING_WINDOW_MS))
        assertEquals(TEST_RING_WINDOW_MS, callRingOfferAgeMs(-1L, TEST_RING_WINDOW_MS))
    }

    // ---- the fold carries it, and decides nothing ----------------------------

    @Test
    fun `the push fold carries mute onto the ring it builds`() {
        val outcome = foldCallRingSignals(
            groupIdHex = groupA,
            signals = listOf(
                CallRingSignal.Offer(callIdHexOf(call1), video = false, offeredAtMs = t0 - 1_000),
            ),
            nowMs = t0,
            windowMs = TEST_RING_WINDOW_MS,
            muted = true,
        )
        assertTrue(outcome.ring?.muted == true)
    }

    @Test
    fun `mute does not make the fold drop the offer`() {
        // Deliberate: the fold must not answer half of a rule the core owns.
        // Dropping it here would lose the missed-call record as well as the
        // ring, which is exactly what `SILENT` is defined not to do.
        val muted = foldCallRingSignals(
            groupA,
            listOf(CallRingSignal.Offer(callIdHexOf(call1), video = false, offeredAtMs = t0)),
            t0,
            TEST_RING_WINDOW_MS,
            muted = true,
        )
        assertEquals(callIdHexOf(call1), muted.ring?.callIdHex)

        // And an offer past the window is still dropped, muted or not: that is
        // a different question, and the fold does own it.
        val stale = foldCallRingSignals(
            groupA,
            listOf(
                CallRingSignal.Offer(
                    callIdHexOf(call1),
                    video = false,
                    offeredAtMs = t0 - TEST_RING_WINDOW_MS - 1,
                ),
            ),
            t0,
            TEST_RING_WINDOW_MS,
            muted = true,
        )
        assertNull(stale.ring)
    }

    @Test
    fun `a ring defaults to unmuted, so a path that cannot resolve it still rings`() {
        val ring = IncomingCallRing(
            groupIdHex = groupA,
            callIdHex = callIdHexOf(call1),
            callerName = null,
            video = false,
            offeredAtMs = t0,
        )
        assertFalse(ring.muted)
    }
}
