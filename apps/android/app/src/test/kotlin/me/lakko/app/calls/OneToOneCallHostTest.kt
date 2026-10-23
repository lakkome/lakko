// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.time.Instant
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiCallEndReason
import uniffi.lakko_ffi_uniffi.FfiCallMediaFlags
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody

/**
 * The app-level 1x1 call host: what rings, what is answered busy, what is
 * applied to the call in the slot, and what is dropped.
 *
 * ## The defect behind these tests
 *
 * The 1x1 call used to live in the chat view model, fed through a single
 * repository sink slot filtered to the chat opened last. The all-groups poll
 * delivered an offer for any other chat to a sink that dropped it, and the
 * cursor moved past the row, so the call never rang. The host is fed from every
 * poll path and owns the call for the whole session; these tests drive it with
 * fake sessions on a virtual clock (no UniFFI `.so`, no WebRTC).
 */
@OptIn(ExperimentalCoroutinesApi::class)
class OneToOneCallHostTest {

    private val groupA = "aa".repeat(16)
    private val groupB = "bb".repeat(16)
    private val call1 = ByteArray(16) { 0x11 }
    private val call2 = ByteArray(16) { 0x22 }
    private val t0 = 1_800_000_000_000L

    /** Mirrors the parts of [CallSession] the host drives. */
    class FakeSession(
        override val callId: ByteArray,
        /** The real session ships its `End` through this on a hang-up. */
        private val outbound: ((ByteArray) -> Unit)? = null,
    ) : OneToOneCallSession {
        val mutableState = MutableStateFlow(CallSession.WebCallState.Idle)
        override val state: StateFlow<CallSession.WebCallState> = mutableState
        val applied = mutableListOf<FfiCallSignalingBody>()
        val hangUps = mutableListOf<CallSession.WebCallEndReason>()
        var keySeed: ByteArray? = null
        var acceptedWithVideo: Boolean? = null
        var declined = false
        var dialed: Pair<Boolean, Boolean>? = null

        override fun setCallKey(seed: ByteArray, localLeaf: Int) {
            keySeed = seed
        }

        override fun applyIncomingSignaling(body: FfiCallSignalingBody) {
            applied += body
            when (body) {
                is FfiCallSignalingBody.Offer ->
                    if (mutableState.value == CallSession.WebCallState.Idle) {
                        mutableState.value = CallSession.WebCallState.Ringing
                    }
                is FfiCallSignalingBody.End -> mutableState.value = CallSession.WebCallState.Done
                else -> Unit
            }
        }

        override fun dial(audio: Boolean, video: Boolean) {
            dialed = audio to video
            mutableState.value = CallSession.WebCallState.Outgoing
        }

        override fun acceptCall(withVideo: Boolean) {
            acceptedWithVideo = withVideo
            mutableState.value = CallSession.WebCallState.Connecting
        }

        override fun declineCall() {
            declined = true
            mutableState.value = CallSession.WebCallState.Done
        }

        override fun hangUp(reason: CallSession.WebCallEndReason) {
            hangUps += reason
            outbound?.invoke("end:session:${callIdHexOf(callId)}".toByteArray())
            mutableState.value = CallSession.WebCallState.Done
        }
    }

    class FakePort : OneToOneCallPort<FakeSession> {
        val created = mutableListOf<Pair<String, FakeSession>>()
        val published = mutableListOf<Pair<String, String>>()

        /** docs/ux/calls.md section 2.1 — conversations the person muted. */
        val mutedGroups = mutableSetOf<String>()
        val mutedAsked = mutableListOf<String>()
        var muteThrows = false

        override suspend fun isMuted(groupIdHex: String): Boolean {
            mutedAsked += groupIdHex
            if (muteThrows) throw RuntimeException("mute store failure")
            return groupIdHex in mutedGroups
        }

        /** docs/ux/calls.md section 6 — the transcript rows the host wrote. */
        val recorded = mutableListOf<Pair<String, CallTranscriptEntry>>()
        /** When each recorded call started, in the order of [recorded]. */
        val recordedStarts = mutableListOf<Long?>()
        var keyDelayMs = 0L
        var keyAvailable = true

        override fun recordCall(groupIdHex: String, entry: CallTranscriptEntry, startedAtMs: Long?) {
            recorded += groupIdHex to entry
            recordedStarts += startedAtMs
        }

        override fun createSession(
            groupIdHex: String,
            callId: ByteArray,
            outbound: (ByteArray) -> Unit,
        ): FakeSession =
            FakeSession(callId, if (sessionsShipEnd) outbound else null).also { created += groupIdHex to it }

        /** Make sessions emit their `End` on hang-up, as [CallSession] does. */
        var sessionsShipEnd = false

        override suspend fun deriveCallKey(groupIdHex: String, callId: ByteArray): OneToOneCallKey? {
            if (keyDelayMs > 0) delay(keyDelayMs)
            return if (keyAvailable) OneToOneCallKey(ByteArray(32) { 7 }, 1) else null
        }

        override suspend fun publish(groupIdHex: String, bytes: ByteArray) {
            published += groupIdHex to String(bytes)
        }

        override fun composeEnd(callId: ByteArray, reason: FfiCallEndReason): ByteArray =
            "end:$reason:${callIdHexOf(callId)}".toByteArray()
    }

    /** Records the two verbs that make the phone ring and stop it. */
    class FakeRinger : IncomingCallRingPort {
        val started = mutableListOf<IncomingCallRing>()
        val stopped = mutableListOf<String>()

        override fun startRinging(ring: IncomingCallRing) {
            started += ring
        }

        override fun stopRinging(callIdHex: String) {
            stopped += callIdHex
        }
    }

    private class Harness(
        val host: OneToOneCallHost<FakeSession>,
        val port: FakePort,
        val ringer: FakeRinger,
        var now: Long,
    )

