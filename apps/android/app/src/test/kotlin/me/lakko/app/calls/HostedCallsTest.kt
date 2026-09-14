// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiCallEndReason
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody

/**
 * The app-level group-call and call-link hosts ([GroupCallHost],
 * [LinkCallHost]) driven with fake ports and sessions on a test scheduler: one
 * call slot, the leave plan, the settled-epoch rekey, and the link-call phases.
 * No UniFFI `.so`, no WebRTC.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class HostedCallsTest {

    private val groupA = "aa".repeat(16)
    private val groupB = "bb".repeat(16)
    private val callHex = "11".repeat(16)

    // ---- Group-call fakes -----------------------------------------------------

    class FakeGroupSession(private val ui: CallUiState, private val connectError: Throwable?) :
        GroupCallSessionHandle {
        var failedFlag = false
        override val failed: Boolean get() = failedFlag
        val rekeys = mutableListOf<Byte>()
        var hangUps = 0
        val mic = mutableListOf<Boolean>()

        override suspend fun connect() {
            connectError?.let { throw it }
            ui.setPhase(CallUiState.CallPhase.Connected)
        }

        override fun hangUp() {
            hangUps += 1
            ui.setPhase(CallUiState.CallPhase.Ended(0L))
        }

        override fun rekey(seed: ByteArray) {
            rekeys += seed[0]
        }

        override suspend fun publishVideo() = Unit

        override fun setCameraEnabled(on: Boolean) = Unit

        override fun setMicrophoneEnabled(on: Boolean) {
            mic += on
        }
    }

    class FakeGroupPort : GroupCallPort {
        var active: Boolean? = true
        var live = true
        var joinedLeaves: Set<UInt>? = emptySet()
        var read = read(epoch = 5u, seedByte = 5)
        var connectError: Throwable? = null
        val published = mutableListOf<Pair<String, FfiCallSignalingBody>>()
        val sessions = mutableListOf<FakeGroupSession>()
        var synced = 0

        override fun sfuBaseUrl(): String = "wss://sfu.test"
        override fun sessionToken(): String = "token"
        override fun isGroupActive(groupIdHex: String): Boolean? = active
        override suspend fun syncGroup(groupIdHex: String) {
            synced += 1
        }
        override fun isCallLive(groupIdHex: String, callIdHex: String, nowMs: Long): Boolean = live
        override fun recordJoinedLeaves(groupIdHex: String, callIdHex: String): Set<UInt>? = joinedLeaves
        override suspend fun readSettledGroup(groupIdHex: String, callId: ByteArray): SettledGroupRead = read
        override suspend fun mintCallJwt(sessionToken: String, callIdHex: String): String = "jwt"
        override suspend fun publishSignal(groupIdHex: String, body: FfiCallSignalingBody): Boolean {
            published += groupIdHex to body
            return true
        }
        override fun createSession(request: GroupCallSessionRequest, ui: CallUiState): GroupCallSessionHandle =
            FakeGroupSession(ui, connectError).also { sessions += it }

        fun kinds(): List<String> = published.map { it.second::class.simpleName ?: "?" }

        companion object {
            fun read(
                epoch: ULong?,
                seedByte: Int,
                settled: Boolean = true,
                isActive: Boolean? = true,
            ) = SettledGroupRead(
                joined = true,
                isActive = isActive,
                settled = settled,
                epoch = epoch,
                seed = ByteArray(32) { seedByte.toByte() },
                localLeaf = 2,
            )
        }
    }

    private class GroupHarness(
        val host: GroupCallHost,
        val port: FakeGroupPort,
        val slotReports: MutableList<Boolean>,
        var other: CallKind?,
    )

    private fun TestScope.groupHarness(): GroupHarness {
        val port = FakeGroupPort()
        val reports = mutableListOf<Boolean>()
        lateinit var h: GroupHarness
        val host = GroupCallHost(
            scope = backgroundScope,
            otherOccupant = { h.other },
            onSlotChanged = { reports += it },
            clock = { 1_800_000_000_000L },
            warn = { _, _ -> },
        )
        host.attach(port)
        h = GroupHarness(host, port, reports, null)
        runCurrent()
        return h
    }

    // ---- Group call ---------------------------------------------------------------

    @Test
    fun `a group call is refused while another call holds the slot`() = runTest {
        val h = groupHarness()
        h.other = CallKind.LINK
        val outcome = h.host.start(groupA, video = false, joinCallIdHex = null)
        assertEquals(GroupCallStartOutcome.Busy(callBusyMessage(CallKind.LINK, CallVerb.START)), outcome)
        assertNull(h.host.slot.value)
        assertTrue(h.port.sessions.isEmpty())
        assertTrue(h.port.published.isEmpty())
    }

    @Test
    fun `starting a group call takes the slot, announces it and joins once connected`() = runTest {
        val h = groupHarness()
        val outcome = h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        assertEquals(GroupCallStartOutcome.Connected, outcome)
        val slot = h.host.slot.value
        assertNotNull(slot)
        assertEquals(groupA, slot!!.groupIdHex)
        assertFalse(slot.starting)
        assertTrue(h.host.occupiesSlot)
        assertTrue(h.host.overlayVisible.value)
        assertTrue("the 1x1 host was told", h.slotReports.last())
        assertEquals(listOf("GroupStart", "GroupJoin"), h.port.kinds())
    }

    @Test
    fun `returning to the live call's chat is not a second call`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        assertEquals(GroupCallStartOutcome.AlreadyInThisCall, h.host.start(groupA, false, null))
        assertEquals(
            GroupCallStartOutcome.Busy(callBusyMessage(CallKind.GROUP, CallVerb.START)),
            h.host.start(groupB, false, null),
        )
        assertEquals(1, h.port.sessions.size)
    }

    @Test
    fun `a settled epoch advance rotates the call key once`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        val session = h.port.sessions.single()
        assertEquals("the catch-up after connect found the key current", emptyList<Byte>(), session.rekeys)

        h.port.read = FakeGroupPort.read(epoch = 6u, seedByte = 6)
        h.host.onGroupSettled(groupA)
        runCurrent()
        assertEquals(listOf<Byte>(6), session.rekeys)

        h.host.onGroupSettled(groupA)
        runCurrent()
        assertEquals("an unchanged epoch is not a rekey", listOf<Byte>(6), session.rekeys)
    }

    @Test
    fun `an unsettled read or another group's drain keeps the current key`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        val session = h.port.sessions.single()

        h.port.read = FakeGroupPort.read(epoch = 7u, seedByte = 7, settled = false)
        h.host.onGroupSettled(groupA)
        h.host.onGroupSettled(groupB)
        runCurrent()
        assertEquals(emptyList<Byte>(), session.rekeys)

        h.port.read = FakeGroupPort.read(epoch = 7u, seedByte = 7, settled = true)
        h.host.onGroupSettled(groupA)
        runCurrent()
        assertEquals(listOf<Byte>(7), session.rekeys)
    }

    @Test
    fun `a group this device left ends the call locally and publishes nothing`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        val publishedBefore = h.port.published.size
        h.port.read = FakeGroupPort.read(epoch = 6u, seedByte = 6, isActive = false)
        h.host.onGroupSettled(groupA)
        runCurrent()
        assertEquals(1, h.port.sessions.single().hangUps)
        assertNull(h.host.slot.value)
        assertFalse(h.slotReports.last())
        assertEquals(publishedBefore, h.port.published.size)
    }

    @Test
    fun `hanging up the last participant publishes Leave then End`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        h.port.joinedLeaves = setOf(2u)
        h.host.leave()
        runCurrent()
        assertEquals(listOf("GroupStart", "GroupJoin", "GroupLeave", "End"), h.port.kinds())
        val end = h.port.published.last().second as FfiCallSignalingBody.End
        assertEquals(FfiCallEndReason.HUNG_UP, end.reason)
        assertNull(h.host.slot.value)
        assertTrue("the ended state stays on screen until the screen exits", h.host.overlayVisible.value)
        h.host.onOverlayExit()
        assertFalse(h.host.overlayVisible.value)
    }

    @Test
    fun `hanging up while others are still in the call publishes only the Leave`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        h.port.joinedLeaves = setOf(2u, 3u)
        h.host.leave()
        runCurrent()
        assertEquals(listOf("GroupStart", "GroupJoin", "GroupLeave"), h.port.kinds())
    }

    @Test
    fun `joining a call that already ended does not start one`() = runTest {
        val h = groupHarness()
        h.port.live = false
        val outcome = h.host.start(groupA, video = false, joinCallIdHex = callHex)
        runCurrent()
        assertEquals(GroupCallStartOutcome.Ended, outcome)
        assertEquals(1, h.port.synced)
        assertTrue(h.port.sessions.isEmpty())
        assertNull(h.host.slot.value)
        assertFalse(h.host.occupiesSlot)
        assertFalse(h.slotReports.last())
    }

    @Test
    fun `no call key means no call`() = runTest {
        val h = groupHarness()
        h.port.read = SettledGroupRead(true, true, true, 5u, seed = null, localLeaf = null)
        val outcome = h.host.start(groupA, video = false, joinCallIdHex = null)
        assertTrue(outcome is GroupCallStartOutcome.Failed)
        assertTrue(h.port.sessions.isEmpty())
        assertTrue(h.port.published.isEmpty())
        assertNull(h.host.slot.value)
    }

    @Test
    fun `a failed connect frees the slot and ends the call it announced`() = runTest {
        val h = groupHarness()
        h.port.connectError = IllegalStateException("sfu down")
        val outcome = h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        assertTrue(outcome is GroupCallStartOutcome.Failed)
        assertNull(h.host.slot.value)
        assertFalse(h.host.occupiesSlot)
        assertEquals(listOf("GroupStart", "End"), h.port.kinds())
        assertEquals(FfiCallEndReason.ERROR, (h.port.published.last().second as FfiCallSignalingBody.End).reason)
    }

    @Test
    fun `the microphone button reaches the hosted session`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        h.host.ui.setParticipants(
            listOf(
                CallUiState.Participant(
                    id = "local",
                    displayName = "You",
                    isLocal = true,
                    micOn = true,
                    camOn = false,
                    handRaised = false,
                    quality = CallUiState.QualityTier.Good,
                    speaking = false,
                    screenSharing = false,
                ),
            ),
        )
        runCurrent()
        h.host.ui.setMic(false)
        runCurrent()
        assertEquals(false, h.port.sessions.single().mic.last())
    }

    // ---- Link-call fakes -------------------------------------------------------------

    class FakeLinkHandle : LinkCallHandle {
        var hangUps = 0
        val mic = mutableListOf<Boolean>()
        override fun hangUp() {
            hangUps += 1
        }
        override suspend fun publishVideo() = Unit
        override fun setCameraEnabled(on: Boolean) = Unit
        override fun setMicrophoneEnabled(on: Boolean) {
            mic += on
        }
    }

    class FakeLinkPort : LinkCallPort {
        var calling = true
        var startError: Throwable? = null
        var events: LinkCallEvents? = null
        val handles = mutableListOf<FakeLinkHandle>()

        override fun sfuBaseUrl(): String = "wss://sfu.test"
        override fun callingAvailable(): Boolean = calling
        override suspend fun start(
            request: LinkCallRequest,
            sfuBaseUrl: String,
            ui: CallUiState,
            events: LinkCallEvents,
        ): LinkCallHandle {
            this.events = events
            startError?.let { throw it }
            // The owner founds the group inside `host()`.
            if (request.isOwner) events.onJoined()
            return FakeLinkHandle().also { handles += it }
        }
    }

    private class LinkHarness(
        val host: LinkCallHost,
        val port: FakeLinkPort,
        val slotReports: MutableList<Boolean>,
        var other: CallKind?,
    )

    private fun TestScope.linkHarness(): LinkHarness {
        val port = FakeLinkPort()
        val reports = mutableListOf<Boolean>()
        lateinit var h: LinkHarness
        val host = LinkCallHost(
            scope = backgroundScope,
            otherOccupant = { h.other },
            onSlotChanged = { reports += it },
            warn = { _, _ -> },
        )
        host.attach(port)
        h = LinkHarness(host, port, reports, null)
        runCurrent()
        return h
    }

    private fun request(owner: Boolean, callByte: Int = 0x11) = LinkCallRequest(
        callId = ByteArray(16) { callByte.toByte() },
        ownerAccountId = ByteArray(16),
        ownerSignatureKey = ByteArray(32),
        mediaVideo = false,
        isOwner = owner,
        slug = "slug-$callByte",
        accountIdentitySeed = ByteArray(64),
        sessionToken = "token",
    )

    // ---- Link call ------------------------------------------------------------------

    @Test
    fun `an owner's link call is live from founding and holds the slot`() = runTest {
        val h = linkHarness()
        assertEquals(LinkCallStartOutcome.Started, h.host.start(request(owner = true)))
        assertEquals(LinkCallPhase.LIVE, h.host.slot.value?.phase)
        assertTrue(h.host.occupiesSlot)
        assertTrue(h.host.overlayVisible.value)
        assertTrue(h.slotReports.last())
    }

    @Test
    fun `a joiner waits, then goes live on its Welcome`() = runTest {
        val h = linkHarness()
        h.host.start(request(owner = false))
        assertEquals(LinkCallPhase.WAITING, h.host.slot.value?.phase)
        assertFalse(h.host.overlayVisible.value)
        h.port.events!!.onJoined()
        assertEquals(LinkCallPhase.LIVE, h.host.slot.value?.phase)
        assertTrue(h.host.overlayVisible.value)
    }

    @Test
    fun `a link call is refused during a group call, and the same link is not started twice`() = runTest {
        val h = linkHarness()
        h.other = CallKind.GROUP
        assertEquals(
            LinkCallStartOutcome.Busy(callBusyMessage(CallKind.GROUP, CallVerb.JOIN)),
            h.host.start(request(owner = false)),
        )
        assertNull(h.host.slot.value)
        assertEquals(callBusyMessage(CallKind.GROUP, CallVerb.JOIN), h.host.busyMessage(CallVerb.JOIN, callHex))

        h.other = null
        h.host.start(request(owner = false))
        assertEquals(LinkCallStartOutcome.AlreadyInThisCall, h.host.start(request(owner = false)))
        assertEquals(
            LinkCallStartOutcome.Busy(callBusyMessage(CallKind.LINK, CallVerb.JOIN)),
            h.host.start(request(owner = false, callByte = 0x22)),
        )
        assertEquals(1, h.port.handles.size)
    }

    @Test
    fun `an admission timeout is reported as dead, frees the slot and releases the call`() = runTest {
        val h = linkHarness()
        h.host.start(request(owner = false))
        val events = h.port.events!!
        events.onEnded()
        events.onNoCommitter()
        runCurrent()
        assertEquals(LinkCallPhase.DEAD, h.host.slot.value?.phase)
        assertFalse(h.host.occupiesSlot)
        assertFalse(h.slotReports.last())
        assertEquals(1, h.port.handles.single().hangUps)
        h.host.dismissOutcome()
        assertNull(h.host.slot.value)
    }

    @Test
    fun `cancelling while waiting leaves the call and ignores its late reports`() = runTest {
        val h = linkHarness()
        h.host.start(request(owner = false))
        h.host.leave()
        assertNull(h.host.slot.value)
        assertEquals(1, h.port.handles.single().hangUps)
        h.port.events!!.onJoined()
        assertNull(h.host.slot.value)
        assertFalse(h.host.overlayVisible.value)
    }

    @Test
    fun `the ended screen's exit forgets an ordinary end, unless the landing is showing it`() = runTest {
        val h = linkHarness()
        h.host.start(request(owner = true))
        h.port.events!!.onEnded()
        h.host.onOverlayExit()
        assertNull(h.host.slot.value)
        assertFalse(h.host.overlayVisible.value)

        h.host.start(request(owner = true, callByte = 0x33))
        val release = h.host.claimLanding()
        h.port.events!!.onEnded()
        h.host.onOverlayExit()
        assertEquals(LinkCallPhase.ENDED, h.host.slot.value?.phase)
        release()
        assertEquals(0, h.host.landingClaims.value)
    }

    @Test
    fun `no SFrame shim or a failed start never takes the slot`() = runTest {
        val h = linkHarness()
        h.port.calling = false
        assertTrue(h.host.start(request(owner = true)) is LinkCallStartOutcome.Failed)
        assertNull(h.host.slot.value)

        h.port.calling = true
        h.port.startError = IllegalStateException("mint refused")
        val outcome = h.host.start(request(owner = false))
        assertEquals(LinkCallStartOutcome.Failed("mint refused"), outcome)
        assertEquals(LinkCallPhase.ERROR, h.host.slot.value?.phase)
        assertEquals("mint refused", h.host.slot.value?.errorMessage)
        assertFalse(h.host.occupiesSlot)
    }

    @Test
    fun `the microphone is not driven before the joiner is admitted`() = runTest {
        val h = linkHarness()
        h.host.start(request(owner = false))
        h.host.ui.setParticipants(
            listOf(
                CallUiState.Participant(
                    id = "local",
                    displayName = "You",
                    isLocal = true,
                    micOn = true,
                    camOn = false,
                    handRaised = false,
                    quality = CallUiState.QualityTier.Good,
                    speaking = false,
                    screenSharing = false,
                ),
            ),
        )
        runCurrent()
        h.host.ui.setMic(false)
        runCurrent()
        assertTrue(h.port.handles.single().mic.isEmpty())
    }
}
