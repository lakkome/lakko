// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
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
        override var lastInboundAtMs: Long = 0L
        override var callEndedByServer: Boolean = false
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
        var otherFreshMembers: Set<String>? = emptySet()
        var read = read(epoch = 5u, seedByte = 5)
        var connectError: Throwable? = null
        val published = mutableListOf<Pair<String, FfiCallSignalingBody>>()
        val sessions = mutableListOf<FakeGroupSession>()
        val rows = mutableListOf<Pair<String, CallTranscriptEntry>>()
        var synced = 0

        override fun recordCall(groupIdHex: String, entry: CallTranscriptEntry) {
            rows += groupIdHex to entry
        }

        override fun sfuBaseUrl(): String = "wss://sfu.test"
        override fun sessionToken(): String = "token"
        override fun isGroupActive(groupIdHex: String): Boolean? = active
        override suspend fun syncGroup(groupIdHex: String) {
            synced += 1
        }
        override fun isCallLive(groupIdHex: String, callIdHex: String, nowMs: Long): Boolean = live
        override fun recordOtherFreshMembers(groupIdHex: String, callIdHex: String, nowMs: Long): Set<String>? =
            otherFreshMembers
        override suspend fun readSettledGroup(groupIdHex: String, callId: ByteArray): SettledGroupRead = read
        override suspend fun mintCallJwt(sessionToken: String, callIdHex: String): String = "jwt"
        /** What the SFU's state route answers; `null` = unreachable. */
        var sfuCallStateBody: String? = null
        val probedUrls = mutableListOf<String>()
        override suspend fun fetchSfuCallState(url: String): String? {
            probedUrls += url
            return sfuCallStateBody
        }
        /** When set, a Join publish suspends here BEFORE it is recorded. */
        var joinGate: CompletableDeferred<Unit>? = null

        override suspend fun publishSignal(groupIdHex: String, body: FfiCallSignalingBody): Boolean {
            if (body is FfiCallSignalingBody.GroupJoin) joinGate?.await()
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

    // ---- Asking the SFU whether an offered call is still there (section 4.3) ---

    /** The SFU says the call ended: the record is closed for the chat with an
     *  End this device publishes, though it was never in the call. */
    @Test
    fun `a probe answered ended closes the record for the chat`() = runTest {
        val h = groupHarness()
        h.port.sfuCallStateBody = """{"state":"ended","peers":0}"""
        h.host.probeCallState(groupA, callHex)
        runCurrent()
        assertEquals(1, h.port.probedUrls.size)
        assertTrue(h.port.probedUrls.first().startsWith("https://sfu.test/v1/calls/$callHex?token="))
        val end = h.port.published.single().second as FfiCallSignalingBody.End
        assertEquals(FfiCallEndReason.HUNG_UP, end.reason)
    }

    /** Any other answer, or none, changes nothing. */
    @Test
    fun `a probe answered live or not at all changes nothing`() = runTest {
        val h = groupHarness()
        for (body in listOf("""{"state":"live","peers":1}""", """{"state":"closing"}""", """{"state":"unknown"}""", "<html>", null)) {
            h.port.sfuCallStateBody = body
            h.host.probeCallState(groupA, callHex)
            runCurrent()
        }
        assertEquals(5, h.port.probedUrls.size)
        assertTrue(h.port.published.isEmpty())
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
    fun `a joined device re-publishes its Join every heartbeat interval until it leaves`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        assertEquals(listOf("GroupStart", "GroupJoin"), h.port.kinds())

        advanceTimeBy(GROUP_CALL_HEARTBEAT_MS - 1)
        runCurrent()
        assertEquals("no heartbeat before the interval", 1, h.port.kinds().count { it == "GroupJoin" })

        advanceTimeBy(1)
        runCurrent()
        assertEquals(2, h.port.kinds().count { it == "GroupJoin" })
        advanceTimeBy(GROUP_CALL_HEARTBEAT_MS)
        runCurrent()
        assertEquals(3, h.port.kinds().count { it == "GroupJoin" })
        val heartbeat = h.port.published.last().second as FfiCallSignalingBody.GroupJoin
        assertEquals("the heartbeat is the same Join body", 2u, heartbeat.leafIndex)

        h.port.otherFreshMembers = setOf("other-member")
        h.host.leave()
        runCurrent()
        val afterLeave = h.port.published.size
        advanceTimeBy(5 * GROUP_CALL_HEARTBEAT_MS)
        runCurrent()
        assertEquals("the heartbeat stops with the call", afterLeave, h.port.published.size)
        assertEquals("GroupLeave", h.port.kinds().last())
    }

    @Test
    fun `a Leave never lands before a heartbeat that was already being published`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        assertEquals(listOf("GroupStart", "GroupJoin"), h.port.kinds())

        // The heartbeat starts publishing and is held mid-publish.
        val gate = CompletableDeferred<Unit>()
        h.port.joinGate = gate
        advanceTimeBy(GROUP_CALL_HEARTBEAT_MS)
        runCurrent()
        assertEquals(listOf("GroupStart", "GroupJoin"), h.port.kinds())

        h.port.otherFreshMembers = setOf("other-device")
        h.host.leave()
        runCurrent()
        assertEquals("the Leave waits for the in-flight heartbeat", listOf("GroupStart", "GroupJoin"), h.port.kinds())

        gate.complete(Unit)
        runCurrent()
        assertEquals(listOf("GroupStart", "GroupJoin", "GroupJoin", "GroupLeave"), h.port.kinds())
    }

    @Test
    fun `a heartbeat that had not started publishing is skipped once the device left`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        h.port.otherFreshMembers = setOf("other-device")
        h.host.leave()
        advanceTimeBy(3 * GROUP_CALL_HEARTBEAT_MS)
        runCurrent()
        assertEquals(listOf("GroupStart", "GroupJoin", "GroupLeave"), h.port.kinds())
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
        h.port.otherFreshMembers = emptySet()
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
        h.port.otherFreshMembers = setOf("other-member")
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
                    quality = CallUiState.QualityTier.Good,
                    speaking = false,
                ),
            ),
        )
        runCurrent()
        h.host.ui.setMic(false)
        runCurrent()
        assertEquals(false, h.port.sessions.single().mic.last())
    }

    // ---- Group call: ends by itself when everyone else has left --------------------

    private fun participant(id: String, isLocal: Boolean) = CallUiState.Participant(
        id = id,
        displayName = if (isLocal) "You" else id,
        isLocal = isLocal,
        micOn = true,
        camOn = false,
        quality = CallUiState.QualityTier.Good,
        speaking = false,
    )

    @Test
    fun `the call ends by itself ten seconds after the last other participant left`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        val local = participant("local", isLocal = true)
        val remote = participant("peer-1", isLocal = false)
        h.host.ui.setParticipants(listOf(local, remote))
        runCurrent()
        h.host.ui.setParticipants(listOf(local))
        runCurrent()

        advanceTimeBy(GROUP_CALL_ALONE_END_MS - 1)
        runCurrent()
        assertEquals("still up just inside the window", 0, h.port.sessions.single().hangUps)

        advanceTimeBy(1)
        runCurrent()
        assertEquals(1, h.port.sessions.single().hangUps)
        // A hang-up: the Leave, then (nobody else fresh in the record) the End.
        assertEquals(listOf("GroupLeave", "End"), h.port.kinds().takeLast(2))
        assertEquals(FfiCallEndReason.HUNG_UP, (h.port.published.last().second as FfiCallSignalingBody.End).reason)
        assertNull(h.host.slot.value)
    }

    @Test
    fun `someone rejoining inside the window cancels the alone-end`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        val local = participant("local", isLocal = true)
        val remote = participant("peer-1", isLocal = false)
        h.host.ui.setParticipants(listOf(local, remote))
        runCurrent()
        h.host.ui.setParticipants(listOf(local))
        runCurrent()
        advanceTimeBy(GROUP_CALL_ALONE_END_MS / 2)
        runCurrent()
        h.host.ui.setParticipants(listOf(local, remote))
        runCurrent()

        advanceTimeBy(3 * GROUP_CALL_ALONE_END_MS)
        runCurrent()
        assertEquals(0, h.port.sessions.single().hangUps)
        assertNotNull(h.host.slot.value)
        assertTrue(h.port.kinds().none { it == "GroupLeave" || it == "End" })
    }

    @Test
    fun `a call that only ever had the local participant never ends by itself`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        h.host.ui.setParticipants(listOf(participant("local", isLocal = true)))
        runCurrent()

        advanceTimeBy(5 * GROUP_CALL_ALONE_END_MS)
        runCurrent()
        assertEquals(0, h.port.sessions.single().hangUps)
        assertNotNull(h.host.slot.value)
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

        /** Every call id an invite link was minted for, in order. */
        val minted = mutableListOf<String>()
        var mintError: Throwable? = null

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

        override suspend fun mintInviteLink(callId: ByteArray, identity: CallInviteIdentity): String {
            mintError?.let { throw it }
            val hex = callIdHexOf(callId)
            minted += hex
            return "https://lakko.me/c/$hex#secret"
        }
    }

    private fun inviteIdentity() = CallInviteIdentity(
        accountIdentitySeed = ByteArray(64),
        accountId = ByteArray(16),
        sessionToken = "token",
    )

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
        accountId = ByteArray(16),
    )

    // ---- Link call ------------------------------------------------------------------

    // ---- The invite link (docs/ux/call-links.md section 3) --------------------

    /** What `Copy link` put on the clipboard, in order. */
    private val clipboard = mutableListOf<String>()
    private val copy: (String) -> Unit = { clipboard += it }

    /** The one link this call has, read from where the last use put it. */
    private fun LinkHarness.link(): String? = mintedInviteUrl(host.invite.value)

    @Test
    fun `the host's first copy mints a link for the call it is in and copies it`() = runTest {
        val h = linkHarness()
        h.host.start(request(owner = true))
        assertEquals(InCallInvite.Idle, h.host.invite.value)

        h.host.copyInviteLink(inviteIdentity(), copy)

        // The LIVE call's id, which is what makes the link lead HERE.
        assertEquals(listOf(callIdHexOf(ByteArray(16) { 0x11 })), h.port.minted)
        val url = h.link()!!
        assertEquals(InCallInvite.Ready(url, LinkDelivery.Copied), h.host.invite.value)
        assertEquals(listOf(url), clipboard)
    }

    @Test
    fun `a second use re-offers the link instead of minting another`() = runTest {
        val h = linkHarness()
        h.host.start(request(owner = true))
        h.host.copyInviteLink(inviteIdentity(), copy)
        h.host.copyInviteLink(inviteIdentity(), copy)
        assertEquals(2, clipboard.size)
        assertEquals(clipboard[0], clipboard[1])
        // One capability on the ledger per call, not one per tap.
        assertEquals(1, h.port.minted.size)
    }

    @Test
    fun `a link that cannot be copied is shown to be copied by hand`() = runTest {
        val h = linkHarness()
        h.host.start(request(owner = true))
        h.host.copyInviteLink(inviteIdentity(), copy = null)
        val url = h.link()!!
        assertEquals(InCallInvite.Ready(url, LinkDelivery.Shown), h.host.invite.value)
    }

    @Test
    fun `a failed mint leaves the call running and the control ready to try again`() = runTest {
        val h = linkHarness()
        h.host.start(request(owner = true))
        h.port.mintError = java.io.IOException("no route")

        h.host.copyInviteLink(inviteIdentity(), copy)

        // Section 5: the call is unaffected and does not end.
        assertEquals(LinkCallPhase.LIVE, h.host.slot.value?.phase)
        assertTrue(h.host.occupiesSlot)
        assertEquals(InCallInvite.Failed(CallInviteFailure.NETWORK), h.host.invite.value)
        assertTrue(clipboard.isEmpty())

        // And the next use tries again.
        h.port.mintError = null
        h.host.copyInviteLink(inviteIdentity(), copy)
        assertTrue(h.host.invite.value is InCallInvite.Ready)
        assertEquals(1, clipboard.size)
    }

    @Test
    fun `a session lost mid-call is said in one sentence, not a mint`() = runTest {
        val h = linkHarness()
        h.host.start(request(owner = true))

        h.host.copyInviteLink(null, copy)

        assertTrue(h.port.minted.isEmpty())
        assertEquals(InCallInvite.Failed(CallInviteFailure.NO_SESSION), h.host.invite.value)
        assertTrue(clipboard.isEmpty())
    }

    @Test
    fun `a joiner has no link to offer`() = runTest {
        val h = linkHarness()
        h.host.start(request(owner = false))
        h.port.events!!.onJoined()
        assertEquals(LinkCallPhase.LIVE, h.host.slot.value?.phase)

        assertFalse(h.host.canInvite())
        h.host.copyInviteLink(inviteIdentity(), copy)
        assertFalse(h.host.sendInvite(inviteIdentity(), "Anna") { })
        // Not reported as a failure: there is simply no item to choose.
        assertEquals(InCallInvite.Idle, h.host.invite.value)
        assertTrue(h.port.minted.isEmpty())
        assertTrue(clipboard.isEmpty())
    }

    @Test
    fun `the next call never offers the previous call's link`() = runTest {
        val h = linkHarness()
        h.host.start(request(owner = true, callByte = 0x11))
        h.host.copyInviteLink(inviteIdentity(), copy)
        h.host.leave()
        assertEquals(InCallInvite.Idle, h.host.invite.value)

        h.host.start(request(owner = true, callByte = 0x22))
        h.host.copyInviteLink(inviteIdentity(), copy)

        assertEquals(2, clipboard.size)
        assertNotEquals(clipboard[0], clipboard[1])
        assertEquals(
            listOf(callIdHexOf(ByteArray(16) { 0x11 }), callIdHexOf(ByteArray(16) { 0x22 })),
            h.port.minted,
        )
    }

    // ---- Invite contacts ------------------------------------------------------

    @Test
    fun `an invitation is the call's link sent into the picked conversation`() = runTest {
        val h = linkHarness()
        h.host.start(request(owner = true))
        val sent = mutableListOf<String>()

        assertTrue(h.host.sendInvite(inviteIdentity(), "Anna") { sent += it })

        val url = h.link()!!
        // The whole message is the link, which is what lets every client draw
        // it as an invitation with a Join button.
        assertEquals(listOf(url), sent)
        assertNotNull(callInviteOf(url, "https://lakko.me"))
        assertEquals(InCallInvite.Ready(url, LinkDelivery.Sent("Anna")), h.host.invite.value)
        assertEquals(1, h.port.minted.size)
    }

    @Test
    fun `several conversations are invited with the one link`() = runTest {
        val h = linkHarness()
        h.host.start(request(owner = true))
        val sent = mutableListOf<String>()

        assertTrue(h.host.sendInvite(inviteIdentity(), "Anna") { sent += it })
        assertTrue(h.host.sendInvite(inviteIdentity(), "Trio") { sent += it })

        assertEquals(2, sent.size)
        assertEquals(sent[0], sent[1])
        assertEquals(1, h.port.minted.size)
        // The line names the conversation the last invitation went to.
        assertEquals(InCallInvite.Ready(sent[0], LinkDelivery.Sent("Trio")), h.host.invite.value)
    }

    @Test
    fun `a failed send does not mint again`() = runTest {
        val h = linkHarness()
        h.host.start(request(owner = true))

        // The link is minted, and then the send fails.
        assertFalse(h.host.sendInvite(inviteIdentity(), "Anna") { throw java.io.IOException("no route") })
        val url = h.link()!!
        assertEquals(InCallInvite.Failed(CallInviteFailure.NOT_SENT, url), h.host.invite.value)
        assertEquals(1, h.port.minted.size)
        // The call is untouched.
        assertEquals(LinkCallPhase.LIVE, h.host.slot.value?.phase)

        // Trying again, and copying instead, both use the link already there.
        val sent = mutableListOf<String>()
        assertTrue(h.host.sendInvite(inviteIdentity(), "Anna") { sent += it })
        h.host.copyInviteLink(inviteIdentity(), copy)
        assertEquals(listOf(url), sent)
        assertEquals(listOf(url), clipboard)
        assertEquals(1, h.port.minted.size)
    }

    @Test
    fun `an invitation whose link could not be prepared is not sent`() = runTest {
        val h = linkHarness()
        h.host.start(request(owner = true))
        h.port.mintError = java.io.IOException("no route")
        val sent = mutableListOf<String>()

        assertFalse(h.host.sendInvite(inviteIdentity(), "Anna") { sent += it })

        assertTrue(sent.isEmpty())
        assertEquals(InCallInvite.Failed(CallInviteFailure.NETWORK), h.host.invite.value)
    }

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
        // The diagnostic stays in the outcome for the log; a failure of no
        // known kind carries no reason to the screen (ADR-0061).
        assertNull(h.host.slot.value?.error)
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
                    quality = CallUiState.QualityTier.Good,
                    speaking = false,
                ),
            ),
        )
        runCurrent()
        h.host.ui.setMic(false)
        runCurrent()
        assertTrue(h.port.handles.single().mic.isEmpty())
    }

    // ---- The conversation row (docs/ux/calls.md section 6) ------------------------

    @Test
    fun `a finished group call leaves one outgoing row in the conversation`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        h.host.leave()
        runCurrent()
        assertEquals(1, h.port.rows.size)
        assertEquals(groupA, h.port.rows.single().first)
        assertEquals(
            CallTranscriptEntry(outcome = CallOutcome.OUTGOING, video = false, durationSecs = 0L),
            h.port.rows.single().second,
        )
    }

    @Test
    fun `a join that never connected leaves no row`() = runTest {
        val h = groupHarness()
        h.port.connectError = IllegalStateException("down")
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        assertTrue(h.port.rows.isEmpty())
    }

    // ---- Putting a call away (docs/ux/calls.md section 4.5) -------------------

    /**
     * A call is born expanded. The flag is cleared in the same breath as the
     * screen being asked for, which is why a previous call's put-away cannot
     * suppress this one.
     */
    @Test
    fun `a group call starts with its screen up, not put away`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        assertFalse(h.host.collapsed.value)
        assertTrue(h.host.overlayVisible.value)
    }

    /**
     * Section 4.5: collapsing "changes nothing about the call itself — the
     * media, the keys and the timer are untouched, only the picture is put
     * away". So the host must still hold the slot, still publish its heartbeat,
     * and never hang the session up.
     */
    @Test
    fun `collapsing a group call puts the screen away and touches nothing else`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        val session = h.port.sessions.single()

        h.host.collapse()
        runCurrent()

        assertTrue("the call is put away", h.host.collapsed.value)
        assertEquals("the call was hung up", 0, session.hangUps)
        assertNotNull("the slot was given up", h.host.slot.value)
        assertTrue(h.host.occupiesSlot)
        assertNotNull("the running time was lost", h.host.connectedSinceMs.value)

        // Still in the call: the heartbeat keeps telling the group so.
        val joinsBefore = h.port.kinds().count { it == "GroupJoin" }
        advanceTimeBy(GROUP_CALL_HEARTBEAT_MS)
        runCurrent()
        assertEquals(joinsBefore + 1, h.port.kinds().count { it == "GroupJoin" })
    }

    /** A tap on the bar brings it back. */
    @Test
    fun `expanding a group call brings the screen back`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        h.host.collapse()
        h.host.expand()
        runCurrent()
        assertFalse(h.host.collapsed.value)
        assertTrue(h.host.overlayVisible.value)
    }

    /**
     * Section 4.5: "a person who put the call away asked not to be interrupted
     * by it." So a collapsed call that ENDS must not un-collapse — doing so
     * would throw the `Call ended` screen across whatever the person went off to
     * read. The flag stays set until the next call takes the slot.
     */
    @Test
    fun `a collapsed group call that ends does not throw its screen back up`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        h.host.collapse()
        runCurrent()

        h.host.leave()
        runCurrent()

        assertTrue("the ended screen was thrown up over another screen", h.host.collapsed.value)
    }

    /**
     * ...and the very next call is nonetheless born expanded, which is the other
     * half of the same rule. Clearing the flag at the start rather than at the
     * end is what makes both hold at once.
     */
    @Test
    fun `the call after a collapsed one still shows its own screen`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        h.host.collapse()
        h.host.leave()
        runCurrent()
        assertTrue(h.host.collapsed.value)

        h.host.start(groupB, video = false, joinCallIdHex = null)
        runCurrent()
        assertFalse("the new call inherited the last one's put-away", h.host.collapsed.value)
        assertTrue(h.host.overlayVisible.value)
    }

    /** The same four rules for the call-link call, which has its own host. */
    @Test
    fun `a link call is born expanded, collapses without ending, and does not come back on its own`() =
        runTest {
            val h = linkHarness()
            h.host.start(request(owner = true))
            runCurrent()
            assertEquals(LinkCallPhase.LIVE, h.host.slot.value?.phase)
            assertFalse(h.host.collapsed.value)

            h.host.collapse()
            runCurrent()
            assertTrue(h.host.collapsed.value)
            assertEquals("the call was hung up", 0, h.port.handles.single().hangUps)
            assertNotNull("the slot was given up", h.host.slot.value)

            h.host.expand()
            assertFalse(h.host.collapsed.value)

            h.host.collapse()
            h.host.leave()
            runCurrent()
            assertTrue("the ended screen was thrown up", h.host.collapsed.value)

            h.host.start(request(owner = true, callByte = 0x33))
            runCurrent()
            assertFalse("the new call inherited the put-away", h.host.collapsed.value)
        }

    /**
     * The running time the bar shows survives the screen. The call screen
     * derives its own from the moment IT observed the connect, which restarts
     * from zero when the call is put away and brought back; this one does not.
     */
    @Test
    fun `the running time survives being put away and brought back`() = runTest {
        val h = groupHarness()
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        val connectedAt = h.host.connectedSinceMs.value
        assertNotNull(connectedAt)

        h.host.collapse()
        h.host.expand()
        h.host.collapse()
        runCurrent()

        assertEquals("the clock restarted", connectedAt, h.host.connectedSinceMs.value)
    }

    /** A call that never connected has no running time to show, so the bar's
     *  line stays a state word. */
    @Test
    fun `a call that never connected reports no running time`() = runTest {
        val h = groupHarness()
        h.port.connectError = IllegalStateException("down")
        h.host.start(groupA, video = false, joinCallIdHex = null)
        runCurrent()
        assertNull(h.host.connectedSinceMs.value)
    }
}
