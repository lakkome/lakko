// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiCallEndReason
import uniffi.lakko_ffi_uniffi.FfiCallMediaFlags
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody

/**
 * The ongoing-group-call ledger (`GroupCallLedger.kt`): the reducer, liveness,
 * the tie-break, and the C-11 / C-12 routing rules.
 *
 * Pure JVM: the `FfiCallSignalingBody` variants are plain Kotlin data classes,
 * so constructing them never touches the UniFFI `.so`.
 */
class GroupCallLedgerTest {

    private val t0 = 1_800_000_000_000L

    private fun id(fill: Int): String = callIdHexOf(ByteArray(16) { fill.toByte() })

    private fun start(
        callIdHex: String,
        createdAtMs: Long = t0,
        leaf: UInt = 1u,
        video: Boolean = false,
    ) = GroupCallSignal.Start(
        callIdHex = callIdHex,
        initiatorLeaf = leaf,
        createdAtMs = createdAtMs,
        audio = true,
        video = video,
    )

    private fun fold(
        signals: List<GroupCallSignal>,
        observedAtMs: Long = t0,
        nowMs: Long = t0,
    ): GroupCallRecord? =
        signals.fold(null as GroupCallRecord?) { acc, s -> reduceGroupCall(acc, s, observedAtMs, nowMs) }

    // -----------------------------------------------------------------
    // Create / join / leave / end
    // -----------------------------------------------------------------

    @Test
    fun `a start creates the record with nobody joined`() {
        val record = fold(listOf(start(id(1), leaf = 4u, video = true)))!!

        assertEquals(id(1), record.callIdHex)
        assertEquals(4u, record.initiatorLeaf)
        assertTrue(record.video)
        assertEquals(t0, record.startedAtMs)
        assertTrue(record.joinedLeaves.isEmpty())
    }

    @Test
    fun `the announced start time is bounded by the DS row time`() {
        // A sender claiming a start in the future of the row is clamped to it.
        val clamped = reduceGroupCall(
            null,
            start(id(1), createdAtMs = t0 + 3_600_000L),
            observedAtMs = t0,
            nowMs = t0,
        )!!
        assertEquals(t0, clamped.startedAtMs)

        // An earlier claim is kept — it can only shorten the call's liveness.
        val earlier = reduceGroupCall(
            null,
            start(id(1), createdAtMs = t0 - 5_000L),
            observedAtMs = t0,
            nowMs = t0,
        )!!
        assertEquals(t0 - 5_000L, earlier.startedAtMs)
    }

    @Test
    fun `join and leave update the joined set for the recorded call`() {
        val joined = fold(
            listOf(
                start(id(1)),
                GroupCallSignal.Join(id(1), 2u),
                GroupCallSignal.Join(id(1), 5u),
            ),
        )!!
        assertEquals(setOf(2u, 5u), joined.joinedLeaves)

        val left = reduceGroupCall(joined, GroupCallSignal.Leave(id(1), 2u), t0, t0)!!
        assertEquals(setOf(5u), left.joinedLeaves)
    }