    private fun TestScope.harness(foreground: Boolean = true): Harness {
        val port = FakePort()
        lateinit var h: Harness
        val host = OneToOneCallHost<FakeSession>(
            scope = backgroundScope,
            clock = { h.now },
            // Stated here rather than read from the core: a JVM unit test does
            // not load the native library. See [TEST_RING_WINDOW_MS].
            timing = OneToOneCallTiming(ringTimeoutMs = TEST_RING_WINDOW_MS),
            warn = { _, _ -> },
        )
        val ringer = FakeRinger()
        h = Harness(host, port, ringer, t0)
        host.attach(port)
        host.attachRinger(ringer)
        host.setForeground(foreground)
        runCurrent()
        return h
    }

    private fun iso(ms: Long): String = Instant.ofEpochMilli(ms).toString()

    private fun offer(callId: ByteArray, video: Boolean = false) =
        FfiCallSignalingBody.Offer(callId, "v=0", 1uL, FfiCallMediaFlags(audio = true, video = video))

    private fun ice(callId: ByteArray) = FfiCallSignalingBody.IceCandidate(callId, "candidate", "0", 0u)

    private fun end(callId: ByteArray) = FfiCallSignalingBody.End(callId, FfiCallEndReason.HUNG_UP)

    private fun Harness.session(): FakeSession? = host.active.value?.session

    // ---- Routing from any poll ----------------------------------------------

    @Test
    fun `an offer from a group whose chat is not open rings`() = runTest {
        val h = harness()
        // Delivered as the all-groups poll delivers it: no chat view exists.
        h.host.deliver(groupB, messageId = 5, body = offer(call1, video = true), createdAt = iso(t0 - 1_000))
        runCurrent()

        val call = h.host.active.value
        assertNotNull(call)
        assertEquals(groupB, call!!.groupIdHex)
        assertTrue(call.incoming)
        assertTrue(call.remoteOffersVideo)
        assertEquals(CallSession.WebCallState.Ringing, call.session.state.value)
        assertNotNull("the SFrame seed is armed while ringing", call.session.keySeed)
        assertTrue(h.host.hasCall)
    }

