// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * docs/ux/navigation.md section 5.1.1 — a call started with `New call`, or
 * joined from a call link, leaves one row in the Calls list: when this device
 * leaves it or it ends for it, whichever comes first, and only if this device
 * got past the join step.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class LinkCallHistoryTest {

    // ---------------------------------------------------------- the recorder

    @Test
    fun `a call that never went live files nothing`() {
        val recorder = LinkCallHistoryRecorder(callIdHex = "0a", outgoing = false, video = false)
        assertNull(recorder.finish(10_000L))
    }

    @Test
    fun `a live call files one row, stamped when this device joined`() {
        val recorder = LinkCallHistoryRecorder(callIdHex = "0a", outgoing = true, video = true)
        recorder.live(1_000L)
        recorder.live(5_000L)
        val row = recorder.finish(66_000L)!!
        assertNull(row.conversationIdHex)
        assertEquals("0a", row.callIdHex)
        assertEquals(CallOutcome.OUTGOING, row.outcome)
        assertTrue(row.video)
        assertEquals(1_000L, row.startedAtMs)
        assertEquals(65L, row.durationSecs)
        // Leaving and the end both report: the second one files nothing.
        assertNull(recorder.finish(70_000L))
    }

    @Test
    fun `a joiner's row is incoming, and a camera turned on makes it video`() {
        val recorder = LinkCallHistoryRecorder(callIdHex = "0a", outgoing = false, video = false)
        recorder.live(0L)
        recorder.sawVideo()
        val row = recorder.finish(1_000L)!!
        assertEquals(CallOutcome.INCOMING, row.outcome)
        assertTrue(row.video)
    }

    @Test
    fun `a clock that moved backwards files the call without a duration`() {
        val recorder = LinkCallHistoryRecorder(callIdHex = "0a", outgoing = true, video = false)
        recorder.live(5_000L)
        assertNull(recorder.finish(1_000L)!!.durationSecs)
    }

    @Test
    fun `a guest account files nothing`() {
        assertFalse(linkCallWritesHistory(guestAccount = true))
        assertTrue(linkCallWritesHistory(guestAccount = false))
    }

    // -------------------------------------------------------------- the host

    private class Harness(
        val host: LinkCallHost,
        val port: HostedCallsTest.FakeLinkPort,
        val filed: MutableList<Pair<ByteArray, CallHistoryEntry>>,
        var now: Long,
    )

    private fun TestScope.harness(): Harness {
        val port = HostedCallsTest.FakeLinkPort()
        val filed = mutableListOf<Pair<ByteArray, CallHistoryEntry>>()
        lateinit var h: Harness
        val host = LinkCallHost(
            scope = backgroundScope,
            otherOccupant = { null },
            onSlotChanged = { },
            warn = { _, _ -> },
            recordHistory = { account, entry -> filed += account to entry },
            nowMs = { h.now },
        )
        host.attach(port)
        h = Harness(host, port, filed, now = 1_000L)
        runCurrent()
        return h
    }

    private val account = ByteArray(16) { 0x42 }

    private fun request(owner: Boolean, callByte: Int = 0x11, slug: String? = "slug") = LinkCallRequest(
        callId = ByteArray(16) { callByte.toByte() },
        ownerAccountId = ByteArray(16),
        ownerSignatureKey = ByteArray(32),
        mediaVideo = true,
        isOwner = owner,
        slug = slug,
        accountIdentitySeed = ByteArray(64),
        sessionToken = "token",
        accountId = account,
    )

    @Test
    fun `New call files an outgoing row when this device leaves`() = runTest {
        val h = harness()
        h.host.start(request(owner = true, slug = null))
        assertTrue(h.filed.isEmpty())
        h.now = 31_000L
        h.host.leave()
        val (filedAccount, row) = h.filed.single()
        assertArrayEquals(account, filedAccount)
        assertEquals(callIdHexOf(ByteArray(16) { 0x11 }), row.callIdHex)
        assertEquals(CallOutcome.OUTGOING, row.outcome)
        assertEquals(1_000L, row.startedAtMs)
        assertEquals(30L, row.durationSecs)
        assertTrue(row.video)
    }

    @Test
    fun `a joiner admitted from a link files an incoming row when the call ends`() = runTest {
        val h = harness()
        h.host.start(request(owner = false))
        h.now = 2_000L
        h.port.events!!.onJoined()
        h.now = 12_000L
        h.port.events!!.onEnded()
        runCurrent()
        val row = h.filed.single().second
        assertEquals(CallOutcome.INCOMING, row.outcome)
        assertEquals(2_000L, row.startedAtMs)
        assertEquals(10L, row.durationSecs)
        // Leaving after the end files nothing more.
        h.host.leave()
        assertEquals(1, h.filed.size)
    }

    @Test
    fun `a joiner who gave up waiting to be admitted files nothing`() = runTest {
        val h = harness()
        h.host.start(request(owner = false))
        h.host.leave()
        assertTrue(h.filed.isEmpty())
    }

    @Test
    fun `a joiner nobody admitted files nothing`() = runTest {
        val h = harness()
        h.host.start(request(owner = false))
        h.port.events!!.onNoCommitter()
        runCurrent()
        h.host.leave()
        assertTrue(h.filed.isEmpty())
    }

    @Test
    fun `ending for everyone files the owner's row once`() = runTest {
        val h = harness()
        h.host.start(request(owner = true, slug = null))
        h.now = 5_000L
        h.host.endForEveryone()
        runCurrent()
        assertEquals(1, h.filed.size)
        assertEquals(4L, h.filed.single().second.durationSecs)
    }

    @Test
    fun `each call is its own row`() = runTest {
        val h = harness()
        h.host.start(request(owner = true, callByte = 0x11, slug = null))
        h.host.leave()
        h.host.start(request(owner = true, callByte = 0x22, slug = null))
        h.host.leave()
        assertEquals(
            listOf(callIdHexOf(ByteArray(16) { 0x11 }), callIdHexOf(ByteArray(16) { 0x22 })),
            h.filed.map { it.second.callIdHex },
        )
    }

    @Test
    fun `a call that failed to start files nothing`() = runTest {
        val h = harness()
        h.port.startError = IllegalStateException("no route")
        h.host.start(request(owner = false))
        h.host.leave()
        assertTrue(h.filed.isEmpty())
    }
}
