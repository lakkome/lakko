// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.async
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withContext
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A call that is left while its start is still waiting (for the SFU socket,
 * the admission token, the TURN credential) must stop there: it is not
 * reported to the person as a failure, and what it does on its way out must
 * not land on the call that was started after it.
 *
 * [GroupCallHost] and [LinkCallHost] with the fakes of [HostedCallsTest], on a
 * test scheduler. No UniFFI `.so`, no WebRTC.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class AbandonedCallStartTest {

    private val groupA = "aa".repeat(16)
    private val groupB = "bb".repeat(16)

    private fun TestScope.groupHost(port: HostedCallsTest.FakeGroupPort): GroupCallHost {
        val host = GroupCallHost(
            scope = backgroundScope,
            otherOccupant = { null },
            onSlotChanged = {},
            clock = { 1_800_000_000_000L },
            warn = { _, _ -> },
        )
        host.attach(port)
        runCurrent()
        return host
    }

    // ---- Group call -------------------------------------------------------------

    @Test
    fun `End pressed while a group call connects is not reported as a failure`() = runTest {
        val port = HostedCallsTest.FakeGroupPort()
        val gate = CompletableDeferred<Unit>()
        port.connectGates += gate
        val host = groupHost(port)

        val outcome = async { host.start(groupA, video = false, joinCallIdHex = null) }
        runCurrent()
        assertTrue("the call screen is up while it connects", host.overlayVisible.value)

        host.leave()
        runCurrent()
        assertEquals(1, port.sessions.single().hangUps)
        assertNull(host.slot.value)

        // The connect comes back only now, after the person left.
        gate.complete(Unit)
        runCurrent()
        assertEquals(GroupCallStartOutcome.Left, outcome.await())
        assertFalse(host.occupiesSlot)
        // The starter still owes the group the End of the call it announced.
        assertEquals(listOf("GroupStart", "End"), port.kinds())
    }

    @Test
    fun `a connect that throws under the hang-up is not reported as a failure either`() = runTest {
        val port = HostedCallsTest.FakeGroupPort()
        val gate = CompletableDeferred<Unit>()
        port.connectGates += gate
        val host = groupHost(port)

        val outcome = async { host.start(groupA, video = false, joinCallIdHex = null) }
        runCurrent()
        host.leave()
        port.sessions.single().errorAfterGate = IllegalStateException("PeerConnection was closed")
        gate.complete(Unit)
        runCurrent()
        assertEquals(GroupCallStartOutcome.Left, outcome.await())
    }

    @Test
    fun `a session that failed while connecting is still a failure`() = runTest {
        val port = HostedCallsTest.FakeGroupPort()
        val gate = CompletableDeferred<Unit>()
        port.connectGates += gate
        val host = groupHost(port)

        val outcome = async { host.start(groupA, video = false, joinCallIdHex = null) }
        runCurrent()
        // The session fails by itself: the call screen goes to ended, and the
        // host's observer takes the call out of the slot as an error.
        val session = port.sessions.single()
        session.failedFlag = true
        host.ui.setPhase(CallUiState.CallPhase.Ended(0L))
        runCurrent()
        gate.complete(Unit)
        runCurrent()
        assertTrue(outcome.await() is GroupCallStartOutcome.Failed)
    }

    @Test
    fun `a start that was left does not end the call started after it`() = runTest {
        val port = HostedCallsTest.FakeGroupPort()
        val firstGate = CompletableDeferred<Unit>()
        port.connectGates += firstGate
        val host = groupHost(port)

        val first = async { host.start(groupA, video = false, joinCallIdHex = null) }
        runCurrent()
        host.leave()
        runCurrent()

        // The next call, in another chat, connects at once.
        assertEquals(GroupCallStartOutcome.Connected, host.start(groupB, video = false, joinCallIdHex = null))
        runCurrent()
        assertEquals(groupB, host.slot.value?.groupIdHex)

        // Only now does the first call's connect come back, failing.
        port.sessions.first().errorAfterGate = IllegalStateException("sfu gone")
        firstGate.complete(Unit)
        runCurrent()

        assertEquals(GroupCallStartOutcome.Left, first.await())
        assertEquals("the second call is still in the slot", groupB, host.slot.value?.groupIdHex)
        assertTrue(host.occupiesSlot)
        assertEquals("the second call's session was not hung up", 0, port.sessions.last().hangUps)
        assertEquals(CallUiState.CallPhase.Connected, host.ui.phase.value)
    }

    // ---- Link call --------------------------------------------------------------

    /** A port whose start waits at [gate] and, like a real session, writes the
     *  ended phase to the call screen's state when it is torn down. */
    private class GatedLinkPort : LinkCallPort {
        val gates = ArrayDeque<CompletableDeferred<Unit>>()
        val handles = mutableListOf<HostedCallsTest.FakeLinkHandle>()
        var starts = 0

        /** The first start ignores cancellation and then fails. */
        var firstStartFailsUncancellably = false

        override fun sfuBaseUrl(): String = "wss://sfu.test"
        override fun callingAvailable(): Boolean = true

        override suspend fun start(
            request: LinkCallRequest,
            sfuBaseUrl: String,
            ui: CallUiState,
            events: LinkCallEvents,
        ): LinkCallHandle {
            starts += 1
            val gate = gates.removeFirstOrNull()
            if (gate != null) {
                if (firstStartFailsUncancellably && starts == 1) {
                    withContext(NonCancellable) { gate.await() }
                    throw IllegalStateException("the signaling client was closed")
                }
                try {
                    gate.await()
                } finally {
                    // What a session writes when it is torn down.
                    if (!gate.isCompleted) ui.setPhase(CallUiState.CallPhase.Ended(0L))
                }
            }
            if (request.isOwner) events.onJoined()
            return HostedCallsTest.FakeLinkHandle().also { handles += it }
        }

        override suspend fun mintInviteLink(callId: ByteArray, identity: CallInviteIdentity): String =
            "https://lakko.me/c/x#y"
    }

    private fun TestScope.linkHost(port: LinkCallPort): LinkCallHost {
        val host = LinkCallHost(
            scope = backgroundScope,
            otherOccupant = { null },
            onSlotChanged = {},
            warn = { _, _ -> },
        )
        host.attach(port)
        runCurrent()
        return host
    }

    private fun request(callByte: Int) = LinkCallRequest(
        callId = ByteArray(16) { callByte.toByte() },
        ownerAccountId = ByteArray(16),
        ownerSignatureKey = ByteArray(32),
        mediaVideo = false,
        isOwner = true,
        slug = null,
        accountIdentitySeed = ByteArray(64),
        sessionToken = "token",
        accountId = ByteArray(16),
    )

    @Test
    fun `leaving a link call that is still starting stops the start and reports nothing`() = runTest {
        val port = GatedLinkPort()
        port.gates += CompletableDeferred()
        val host = linkHost(port)

        val outcome = async { host.start(request(0x11)) }
        runCurrent()
        assertEquals(LinkCallPhase.STARTING, host.slot.value?.phase)

        host.leave()
        runCurrent()
        assertEquals(LinkCallStartOutcome.Started, outcome.await())
        assertNull(host.slot.value)
        assertTrue("the start was stopped before it produced a session", port.handles.isEmpty())
    }

    @Test
    fun `an abandoned link start that fails late is not reported as a failure`() = runTest {
        val port = GatedLinkPort()
        val gate = CompletableDeferred<Unit>()
        port.gates += gate
        port.firstStartFailsUncancellably = true
        val host = linkHost(port)

        val outcome = async { host.start(request(0x11)) }
        runCurrent()
        host.leave()
        gate.complete(Unit)
        runCurrent()
        assertEquals(LinkCallStartOutcome.Started, outcome.await())
        assertNull("no error is left in the slot", host.slot.value)
    }

    @Test
    fun `what an abandoned link start writes on its way out does not reach the next call`() = runTest {
        val port = GatedLinkPort()
        port.gates += CompletableDeferred()
        val host = linkHost(port)

        val first = async { host.start(request(0x11)) }
        runCurrent()
        // Left and started again in the same breath, before the first start's
        // teardown has run.
        host.leave()
        val second = async { host.start(request(0x22)) }
        runCurrent()

        assertEquals(LinkCallStartOutcome.Started, first.await())
        assertEquals(LinkCallStartOutcome.Started, second.await())
        assertEquals(LinkCallPhase.LIVE, host.slot.value?.phase)
        assertEquals(callIdHexOf(ByteArray(16) { 0x22 }), host.slot.value?.callIdHex)
        // The first start's ended phase was written before the second call
        // took the call screen's state, not over it.
        assertFalse(host.ui.phase.value is CallUiState.CallPhase.Ended)
        assertEquals(0, port.handles.single().hangUps)
    }
}