    @Test
    fun `each signaling row is delivered once, even when re-fetched`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 10, offer(call1), iso(t0))
        h.host.deliver(groupA, 11, ice(call1), iso(t0))
        // A held cursor re-fetches the same batch.
        h.host.deliver(groupA, 10, offer(call1), iso(t0))
        h.host.deliver(groupA, 11, ice(call1), iso(t0))
        // An older row after a newer one is never replayed.
        h.host.deliver(groupA, 9, ice(call1), iso(t0))
        runCurrent()

        val applied = h.session()!!.applied
        assertEquals(2, applied.size)
        assertEquals(1, h.port.created.size)
    }

    @Test
    fun `rows of one call apply in order even while the offer's key is still being derived`() = runTest {
        val h = harness()
        h.port.keyDelayMs = 500
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        h.host.deliver(groupA, 2, ice(call1), iso(t0))
        h.host.deliver(groupA, 3, ice(call1), iso(t0))
        runCurrent()
        // The consumer is suspended in the key derivation with both ICE rows
        // queued behind the offer; nothing has been applied or dropped yet.
        assertNull(h.host.active.value)
        advanceTimeBy(501)
        runCurrent()

        val applied = h.session()!!.applied
        assertEquals(3, applied.size)
        assertTrue(applied[0] is FfiCallSignalingBody.Offer)
        assertTrue(applied[1] is FfiCallSignalingBody.IceCandidate)
        assertTrue(applied[2] is FfiCallSignalingBody.IceCandidate)
    }

    @Test
    fun `message ids are tracked per group`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 50, offer(call1), iso(t0))
        // A lower id on ANOTHER group's journal is a different row.
        h.host.deliver(groupB, 3, offer(call2), iso(t0))
        runCurrent()
        assertEquals(listOf("end:BUSY:${callIdHexOf(call2)}"), h.port.published.filter { it.first == groupB }.map { it.second })
    }

    // ---- Stale offers --------------------------------------------------------

    @Test
    fun `an offer older than sixty seconds does not ring and is not answered busy`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0 - STALE_OFFER_MS - 1))
        runCurrent()
        assertNull(h.host.active.value)
        assertTrue(h.port.published.isEmpty())
        assertTrue(h.port.created.isEmpty())
    }

    @Test
    fun `an offer just inside sixty seconds rings`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0 - STALE_OFFER_MS))
        runCurrent()
        assertEquals(CallSession.WebCallState.Ringing, h.session()?.state?.value)
    }

    @Test
    fun `an offer with an unreadable row timestamp does not ring`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), "not a timestamp")
        runCurrent()
        assertNull(h.host.active.value)
    }

    // ---- One call at a time --------------------------------------------------

    @Test
    fun `a second offer while ringing is answered busy on its own group`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        val first = h.session()

        h.host.deliver(groupB, 1, offer(call2), iso(t0))
        runCurrent()

        assertSame(first, h.session())
        assertEquals(1, h.port.created.size)
        assertEquals(listOf(groupB to "end:BUSY:${callIdHexOf(call2)}"), h.port.published)
    }

    @Test
    fun `a second offer during a connected call is answered busy`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        h.host.accept(withVideo = false)
        runCurrent()
        h.session()!!.mutableState.value = CallSession.WebCallState.Connected
        runCurrent()

        h.host.deliver(groupA, 2, offer(call2), iso(t0))
        runCurrent()
        assertEquals(listOf(groupA to "end:BUSY:${callIdHexOf(call2)}"), h.port.published)
        assertEquals(CallSession.WebCallState.Connected, h.session()!!.state.value)
    }

    @Test
    fun `a live group call makes this device busy for a 1x1 offer`() = runTest {
        val h = harness()
        h.host.setOtherCallActive("group", true)
        h.host.deliver(groupB, 1, offer(call1), iso(t0))
        runCurrent()
        assertNull(h.host.active.value)
        assertEquals(listOf(groupB to "end:BUSY:${callIdHexOf(call1)}"), h.port.published)
        assertFalse(h.host.startOutgoing(groupB))
    }

    // ---- C-11 / C-12 ---------------------------------------------------------

    @Test
    fun `group-control bodies never reach the 1x1 session`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        val media = FfiCallMediaFlags(audio = true, video = false)
        h.host.deliver(groupA, 2, FfiCallSignalingBody.GroupStart(call1, 1u, 1uL, media), iso(t0))
        h.host.deliver(groupA, 3, FfiCallSignalingBody.GroupJoin(call1, 1u, 1uL), iso(t0))
        h.host.deliver(groupA, 4, FfiCallSignalingBody.GroupLeave(call1, 1u, FfiCallEndReason.HUNG_UP), iso(t0))
        runCurrent()

        assertEquals(1, h.session()!!.applied.size)
        // Nor does a group body ever open a session on its own.
        val fresh = harness()
        fresh.host.deliver(groupB, 1, FfiCallSignalingBody.GroupStart(call2, 1u, 1uL, media), iso(t0))
        runCurrent()
        assertNull(fresh.host.active.value)
    }

    @Test
    fun `signaling for a different call id is not applied (C-12)`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        h.host.deliver(groupA, 2, ice(call2), iso(t0))
        h.host.deliver(groupA, 3, end(call2), iso(t0))
        h.host.deliver(groupA, 4, FfiCallSignalingBody.Answer(call2, "v=0"), iso(t0))
        runCurrent()
        assertEquals(1, h.session()!!.applied.size)
        assertEquals(CallSession.WebCallState.Ringing, h.session()!!.state.value)
    }

    @Test
    fun `the call's own id on another group's journal is not this call (C-12 widened)`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        h.host.deliver(groupB, 1, end(call1), iso(t0))
        h.host.deliver(groupB, 2, ice(call1), iso(t0))
        runCurrent()
        assertEquals(1, h.session()!!.applied.size)
        assertEquals(CallSession.WebCallState.Ringing, h.session()!!.state.value)
    }

    @Test
    fun `the caller hanging up while ringing clears the banner and the call never rings again`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        h.host.deliver(groupA, 2, end(call1), iso(t0))
        runCurrent()
        assertNull(h.host.active.value)

        // A repeat of the same offer (another path, a later row) stays silent.
        h.host.deliver(groupA, 3, offer(call1), iso(t0))
        runCurrent()
        assertNull(h.host.active.value)
        assertTrue(h.port.published.isEmpty())
    }

    // ---- Timeouts ------------------------------------------------------------

    @Test
    fun `an unanswered ring ends as timed out`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        val session = h.session()!!

        advanceTimeBy(TEST_RING_WINDOW_MS - 1)
        runCurrent()
        assertTrue(session.hangUps.isEmpty())

        advanceTimeBy(2)
        runCurrent()
        assertEquals(listOf(CallSession.WebCallEndReason.TimedOut), session.hangUps)
        runCurrent()
        assertNull("an unanswered call leaves no banner behind", h.host.active.value)
    }

    @Test
    fun `accepting cancels the ring timeout and a call that never connects times out`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        advanceTimeBy(30_000)
        h.host.accept(withVideo = true)
        runCurrent()
        val session = h.session()!!
        assertEquals(true, session.acceptedWithVideo)
        assertTrue(h.host.active.value!!.accepted)

        // Past the original ring deadline: nothing, the ring timer is gone.
        advanceTimeBy(20_000)
        runCurrent()
        assertTrue(session.hangUps.isEmpty())

        advanceTimeBy(OneToOneCallTiming(ringTimeoutMs = TEST_RING_WINDOW_MS).establishTimeoutMs)
        runCurrent()
        assertEquals(listOf(CallSession.WebCallEndReason.TimedOut), session.hangUps)
        runCurrent()
        assertNotNull("an accepted call keeps its screen until dismissed", h.host.active.value)
        h.host.clearFinished()
        runCurrent()
        assertNull(h.host.active.value)
    }

    @Test
    fun `a connected call is never timed out`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        h.host.accept(withVideo = false)
        runCurrent()
        h.session()!!.mutableState.value = CallSession.WebCallState.Connected
        runCurrent()
        advanceTimeBy(10 * 60_000L)
        runCurrent()
        assertTrue(h.session()!!.hangUps.isEmpty())
    }

    @Test
    fun `an outgoing call is prepared, dialed, and times out unanswered`() = runTest {
        val h = harness()
        // Since the camera check went (2026-09-21) both kinds are dialled by
        // `startOutgoing` itself, so what this test watches is the dial and
        // the unanswered window, not a wait in between.
        assertTrue(h.host.startOutgoing(groupA, RequestedCallKind.VOICE))
        runCurrent()
        val call = h.host.active.value!!
        assertFalse(call.incoming)
        assertNotNull(call.session.keySeed)
        assertEquals(true to false, call.session.dialed)

        advanceTimeBy(OneToOneCallTiming(ringTimeoutMs = TEST_RING_WINDOW_MS).establishTimeoutMs + 1)
        runCurrent()
        assertEquals(listOf(CallSession.WebCallEndReason.TimedOut), call.session.hangUps)
        assertFalse(h.host.hasCall)
    }

    @Test
    fun `a second outgoing call is refused while one is up`() = runTest {
        val h = harness()
        assertTrue(h.host.startOutgoing(groupA))
        runCurrent()
        assertFalse(h.host.startOutgoing(groupB))
        runCurrent()
        assertEquals(1, h.port.created.size)
    }

    @Test
    fun `no outgoing call is ever left prepared and undialled`() = runTest {
        // This replaces `cancelling a prepared call clears the slot`, whose
        // premise the 2026-09-21 removal of the camera check took away: both
        // kinds now dial in the same breath as they are prepared, so there is
        // no undialled state for a close control to cancel. `cancelPrepared`
        // and `AppCallHost.endCurrentCall`'s Idle branch are kept as the
        // state machine's own defence, not as a reachable route, and this
        // pins the reason they are unreachable rather than deleting them.
        val h = harness()
        for (kind in RequestedCallKind.entries) {
            assertTrue(h.host.startOutgoing(groupA, kind, hasCamera = true))
            runCurrent()
            val call = h.host.active.value!!
            assertNotNull("$kind must be dialled by startOutgoing itself", call.session.dialed)
            h.host.hangUp()
            runCurrent()
            h.host.clearFinished()
            runCurrent()
        }
    }

    @Test
    fun `declining ends the call and clears the banner`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        val session = h.session()!!
        h.host.decline()
        runCurrent()
        assertTrue(session.declined)
        assertNull(h.host.active.value)
    }

    // ---- Background ----------------------------------------------------------

    @Test
    fun `an offer received in the background rings on foreground while still fresh`() = runTest {
        val h = harness(foreground = false)
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        assertNull("nothing rings off-screen", h.host.active.value)

        h.now = t0 + 40_000
        h.host.setForeground(true)
        runCurrent()
        assertEquals(CallSession.WebCallState.Ringing, h.session()?.state?.value)
    }

    @Test
    fun `an offer received in the background does not ring once stale`() = runTest {
        val h = harness(foreground = false)
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        h.now = t0 + STALE_OFFER_MS + 1
        h.host.setForeground(true)
        runCurrent()
        assertNull(h.host.active.value)
        assertTrue(h.port.published.isEmpty())
    }

    @Test
    fun `ICE candidates of a held background offer reach the call when it rings`() = runTest {
        val h = harness(foreground = false)
        val c1 = FfiCallSignalingBody.IceCandidate(call1, "candidate-1", "0", 0u)
        val c2 = FfiCallSignalingBody.IceCandidate(call1, "candidate-2", "1", 1u)
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        h.host.deliver(groupA, 2, c1, iso(t0))
        h.host.deliver(groupA, 3, c2, iso(t0))
        // Another call's candidate is not this call's (C-12).
        h.host.deliver(groupA, 4, ice(call2), iso(t0))
        runCurrent()
        assertNull(h.host.active.value)

        h.now = t0 + 5_000
        h.host.setForeground(true)
        runCurrent()

        val applied = h.session()!!.applied
        assertEquals(listOf(offer(call1), c1, c2).map { it.toString() }, applied.map { it.toString() })
    }

    @Test
    fun `a held offer's candidates are forgotten with the offer`() = runTest {
        val h = harness(foreground = false)
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        h.host.deliver(groupA, 2, ice(call1), iso(t0))
        h.host.deliver(groupA, 3, end(call1), iso(t0))
        // A fresh call's offer is held after the first one ended.
        h.host.deliver(groupA, 4, offer(call2), iso(t0))
        runCurrent()
        h.host.setForeground(true)
        runCurrent()

        val applied = h.session()!!.applied
        assertEquals(1, applied.size)
        assertTrue(applied.single() is FfiCallSignalingBody.Offer)
    }

    @Test
    fun `a held background offer whose caller hung up never rings`() = runTest {
        val h = harness(foreground = false)
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        h.host.deliver(groupA, 2, end(call1), iso(t0))
        runCurrent()
        h.host.setForeground(true)
        runCurrent()
        assertNull(h.host.active.value)
    }

    // ---- Lifecycle -----------------------------------------------------------

    @Test
    fun `reset hangs up and forgets the delivered rows`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 7, offer(call1), iso(t0))
        runCurrent()
        val session = h.session()!!
        h.host.reset()
        runCurrent()
        assertEquals(listOf(CallSession.WebCallEndReason.HungUp), session.hangUps)
        assertNull(h.host.active.value)

        // A new account's group may reuse nothing, but the high-water mark is
        // gone: a fresh row at a lower id is accepted.
        h.host.deliver(groupA, 1, offer(call2), iso(t0))
        runCurrent()
        assertNotNull(h.host.active.value)
    }

    // ---- A one-to-one that closes under a call (UX 16.4) ----------------------

    @Test
    fun `a connected call ends locally when its chat closes, and no End is published`() = runTest {
        val h = harness()
        h.port.sessionsShipEnd = true
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        h.host.accept(withVideo = false)
        runCurrent()
        val session = h.session()!!
        session.mutableState.value = CallSession.WebCallState.Connected
        runCurrent()

        h.host.conversationMoved(groupA.uppercase())
        runCurrent()

        assertEquals(listOf(CallSession.WebCallEndReason.HungUp), session.hangUps)
        assertFalse(h.host.hasCall)
        assertTrue("nothing is published into a closed one-to-one", h.port.published.isEmpty())
        assertEquals(1, h.port.recorded.size)
        // The ended screen is dismissed the ordinary way.
        h.host.clearFinished()
        runCurrent()
        assertNull(h.host.active.value)
    }

    @Test
    fun `a ringing call stops ringing and leaves the slot when its chat closes`() = runTest {
        val h = harness()
        h.port.sessionsShipEnd = true
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        assertNotNull(h.host.active.value)

        h.host.conversationMoved(groupA)
        runCurrent()

        assertNull(h.host.active.value)
        assertTrue(callIdHexOf(call1) in h.ringer.stopped)
        assertTrue(h.port.published.isEmpty())
    }

    @Test
    fun `a held offer of a chat that closes never rings`() = runTest {
        val h = harness(foreground = false)
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        h.host.conversationMoved(groupA)
        runCurrent()
        assertTrue(callIdHexOf(call1) in h.ringer.stopped)
        h.host.setForeground(true)
        runCurrent()
        assertNull(h.host.active.value)
        assertTrue(h.port.published.isEmpty())
    }

    @Test
    fun `a call in another chat is untouched when a chat closes, and a closed chat places no call`() = runTest {
        val h = harness()
        h.port.sessionsShipEnd = true
        h.host.deliver(groupB, 1, offer(call1), iso(t0))
        runCurrent()
        val session = h.session()!!

        h.host.conversationMoved(groupA)
        runCurrent()
        assertTrue(session.hangUps.isEmpty())
        assertTrue(h.host.hasCall)

        h.host.hangUp()
        runCurrent()
        assertEquals("the other chat's End still goes out", 1, h.port.published.size)
        h.host.clearFinished()
        runCurrent()
        assertFalse(h.host.startOutgoing(groupA))
    }

    @Test
    fun `routing decisions`() {
        val occupant = OneToOneSlotKey(groupA, callIdHexOf(call1))
        fun route(body: FfiCallSignalingBody, group: String = groupA, occupied: OneToOneSlotKey? = occupant, busy: Boolean = false, ended: Set<String> = emptySet(), created: Long? = t0) =
            routeOneToOneSignaling(body, group, created, t0, occupied, ended, busy)

        assertEquals(OneToOneRoute.APPLY, route(ice(call1)))
        assertEquals(OneToOneRoute.APPLY, route(offer(call1)))
        assertEquals(OneToOneRoute.DROP, route(ice(call1), group = groupB))
        assertEquals(OneToOneRoute.BUSY, route(offer(call2)))
        assertEquals(OneToOneRoute.RING, route(offer(call2), occupied = null))
        assertEquals(OneToOneRoute.BUSY, route(offer(call2), occupied = null, busy = true))
        assertEquals(OneToOneRoute.DROP, route(offer(call2), occupied = null, ended = setOf(callIdHexOf(call2))))
        assertEquals(OneToOneRoute.DROP, route(offer(call2), occupied = null, created = null))
        assertEquals(OneToOneRoute.DROP, route(end(call2), occupied = null))
        assertEquals(
            OneToOneRoute.DROP,
            route(FfiCallSignalingBody.GroupJoin(call1, 1u, 1uL)),
        )
    }

    // ---- The phone ringing --------------------------------------------------
    //
    // docs/ux/calls.md section 2.1. These tests drive the path the test stack
    // actually exercises: the app's own poll. The FCM wake reaches the same two
    // verbs through the same port, and the emulator never receives a push
    // because the test stack has no FCM credentials, so a ring wired only to
    // the wake would be a ring nobody could check.

    @Test
    fun `an offer rings the phone with no screen showing`() = runTest {
        val h = harness(foreground = false)
        h.host.deliver(groupA, 1, offer(call1, video = true), iso(t0))
        runCurrent()

        assertEquals(1, h.ringer.started.size)
        val ring = h.ringer.started.single()
        assertEquals(callIdHexOf(call1), ring.callIdHex)
        assertEquals(groupA, ring.groupIdHex)
        assertTrue("the ring says what kind of call it is", ring.video)
        // Backgrounded, the offer is still only HELD: nothing rings on a screen
        // nobody is looking at, and the ring window is not burned off-screen.
        assertNull(h.host.active.value)
    }

    @Test
    fun `an offer rings the phone with the app open too`() = runTest {
        // The in-app banner has never made a sound; the notification channel is
        // the only thing on Android that produces a ringtone.
        val h = harness(foreground = true)
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()

        assertEquals(listOf(callIdHexOf(call1)), h.ringer.started.map { it.callIdHex })
        assertNotNull("and the call is live in the app", h.host.active.value)
    }

    @Test
    fun `answering in the app stops the ring`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        h.host.accept(withVideo = false)
        runCurrent()

        assertTrue(callIdHexOf(call1) in h.ringer.stopped)
        assertEquals(false, h.session()!!.acceptedWithVideo)
    }

    @Test
    fun `the caller hanging up stops the ring of an offer this device only holds`() = runTest {
        val h = harness(foreground = false)
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        h.host.deliver(groupA, 2, end(call1), iso(t0))
        runCurrent()

        assertEquals(listOf(callIdHexOf(call1)), h.ringer.stopped)
        // And it must not ring later, when the app comes back.
        h.host.setForeground(true)
        runCurrent()
        assertNull(h.host.active.value)
    }

    @Test
    fun `answering from the ring screen takes the call the app has not rung yet`() = runTest {
        val h = harness(foreground = false)
        h.host.deliver(groupA, 1, offer(call1, video = true), iso(t0))
        runCurrent()
        // The user answers on the lock screen: there is no session yet.
        h.host.acceptRinging(callIdHexOf(call1), withVideo = true)
        runCurrent()
        assertNull(h.host.active.value)

        // Answering brought the app forward; the held offer rings there and the
        // answer already taken is applied to it.
        h.host.setForeground(true)
        runCurrent()

        val call = h.host.active.value
        assertNotNull(call)
        assertTrue(call!!.accepted)
        assertEquals(true, h.session()!!.acceptedWithVideo)
    }

    @Test
    fun `an answer from the ring screen never answers a later, unrelated call`() = runTest {
        val h = harness(foreground = false)
        h.host.acceptRinging(callIdHexOf(call1), withVideo = false)
        runCurrent()
        h.host.deliver(groupA, 1, offer(call2), iso(t0))
        h.host.setForeground(true)
        runCurrent()

        assertNotNull(h.host.active.value)
        assertNull("call2 was never answered on the ring screen", h.session()!!.acceptedWithVideo)
    }

    @Test
    fun `declining from the notification publishes the End with no session`() = runTest {
        val h = harness(foreground = false)
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        h.host.declineRinging(groupA, callIdHexOf(call1))
        runCurrent()

        assertEquals(
            listOf(groupA to "end:DECLINED:${callIdHexOf(call1)}"),
            h.port.published,
        )
        assertTrue(callIdHexOf(call1) in h.ringer.stopped)
        // The refused call must not ring when the app is next opened.
        h.host.setForeground(true)
        runCurrent()
        assertNull(h.host.active.value)
    }

    @Test
    fun `declining a ringing call in the app goes through its session`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        val session = h.session()
        h.host.declineRinging(groupA, callIdHexOf(call1))
        runCurrent()

        assertTrue("the session publishes its own End", session?.declined == true)
        assertTrue(h.port.published.isEmpty())
    }

    @Test
    fun `a ring that timed out does not ring again on the next foreground`() = runTest {
        val h = harness(foreground = false)
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        // The ringer's window ran out while the app was away.
        h.host.dismissRingingOffer(callIdHexOf(call1))
        runCurrent()

        h.host.setForeground(true)
        runCurrent()
        assertNull(h.host.active.value)
    }

    // ---- What a finished call leaves behind (section 6) ----------------------

    @Test
    fun `an answered incoming call writes an incoming row with its duration`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        h.host.accept(withVideo = false)
        runCurrent()
        h.session()!!.mutableState.value = CallSession.WebCallState.Connected
        runCurrent()
        h.now = t0 + 724_000L
        h.host.hangUp()
        runCurrent()

        assertEquals(
            listOf(groupA to CallTranscriptEntry(CallOutcome.INCOMING, video = false, durationSecs = 724)),
            h.port.recorded,
        )
    }

    @Test
    fun `an incoming call is filed under when it began to ring, not when it ended`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        h.now = t0 + 5_000L
        h.host.accept(withVideo = false)
        runCurrent()
        h.session()!!.mutableState.value = CallSession.WebCallState.Connected
        runCurrent()
        h.now = t0 + 60_000L
        h.host.hangUp()
        runCurrent()

        assertEquals(listOf<Long?>(t0), h.port.recordedStarts)
    }

    @Test
    fun `an outgoing call is filed under when it was dialled`() = runTest {
        val h = harness()
        h.now = t0 + 1_000L
        assertTrue(h.host.startOutgoing(groupA, RequestedCallKind.VOICE))
        runCurrent()
        h.now = t0 + 30_000L
        h.host.hangUp()
        runCurrent()

        assertEquals(listOf<Long?>(t0 + 1_000L), h.port.recordedStarts)
    }

    @Test
    fun `an incoming call that rang out writes a missed row`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1, video = true), iso(t0))
        runCurrent()
        advanceTimeBy(TEST_RING_WINDOW_MS + 1)
        runCurrent()

        assertEquals(
            listOf(groupA to CallTranscriptEntry(CallOutcome.MISSED, video = true, durationSecs = null)),
            h.port.recorded,
        )
    }

    @Test
    fun `declining a ringing call writes a declined row, never a missed one`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        h.host.decline()
        runCurrent()

        assertEquals(
            listOf(groupA to CallTranscriptEntry(CallOutcome.DECLINED, video = false, durationSecs = null)),
            h.port.recorded,
        )
    }

    @Test
    fun `an offer refused because this device is busy is still recorded as missed`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        // A second call on another journal while the first occupies the slot.
        h.host.deliver(groupB, 1, offer(call2), iso(t0))
        runCurrent()

        assertTrue(
            h.port.recorded.contains(
                groupB to CallTranscriptEntry(CallOutcome.MISSED, video = false, durationSecs = null),
            ),
        )
    }

    @Test
    fun `an offer only ever held is recorded when its ring window runs out`() = runTest {
        val h = harness(foreground = false)
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
    fun `an outgoing call the peer declined writes a declined row`() = runTest {
        val h = harness()
        h.host.startOutgoing(groupA, RequestedCallKind.VOICE)
        runCurrent()
        h.host.dial(audio = true, video = false)
        runCurrent()
        val callId = h.host.active.value!!.session.callId
        h.host.deliver(
            groupA,
            1,
            FfiCallSignalingBody.End(callId, FfiCallEndReason.DECLINED),
            iso(t0),
        )
        runCurrent()

        assertEquals(
            listOf(groupA to CallTranscriptEntry(CallOutcome.DECLINED, video = false, durationSecs = null)),
            h.port.recorded,
        )
    }

    @Test
    fun `a video call the caller placed records itself as a video call`() = runTest {
        val h = harness()
        h.host.startOutgoing(groupA, RequestedCallKind.VIDEO)
        runCurrent()
        h.host.dial(audio = true, video = true)
        runCurrent()
        h.host.hangUp()
        runCurrent()

        assertEquals(
            listOf(groupA to CallTranscriptEntry(CallOutcome.OUTGOING, video = true, durationSecs = null)),
            h.port.recorded,
        )
    }

    @Test
    fun `a pre-join the user cancelled is not a call and writes nothing`() = runTest {
        val h = harness()
        h.host.startOutgoing(groupA, RequestedCallKind.VIDEO)
        runCurrent()
        h.host.cancelPrepared()
        runCurrent()

        assertTrue(h.port.recorded.isEmpty())
    }

    @Test
    fun `the phone button dials without any screen in between`() = runTest {
        // `docs/ux/calls.md` section 3.1 — "Tapping the phone button places the
        // call immediately. No preparation screen." The host is where that is
        // true or not: a voice call leaves `startOutgoing` already dialled, so
        // there is no undialled state for a screen to be drawn over.
        val h = harness()
        assertTrue(h.host.startOutgoing(groupA, RequestedCallKind.VOICE))
        runCurrent()
        val call = h.host.active.value!!
        assertEquals(RequestedCallKind.VOICE, call.requestedKind)
        assertEquals(true to false, call.session.dialed)
        assertNotEquals(CallSession.WebCallState.Idle, call.session.state.value)
    }

    @Test
    fun `the video button dials at once, carrying the camera`() = runTest {
        // Was `the video button stops at the camera check` until 2026-09-21.
        // docs/ux/calls.md section 3.2: one press of a call button is one
        // call, for both kinds; the self-view the check existed for is on the
        // calling screen already.
        val h = harness()
        assertTrue(h.host.startOutgoing(groupA, RequestedCallKind.VIDEO, hasCamera = true))
        runCurrent()
        val call = h.host.active.value!!
        assertEquals(RequestedCallKind.VIDEO, call.requestedKind)
        assertEquals("dialled at once, with the camera", true to true, call.session.dialed)
    }

    @Test
    fun `the video button on a camera-less device places a voice call`() = runTest {
        // Section 3.2: "If the device has no camera, the video button starts a
        // voice call and says so in one line." A check with nothing in it is
        // the empty preview box all over again.
        val h = harness()
        assertTrue(h.host.startOutgoing(groupA, RequestedCallKind.VIDEO, hasCamera = false))
        runCurrent()
        val call = h.host.active.value!!
        assertEquals(true to false, call.session.dialed)
    }

    // ---- The ring does not depend on which screen is open (section 2.1) ------

    @Test
    fun `an offer for a group with no chat screen open raises the ring`() = runTest {
        // The live defect: the call rang only while its own chat was the visible
        // screen. Nothing in this host knows about screens — an offer is
        // delivered by whichever poll read it — so what has to hold is that a
        // delivery carrying no screen context at all still rings the PHONE
        // (the notification), not merely the in-app banner.
        val h = harness()
        h.host.deliver(groupB, messageId = 1, body = offer(call1), createdAt = iso(t0))
        runCurrent()

        assertEquals(1, h.ringer.started.size)
        val ring = h.ringer.started.single()
        assertEquals(groupB, ring.groupIdHex)
        assertEquals(callIdHexOf(call1), ring.callIdHex)
        assertEquals(CallSession.WebCallState.Ringing, h.session()?.state?.value)
    }

    @Test
    fun `an offer still rings when it is read a whole sweep after it was published`() = runTest {
        // The mechanism behind the defect. The app-level loop sweeps every group
        // in waves, so an offer is read up to one sweep after the DS stamped it;
        // what matters is whether that delay fits inside the window. Driven with
        // the sweep the CURRENT policy produces for an account the size of the
        // repro's, so a policy change that widened the sweep past the window
        // would fail here rather than in someone's hand.
        val h = harness()
        val sweepSecs = me.lakko.app.chat.appPollSweepSecs(
            groupCount = 7,
            concurrency = me.lakko.app.chat.POLL_ALL_CONCURRENCY,
            maxWaitSecs = 30,
        )
        val readAt = t0
        val publishedAt = readAt - sweepSecs * 1_000L
        h.now = readAt
        h.host.deliver(groupB, messageId = 1, body = offer(call1), createdAt = iso(publishedAt))
        runCurrent()

        assertEquals(1, h.ringer.started.size)
        assertEquals(CallSession.WebCallState.Ringing, h.session()?.state?.value)
        // And it rings with time left to answer, not in the last instant. The
        // bound is the policy's own: a sweep never exceeds its budget, so what
        // is left is at least the window minus that budget. Derived rather than
        // picked, so widening the budget fails here.
        val remaining = callRingRemainingMs(publishedAt, readAt, TEST_RING_WINDOW_MS)
        val guaranteed = TEST_RING_WINDOW_MS - me.lakko.app.chat.APP_POLL_SWEEP_BUDGET_SECS * 1_000L
        assertTrue("only ${remaining}ms of ring window left", remaining >= guaranteed)
        assertTrue("the guarantee is worth having", guaranteed >= 20_000L)
    }

    @Test
    fun `the sweep the old fixed wait produced would never have rung at all`() = runTest {
        // Seven chats, three waves, thirty seconds each: the offer is ninety
        // seconds old by the time it is read, past the stale cut-off, and the
        // host deliberately refuses to ring for it. This is the test that
        // distinguishes the fix from the bug, and it must keep failing to ring.
        val h = harness()
        val legacySweepSecs = 3 * 30
        h.now = t0
        h.host.deliver(
            groupB,
            messageId = 1,
            body = offer(call1),
            createdAt = iso(t0 - legacySweepSecs * 1_000L),
        )
        runCurrent()

        assertTrue(legacySweepSecs * 1_000L > STALE_OFFER_MS)
        assertTrue("a stale offer must not ring", h.ringer.started.isEmpty())
        assertNull(h.host.active.value)
    }

    @Test
    fun `one call writes exactly one row`() = runTest {
        val h = harness()
        h.host.deliver(groupA, 1, offer(call1), iso(t0))
        runCurrent()
        h.host.decline()
        runCurrent()
        // Every later way of observing the same finished call is idempotent.
        h.host.deliver(groupA, 2, end(call1), iso(t0))
        h.host.hangUp()
        h.host.clearFinished()
        runCurrent()

        assertEquals(1, h.port.recorded.size)
    }

    // ---- Putting a call away (docs/ux/calls.md section 4.5) -------------------

    /** A call is born expanded: nothing is put away until somebody puts it away. */
    @Test
    fun `a 1x1 call starts with its screen up, not put away`() = runTest {
        val h = harness()
        h.host.startOutgoing(groupA, RequestedCallKind.VOICE)
        runCurrent()
        assertNotNull(h.host.active.value)
        assertFalse(h.host.collapsed.value)
    }

    /**
     * Section 4.5: collapsing "changes nothing about the call itself — the
     * media, the keys and the timer are untouched, only the picture is put
     * away". Above all it must not hang up: back has never meant hang up
     * anywhere else in the app.
     */
    @Test
    fun `collapsing a 1x1 call never ends it`() = runTest {
        val h = harness()
        h.host.startOutgoing(groupA, RequestedCallKind.VOICE)
        runCurrent()
        val call = h.host.active.value!!

        h.host.collapse()
        runCurrent()

        assertTrue(h.host.collapsed.value)
        assertTrue("the call was hung up", call.session.hangUps.isEmpty())
        assertSame("the call left the slot", call, h.host.active.value)
        assertTrue(h.host.hasCall)

        h.host.expand()
        assertFalse(h.host.collapsed.value)
    }

    /**
     * Section 4.5: "a person who put the call away asked not to be interrupted
     * by it." A collapsed call that ends must not un-collapse, or its
     * `Call ended` screen lands over whatever the person went off to read.
     */
    @Test
    fun `a collapsed 1x1 call that ends does not throw its screen back up`() = runTest {
        val h = harness()
        h.host.startOutgoing(groupA, RequestedCallKind.VOICE)
        runCurrent()
        val call = h.host.active.value!!
        h.host.collapse()
        runCurrent()

        call.session.mutableState.value = CallSession.WebCallState.Done
        runCurrent()

        assertTrue("the ended screen was thrown up over another screen", h.host.collapsed.value)
    }

    /**
     * ...and the NEXT call still shows its own screen, which is the other half
     * of the same rule. Clearing the flag where a call takes the slot rather
     * than where one ends is what makes both hold at once.
     */
    @Test
    fun `the 1x1 call after a collapsed one shows its own screen`() = runTest {
        val h = harness()
        h.host.startOutgoing(groupA, RequestedCallKind.VOICE)
        runCurrent()
        val first = h.host.active.value!!
        h.host.collapse()
        first.session.mutableState.value = CallSession.WebCallState.Done
        runCurrent()
        h.host.clearFinished()
        runCurrent()
        assertTrue(h.host.collapsed.value)

        h.host.startOutgoing(groupB, RequestedCallKind.VOICE)
        runCurrent()
        assertNotNull(h.host.active.value)
        assertFalse("the new call inherited the last one's put-away", h.host.collapsed.value)
    }

    /**
     * The running time the bar shows is the call's, not the screen's: it is
     * latched once when media first flows and survives being put away and
     * brought back, where the screen's own timer restarts from zero.
     */
    @Test
    fun `the running time is latched at the connect and survives being put away`() = runTest {
        val h = harness()
        h.host.startOutgoing(groupA, RequestedCallKind.VOICE)
        runCurrent()
        assertNull("a call that has not connected has no running time", h.host.connectedSinceMs.value)
        val call = h.host.active.value!!

        call.session.mutableState.value = CallSession.WebCallState.Connected
        runCurrent()
        val connectedAt = h.host.connectedSinceMs.value
        assertEquals(h.now, connectedAt)

        h.host.collapse()
        h.host.expand()
        runCurrent()
        assertEquals("the clock restarted", connectedAt, h.host.connectedSinceMs.value)

        // A reconnect passes through Connected again and must not restart it.
        h.now += 60_000L
        call.session.mutableState.value = CallSession.WebCallState.Connecting
        runCurrent()
        call.session.mutableState.value = CallSession.WebCallState.Connected
        runCurrent()
        assertEquals("a reconnect restarted the clock", connectedAt, h.host.connectedSinceMs.value)
    }

    /** A new call does not inherit the previous call's clock. */
    @Test
    fun `the next call starts with no running time`() = runTest {
        val h = harness()
        h.host.startOutgoing(groupA, RequestedCallKind.VOICE)
        runCurrent()
        h.host.active.value!!.session.mutableState.value = CallSession.WebCallState.Connected
        runCurrent()
        assertNotNull(h.host.connectedSinceMs.value)

        h.host.active.value!!.session.mutableState.value = CallSession.WebCallState.Done
        runCurrent()
        h.host.clearFinished()
        runCurrent()

        h.host.startOutgoing(groupB, RequestedCallKind.VOICE)
        runCurrent()
        assertNull("the new call inherited the last one's clock", h.host.connectedSinceMs.value)
    }

    // ---- Collapsing (`docs/ux/calls.md` section 4.5) --------------------------

    /**
     * The half of "back collapses the call and never ends it" that does not
     * need the back key.
     *
     * The key itself is the platform's: the call screen is a `Dialog` with
     * `dismissOnBackPress = true` whose `onDismissRequest` is this very
     * [OneToOneCallHost.collapse], so what back does is exactly what this
     * asserts. What it must NOT do is the reason the defect was worth writing
     * down — the maintainer pressed back on a live call and the screen stayed up
     * with the timer running, and the obvious fix for that (let the dialog
     * dismiss) would have hung the call up instead, which "back has never meant
     * anywhere else in the app".
     *
     * So every part of the call is asserted untouched, not just the flag: the
     * call is still in the slot, its state has not moved, nothing was hung up,
     * and the clock it counts from is the same number.
     */
    @Test
    fun `collapsing a call puts it away and never ends it`() = runTest {
        val h = harness()
        h.host.startOutgoing(groupA, RequestedCallKind.VOICE)
        runCurrent()
        val call = h.host.active.value!!
        call.session.mutableState.value = CallSession.WebCallState.Connected
        runCurrent()
        val clock = h.host.connectedSinceMs.value
        assertNotNull("the call never reached a running clock to preserve", clock)
        assertFalse("a call is born collapsed", h.host.collapsed.value)

        h.host.collapse()
        runCurrent()

        assertTrue("the call was not put away", h.host.collapsed.value)
        assertSame("collapsing took the call out of the slot", call, h.host.active.value)
        assertEquals(
            "collapsing moved the call's state",
            CallSession.WebCallState.Connected,
            call.session.state.value,
        )
        assertTrue("collapsing hung the call up", call.session.hangUps.isEmpty())
        assertEquals("collapsing restarted the call's clock", clock, h.host.connectedSinceMs.value)
    }

    /** A tap on the bar, which is the same journey back. */
    @Test
    fun `expanding a collapsed call brings it back and never ends it`() = runTest {
        val h = harness()
        h.host.startOutgoing(groupA, RequestedCallKind.VOICE)
        runCurrent()
        val call = h.host.active.value!!
        call.session.mutableState.value = CallSession.WebCallState.Connected
        runCurrent()
        h.host.collapse()
        runCurrent()

        h.host.expand()
        runCurrent()

        assertFalse("the call did not come back", h.host.collapsed.value)
        assertSame(call, h.host.active.value)
        assertTrue("coming back hung the call up", call.session.hangUps.isEmpty())
    }
}
