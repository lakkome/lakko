// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import me.lakko.app.calls.CallSession
import me.lakko.app.calls.IncomingCallRing
import me.lakko.app.calls.IncomingCallRingPort
import me.lakko.app.calls.OneToOneCallHost
import me.lakko.app.calls.OneToOneCallKey
import me.lakko.app.calls.OneToOneCallPort
import me.lakko.app.calls.OneToOneCallSession
import uniffi.lakko_ffi_uniffi.FfiCallEndReason
import uniffi.lakko_ffi_uniffi.FfiCallMediaFlags
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The own-echo filter, and the live defect it caused: an incoming call rang only
 * while its own chat was the visible screen.
 *
 * ## What was actually dropping the call
 *
 * `ChatClient` kept the rows this device published in a bare `Set<Long>` of DS
 * message ids, with no group in the key, and `applyFetchedMessages` consulted it
 * for every row of every group. A DS id is assigned **per journal**, and there
 * is one journal per group, so id 7 exists independently in every chat.
 * Publishing anything in chat A therefore armed a trap for the next row numbered
 * the same in chat B: the poll dropped it as "our own echo" and advanced the
 * cursor past it, so it was gone for good.
 *
 * ## Why it looked like a screen dependence
 *
 * It is a function of WHO applies the rows. A chat that is open has its own
 * uninterrupted poll and consumes its own echoes within a second. Every other
 * chat is applied by the app-level all-groups poll, where every group's rows run
 * through the one shared filter — so an offer for a chat the user was not
 * looking at was swallowed by an id left behind by a message they had sent
 * somewhere else. The call was fetched and dropped, not fetched late.
 *
 * These tests drive the all-groups shape — several groups sharing one filter —
 * rather than the single-group chat-screen shape, which is exactly the
 * difference that hid the bug.
 *
 * ## Why the existing coverage did not catch it
 *
 * [OneToOneCallInboundRoutingTest] already pins that a parsed body reaches the
 * call host "with no chat view in existence", and it passed throughout. It
 * enters at `routeParsedCallSignaling`, which is BELOW the own-echo filter — by
 * the time a row reaches it the drop has already happened. The gap was the one
 * step above, where a row is either kept or silently discarded.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class OwnEchoRegistryTest {

    private val groupA = "aa".repeat(16)
    private val groupB = "bb".repeat(16)
    private val groupC = "cc".repeat(16)

    @Test
    fun `an id published in one chat never silences another chat's row`() {
        // THE defect, in one assertion. Every journal starts at 1 and the ids
        // stay small, so across a handful of chats this collision is the common
        // case rather than a corner one.
        val echoes = OwnEchoRegistry()
        echoes.remember(groupA, 7L)

        assertFalse("group B's row 7 is not our echo", echoes.consume(groupB, 7L))
        assertFalse("nor is group C's", echoes.consume(groupC, 7L))
        assertTrue("group A's row 7 still is", echoes.consume(groupA, 7L))
    }

    @Test
    fun `our own echo is still suppressed, once`() {
        val echoes = OwnEchoRegistry()
        echoes.remember(groupA, 42L)

        assertTrue(echoes.consume(groupA, 42L))
        // A published row comes back exactly once; a second match would mean a
        // later, unrelated row of the same number.
        assertFalse(echoes.consume(groupA, 42L))
        assertEquals(0, echoes.size)
    }

    @Test
    fun `the all-groups poll shape does not consume its way through a chat`() {
        // The app-level loop applies every group through one filter. Register a
        // send in each of three chats, then walk the ids every chat carries.
        val echoes = OwnEchoRegistry()
        val chats = listOf(groupA, groupB, groupC)
        chats.forEach { echoes.remember(it, 3L) }

        // Each chat suppresses only its own row 3 ...
        chats.forEach { assertTrue(it, echoes.consume(it, 3L)) }
        assertEquals(0, echoes.size)
        // ... and nothing is left to swallow anyone's next rows.
        for (chat in chats) {
            for (id in 1L..10L) {
                assertFalse("$chat row $id", echoes.consume(chat, id))
            }
        }
    }

    @Test
    fun `duplicate registration is harmless and clear starts over`() {
        val echoes = OwnEchoRegistry()
        echoes.remember(groupA, 1L)
        echoes.remember(groupA, 1L)
        assertEquals(1, echoes.size)

        echoes.remember(groupB, 2L)
        echoes.clear()
        assertFalse(echoes.consume(groupA, 1L))
        assertFalse(echoes.consume(groupB, 2L))
    }

    /**
     * The whole chain the live defect ran through, with the filter in the middle:
     * a row arrives on a chat with no screen open, survives the own-echo filter,
     * and reaches the call host, which rings.
     *
     * The filter is driven directly rather than through `applyFetchedMessages`
     * because that needs a live FFI `Group` and the native library; what this
     * adds over the unit cases above is that the SURVIVING row is then handed to
     * the real host and really does ring.
     */
    @Test
    fun `an offer on a chat with no screen open survives the filter and rings`() = runTest {
        val echoes = OwnEchoRegistry()
        // The user sent a message in the chat they had open. Its id is 7 on
        // THAT journal.
        echoes.remember(groupA, 7L)

        val ringer = RecordingRinger()
        val host = OneToOneCallHost<FakeSession>(
            scope = backgroundScope,
            // A fixed clock, so the offer below is exactly as old as the test
            // says rather than as old as the wall clock happens to make it.
            clock = { NOW_MS },
            warn = { _, _ -> },
        )
        host.attach(FakePort())
        host.attachRinger(ringer)
        host.setForeground(true)
        runCurrent()

        // The all-groups poll now applies group B, whose journal also reaches
        // row 7 — and that row is the incoming call.
        val offerRowId = 7L
        val suppressed = echoes.consume(groupB, offerRowId)
        assertFalse("the offer must not be taken for our own echo", suppressed)

        // Surviving the filter is what lets it reach the host at all.
        if (!suppressed) {
            host.deliver(
                groupIdHex = groupB,
                messageId = offerRowId,
                body = offerBody(),
                createdAt = java.time.Instant.ofEpochMilli(NOW_MS).toString(),
            )
        }
        runCurrent()

        assertEquals("the phone rings", 1, ringer.started.size)
        assertEquals(groupB, ringer.started.single().groupIdHex)
        assertNotNull(host.active.value)
        assertEquals(CallSession.WebCallState.Ringing, host.active.value?.session?.state?.value)
    }

    @Test
    fun `the old id-only filter would have swallowed that same offer`() {
        // The regression this closes, stated as the behaviour it replaces: with
        // ids alone, group A's row 7 matches group B's row 7 and the call is
        // dropped before anything can ring for it.
        val idsOnly = mutableSetOf(7L)
        assertTrue("the old filter ate another chat's row", idsOnly.remove(7L))
    }

    private class RecordingRinger : IncomingCallRingPort {
        val started = mutableListOf<IncomingCallRing>()

        override fun startRinging(ring: IncomingCallRing) {
            started += ring
        }

        override fun stopRinging(callIdHex: String) = Unit
    }

    /** The parts of a call session the host drives; no WebRTC, no native library. */
    private class FakeSession(override val callId: ByteArray) : OneToOneCallSession {
        val mutableState = MutableStateFlow(CallSession.WebCallState.Idle)
        override val state: StateFlow<CallSession.WebCallState> = mutableState

        override fun setCallKey(seed: ByteArray, localLeaf: Int) = Unit

        override fun applyIncomingSignaling(body: FfiCallSignalingBody) {
            if (body is FfiCallSignalingBody.Offer &&
                mutableState.value == CallSession.WebCallState.Idle
            ) {
                mutableState.value = CallSession.WebCallState.Ringing
            }
        }

        override fun dial(audio: Boolean, video: Boolean) = Unit

        override fun acceptCall(withVideo: Boolean) = Unit

        override fun declineCall() = Unit

        override fun hangUp(reason: CallSession.WebCallEndReason) {
            mutableState.value = CallSession.WebCallState.Done
        }
    }

    private class FakePort : OneToOneCallPort<FakeSession> {
        override fun createSession(
            groupIdHex: String,
            callId: ByteArray,
            outbound: (ByteArray) -> Unit,
        ): FakeSession = FakeSession(callId)

        override suspend fun deriveCallKey(groupIdHex: String, callId: ByteArray): OneToOneCallKey =
            OneToOneCallKey(ByteArray(32), 0)

        override suspend fun publish(groupIdHex: String, bytes: ByteArray) = Unit

        override fun composeEnd(callId: ByteArray, reason: FfiCallEndReason): ByteArray? = null
    }

    private fun offerBody(): FfiCallSignalingBody =
        FfiCallSignalingBody.Offer(
            callId = ByteArray(16) { 0x11 },
            sdp = "v=0",
            createdAtMs = NOW_MS.toULong(),
            media = FfiCallMediaFlags(audio = true, video = false),
        )

    private companion object {
        const val NOW_MS: Long = 1_800_000_000_000L
    }
}
