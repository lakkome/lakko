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
    class FakeSession(override val callId: ByteArray) : OneToOneCallSession {
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
            mutableState.value = CallSession.WebCallState.Done
        }
    }

    class FakePort : OneToOneCallPort<FakeSession> {
        val created = mutableListOf<Pair<String, FakeSession>>()
        val published = mutableListOf<Pair<String, String>>()
        var keyDelayMs = 0L
        var keyAvailable = true

        override fun createSession(
            groupIdHex: String,
            callId: ByteArray,
            outbound: (ByteArray) -> Unit,
        ): FakeSession = FakeSession(callId).also { created += groupIdHex to it }

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

    private class Harness(val host: OneToOneCallHost<FakeSession>, val port: FakePort, var now: Long)

    private fun TestScope.harness(foreground: Boolean = true): Harness {
        val port = FakePort()
        lateinit var h: Harness
        val host = OneToOneCallHost<FakeSession>(
            scope = backgroundScope,
            clock = { h.now },
            warn = { _, _ -> },
        )
        h = Harness(host, port, t0)
        host.attach(port)
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

        advanceTimeBy(OneToOneCallTiming().ringTimeoutMs - 1)
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

        advanceTimeBy(OneToOneCallTiming().establishTimeoutMs)
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
        assertTrue(h.host.startOutgoing(groupA))
        runCurrent()
        val call = h.host.active.value!!
        assertFalse(call.incoming)
        assertNotNull(call.session.keySeed)
        assertEquals(CallSession.WebCallState.Idle, call.session.state.value)

        h.host.dial(audio = true, video = false)
        runCurrent()
        assertEquals(true to false, call.session.dialed)

        advanceTimeBy(OneToOneCallTiming().establishTimeoutMs + 1)
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
    fun `cancelling a prepared call clears the slot`() = runTest {
        val h = harness()
        h.host.startOutgoing(groupA)
        runCurrent()
        h.host.cancelPrepared()
        runCurrent()
        assertNull(h.host.active.value)
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
}