    @Test
    fun `C-12 - join and leave for a different call id are dropped`() {
        val record = fold(listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u)))!!

        assertSame(record, reduceGroupCall(record, GroupCallSignal.Join(id(9), 7u), t0, t0))
        assertSame(record, reduceGroupCall(record, GroupCallSignal.Leave(id(9), 2u), t0, t0))
    }

    @Test
    fun `join or leave without a recorded call creates nothing`() {
        assertNull(reduceGroupCall(null, GroupCallSignal.Join(id(1), 2u), t0, t0))
        assertNull(reduceGroupCall(null, GroupCallSignal.Leave(id(1), 2u), t0, t0))
    }

    @Test
    fun `a call end clears only the recorded call`() {
        val record = fold(listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u)))!!

        assertNull(reduceGroupCall(record, GroupCallSignal.End(id(1)), t0, t0))
        assertSame(record, reduceGroupCall(record, GroupCallSignal.End(id(9)), t0, t0))
    }

    @Test
    fun `a repeated start for the recorded call keeps the joined set`() {
        val record = fold(listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u)))!!

        val again = reduceGroupCall(record, start(id(1)), t0 + 1_000L, t0 + 1_000L)
        assertEquals(setOf(2u), again!!.joinedLeaves)
    }

    // -----------------------------------------------------------------
    // Liveness
    // -----------------------------------------------------------------

    @Test
    fun `an unjoined call is live only for the grace window`() {
        val record = fold(listOf(start(id(1))))!!

        assertTrue(isGroupCallLive(record, t0 + GROUP_CALL_UNJOINED_GRACE_MS - 1))
        assertFalse(isGroupCallLive(record, t0 + GROUP_CALL_UNJOINED_GRACE_MS))
    }

    @Test
    fun `a joined call is live until the maximum age`() {
        val record = fold(listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u)))!!

        assertTrue(isGroupCallLive(record, t0 + GROUP_CALL_UNJOINED_GRACE_MS + 1))
        assertTrue(isGroupCallLive(record, t0 + GROUP_CALL_MAX_AGE_MS - 1))
        assertFalse(isGroupCallLive(record, t0 + GROUP_CALL_MAX_AGE_MS))
    }

    @Test
    fun `a call whose last participant left stops being live after the grace`() {
        val record = fold(
            listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u), GroupCallSignal.Leave(id(1), 2u)),
        )!!

        assertFalse(isGroupCallLive(record, t0 + GROUP_CALL_UNJOINED_GRACE_MS))
    }

    // -----------------------------------------------------------------
    // Two different calls
    // -----------------------------------------------------------------

    @Test
    fun `two live starts converge on the earliest regardless of arrival order`() {
        val early = start(id(9), createdAtMs = t0)
        val late = start(id(1), createdAtMs = t0 + 2_000L)
        val now = t0 + 3_000L

        val earlyFirst = listOf(early, late).fold(null as GroupCallRecord?) { acc, s ->
            reduceGroupCall(acc, s, observedAtMs = now, nowMs = now)
        }
        val lateFirst = listOf(late, early).fold(null as GroupCallRecord?) { acc, s ->
            reduceGroupCall(acc, s, observedAtMs = now, nowMs = now)
        }

        assertEquals(id(9), earlyFirst!!.callIdHex)
        assertEquals(id(9), lateFirst!!.callIdHex)
    }

    @Test
    fun `equal start times converge on the lowest call id hex`() {
        val a = start(id(0x0a), createdAtMs = t0)
        val b = start(id(0x0b), createdAtMs = t0)

        val ab = listOf(a, b).fold(null as GroupCallRecord?) { acc, s -> reduceGroupCall(acc, s, t0, t0) }
        val ba = listOf(b, a).fold(null as GroupCallRecord?) { acc, s -> reduceGroupCall(acc, s, t0, t0) }

        assertEquals(id(0x0a), ab!!.callIdHex)
        assertEquals(id(0x0a), ba!!.callIdHex)
    }

    @Test
    fun `a start replaces a record that is no longer live`() {
        val stale = fold(listOf(start(id(1), createdAtMs = t0)))!!
        val now = t0 + GROUP_CALL_UNJOINED_GRACE_MS + 10_000L

        val next = reduceGroupCall(stale, start(id(9), createdAtMs = now), now, now)
        assertEquals(id(9), next!!.callIdHex)
    }

    @Test
    fun `a start that is not itself live never displaces a live call`() {
        val live = fold(listOf(start(id(9)), GroupCallSignal.Join(id(9), 2u)))!!
        val now = t0 + 10 * 60_000L
        // An older, never-joined start delivered late (a backlog poll).
        val backlog = start(id(1), createdAtMs = t0 - 60_000L)

        assertSame(live, reduceGroupCall(live, backlog, observedAtMs = t0, nowMs = now))
    }

    // -----------------------------------------------------------------
    // Routing (C-11) and body mapping
    // -----------------------------------------------------------------

    private val callId = ByteArray(16) { 7 }

    @Test
    fun `C-11 - group control bodies never route to the 1x1 session`() {
        val media = FfiCallMediaFlags(audio = true, video = false)
        assertEquals(
            CallSignalingRoute.GROUP_LEDGER_ONLY,
            callSignalingRoute(FfiCallSignalingBody.GroupStart(callId, 1u, 5uL, media)),
        )
        assertEquals(
            CallSignalingRoute.GROUP_LEDGER_ONLY,
            callSignalingRoute(FfiCallSignalingBody.GroupJoin(callId, 1u, 5uL)),
        )
        assertEquals(
            CallSignalingRoute.GROUP_LEDGER_ONLY,
            callSignalingRoute(FfiCallSignalingBody.GroupLeave(callId, 1u, FfiCallEndReason.HUNG_UP)),
        )
    }

    @Test
    fun `1x1 bodies route to the session only and a call end to both`() {
        val media = FfiCallMediaFlags(audio = true, video = false)
        assertEquals(
            CallSignalingRoute.ONE_TO_ONE_ONLY,
            callSignalingRoute(FfiCallSignalingBody.Offer(callId, "v=0", 5uL, media)),
        )
        assertEquals(
            CallSignalingRoute.ONE_TO_ONE_ONLY,
            callSignalingRoute(FfiCallSignalingBody.Answer(callId, "v=0")),
        )
        assertEquals(
            CallSignalingRoute.ONE_TO_ONE_ONLY,
            callSignalingRoute(FfiCallSignalingBody.IceCandidate(callId, "c", "0", 0u)),
        )
        assertEquals(
            CallSignalingRoute.BOTH,
            callSignalingRoute(FfiCallSignalingBody.End(callId, FfiCallEndReason.HUNG_UP)),
        )
    }

    @Test
    fun `bodies map onto ledger signals`() {
        val start = groupCallSignalOf(
            FfiCallSignalingBody.GroupStart(callId, 3u, 42uL, FfiCallMediaFlags(audio = true, video = true)),
        )
        assertEquals(
            GroupCallSignal.Start(callIdHexOf(callId), 3u, 42L, audio = true, video = true),
            start,
        )
        assertEquals(
            GroupCallSignal.Join(callIdHexOf(callId), 6u),
            groupCallSignalOf(FfiCallSignalingBody.GroupJoin(callId, 6u, 1uL)),
        )
        assertEquals(
            GroupCallSignal.Leave(callIdHexOf(callId), 6u),
            groupCallSignalOf(FfiCallSignalingBody.GroupLeave(callId, 6u, FfiCallEndReason.ERROR)),
        )
        assertEquals(
            GroupCallSignal.End(callIdHexOf(callId)),
            groupCallSignalOf(FfiCallSignalingBody.End(callId, FfiCallEndReason.HUNG_UP)),
        )
        assertNull(groupCallSignalOf(FfiCallSignalingBody.Answer(callId, "v=0")))
    }

    @Test
    fun `an out of range created_at clamps instead of wrapping negative`() {
        val signal = groupCallSignalOf(
            FfiCallSignalingBody.GroupStart(callId, 3u, ULong.MAX_VALUE, FfiCallMediaFlags(true, false)),
        ) as GroupCallSignal.Start

        assertEquals(Long.MAX_VALUE, signal.createdAtMs)
        // ...which the row bound then brings back to the row time.
        assertEquals(t0, reduceGroupCall(null, signal, t0, t0)!!.startedAtMs)
    }

    @Test
    fun `call id hex round trips`() {
        val bytes = ByteArray(16) { (it * 17).toByte() }
        assertTrue(bytes.contentEquals(callIdBytesOf(callIdHexOf(bytes))))
        assertNull(callIdBytesOf("abc"))
        assertNull(callIdBytesOf("zz"))
    }

    // -----------------------------------------------------------------
    // What the chat screen does with a record
    // -----------------------------------------------------------------

    @Test
    fun `a live call is joinable only while no call is up on the chat`() {
        val record = fold(listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u)))!!

        assertEquals(record, joinableGroupCall(record, t0, anyCallActive = false, activeGroupCallIdHex = null))
        assertNull(joinableGroupCall(record, t0, anyCallActive = true, activeGroupCallIdHex = null))
        assertNull(joinableGroupCall(record, t0, anyCallActive = false, activeGroupCallIdHex = id(1)))
        assertNull(joinableGroupCall(record, t0 + GROUP_CALL_MAX_AGE_MS, anyCallActive = false, activeGroupCallIdHex = null))
        assertNull(joinableGroupCall(null, t0, anyCallActive = false, activeGroupCallIdHex = null))
    }

    @Test
    fun `the call buttons join a live call instead of starting another`() {
        val record = fold(listOf(start(id(1))))!!

        assertEquals(CallButtonAction.JOIN_LIVE_GROUP_CALL, callButtonAction(record, isGroupChat = true))
        assertEquals(CallButtonAction.JOIN_LIVE_GROUP_CALL, callButtonAction(record, isGroupChat = false))
        assertEquals(CallButtonAction.START_GROUP_CALL, callButtonAction(null, isGroupChat = true))
        assertEquals(CallButtonAction.START_ONE_TO_ONE_CALL, callButtonAction(null, isGroupChat = false))
    }

    @Test
    fun `the last one out ends the call, anyone else only leaves`() {
        val alone = fold(listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u)))!!
        assertTrue(shouldEndGroupCallAfterLeave(alone, id(1), ownLeaf = 2u))

        val withOthers = reduceGroupCall(alone, GroupCallSignal.Join(id(1), 5u), t0, t0)
        assertFalse(shouldEndGroupCallAfterLeave(withOthers, id(1), ownLeaf = 2u))

        // A record about a different call knows nothing about who is in ours.
        assertFalse(shouldEndGroupCallAfterLeave(alone, id(9), ownLeaf = 2u))
        assertFalse(shouldEndGroupCallAfterLeave(null, id(1), ownLeaf = 2u))
    }
}
