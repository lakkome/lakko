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
        assertTrue(record.joinedMembers.isEmpty())
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
                GroupCallSignal.Join(id(1), 2u, "m2"),
                GroupCallSignal.Join(id(1), 5u, "m5"),
            ),
        )!!
        assertEquals(setOf("m2", "m5"), joined.joinedMembers.keys)

        val left = reduceGroupCall(joined, GroupCallSignal.Leave(id(1), 2u, "m2"), t0, t0)!!
        assertEquals(setOf("m5"), left.joinedMembers.keys)
    }

    @Test
    fun `C-12 - join and leave for a different call id are dropped`() {
        val record = fold(listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u, "m2")))!!

        assertSame(record, reduceGroupCall(record, GroupCallSignal.Join(id(9), 7u, "m7"), t0, t0))
        assertSame(record, reduceGroupCall(record, GroupCallSignal.Leave(id(9), 2u, "m2"), t0, t0))
    }

    @Test
    fun `join or leave without a recorded call creates nothing`() {
        assertNull(reduceGroupCall(null, GroupCallSignal.Join(id(1), 2u, "m2"), t0, t0))
        assertNull(reduceGroupCall(null, GroupCallSignal.Leave(id(1), 2u, "m2"), t0, t0))
    }

    @Test
    fun `a call end clears only the recorded call`() {
        val record = fold(listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u, "m2")))!!

        assertNull(reduceGroupCall(record, GroupCallSignal.End(id(1)), t0, t0))
        assertSame(record, reduceGroupCall(record, GroupCallSignal.End(id(9)), t0, t0))
    }

    @Test
    fun `a repeated start for the recorded call keeps the joined set`() {
        val record = fold(listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u, "m2")))!!

        val again = reduceGroupCall(record, start(id(1)), t0 + 1_000L, t0 + 1_000L)
        assertEquals(setOf("m2"), again!!.joinedMembers.keys)
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
    fun `a heartbeating call is live until the maximum age`() {
        var record = fold(listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u, "m2")))!!
        // One heartbeat per interval, each observed at its DS row time.
        var at = t0
        while (at + GROUP_CALL_HEARTBEAT_MS < t0 + GROUP_CALL_MAX_AGE_MS) {
            at += GROUP_CALL_HEARTBEAT_MS
            record = reduceGroupCall(record, GroupCallSignal.Join(id(1), 2u, "m2"), at, at)!!
            assertTrue(isGroupCallLive(record, at + GROUP_CALL_HEARTBEAT_MS - 1))
        }
        assertFalse(isGroupCallLive(record, t0 + GROUP_CALL_MAX_AGE_MS))
    }

    @Test
    fun `a member counts only while its newest Join is fresh`() {
        val record = fold(
            listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u, "m2"), GroupCallSignal.Join(id(1), 5u, "m5")),
        )!!

        assertEquals(setOf("m2", "m5"), freshGroupCallMembers(record, t0 + GROUP_CALL_MEMBER_FRESH_MS))
        assertEquals(emptySet<String>(), freshGroupCallMembers(record, t0 + GROUP_CALL_MEMBER_FRESH_MS + 1))

        // m5 heartbeats, m2 does not.
        val later = t0 + GROUP_CALL_HEARTBEAT_MS
        val beat = reduceGroupCall(record, GroupCallSignal.Join(id(1), 5u, "m5"), later, later)!!
        assertEquals(setOf("m5"), freshGroupCallMembers(beat, t0 + GROUP_CALL_MEMBER_FRESH_MS + 1))
    }

    @Test
    fun `a heartbeat advances last seen and never lowers it`() {
        val record = fold(listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u, "m2")))!!

        val beat = reduceGroupCall(record, GroupCallSignal.Join(id(1), 2u, "m2"), t0 + 60_000L, t0 + 60_000L)!!
        assertEquals(GroupCallMember("m2", t0 + 60_000L), beat.joinedMembers["m2"])

        // A row older than the newest one (a backlog re-delivery) changes nothing.
        assertSame(beat, reduceGroupCall(beat, GroupCallSignal.Join(id(1), 2u, "m2"), t0 + 1L, t0 + 60_000L))
    }

    @Test
    fun `a ghost participant stops making the call live after the freshness window`() {
        // Joined, then the app was killed: no Leave, no more heartbeats.
        val record = fold(listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u, "m2")))!!

        assertTrue(isGroupCallLive(record, t0 + GROUP_CALL_MEMBER_FRESH_MS))
        assertFalse(isGroupCallLive(record, t0 + GROUP_CALL_MEMBER_FRESH_MS + 1))
        assertNull(
            joinableGroupCall(record, t0 + GROUP_CALL_MEMBER_FRESH_MS + 1, anyCallActive = false, activeGroupCallIdHex = null),
        )
        // ...and a new start is not blocked by it.
        val now = t0 + GROUP_CALL_MEMBER_FRESH_MS + 1
        assertEquals(id(9), reduceGroupCall(record, start(id(9), createdAtMs = now), now, now)!!.callIdHex)
    }

    @Test
    fun `two devices of one person count once, and one of them leaving keeps the call live`() {
        val phone = GroupCallSignal.Join(id(1), 2u, memberKey = "bob-phone", personKey = "bob")
        val laptop = GroupCallSignal.Join(id(1), 3u, memberKey = "bob-laptop", personKey = "bob")
        val carol = GroupCallSignal.Join(id(1), 4u, memberKey = "carol-phone", personKey = "carol")
        val record = fold(listOf(start(id(1)), phone, laptop, carol))!!

        assertEquals(3, freshGroupCallMembers(record, t0).size)
        assertEquals("devices of one person count once", 2, freshGroupCallPeopleCount(record, t0))

        // Carol leaves; then Bob's phone leaves while his laptop stays.
        val later = t0 + GROUP_CALL_UNJOINED_GRACE_MS + 1
        val noCarol = reduceGroupCall(record, GroupCallSignal.Leave(id(1), 4u, "carol-phone", "carol"), later, later)!!
        val phoneLeft = reduceGroupCall(noCarol, GroupCallSignal.Leave(id(1), 2u, "bob-phone", "bob"), later, later)!!
        assertEquals(setOf("bob-laptop"), phoneLeft.joinedMembers.keys)
        assertTrue(isGroupCallLive(phoneLeft, later))
        assertEquals(1, freshGroupCallPeopleCount(phoneLeft, later))

        // The phone's leave plan: the laptop is someone else in the call, so no End.
        assertFalse(shouldEndGroupCallAfterLeave(noCarol, id(1), ownMemberKey = "bob-phone", nowMs = later))
    }

    @Test
    fun `a person key that did not resolve counts the device as its own person`() {
        val record = fold(
            listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u, memberKey = "dev", personKey = null)),
        )!!
        assertEquals(GroupCallMember("dev", t0), record.joinedMembers["dev"])
        assertEquals(1, freshGroupCallPeopleCount(record, t0))
    }

    @Test
    fun `this device's own stale entry is ignored while it is not in the call`() {
        // Before a relaunch this device ("me") was in the call alone; its record
        // survived the relaunch and its entry is still inside the window.
        val record = fold(
            listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u, memberKey = "me", personKey = "my-account")),
        )!!
        val now = t0 + GROUP_CALL_UNJOINED_GRACE_MS + 1

        // Not in the call: the own entry is a dead session.
        assertEquals("me", groupCallIgnoredDeviceKey(record, ownDeviceKey = "me", activeGroupCallIdHex = null))
        assertFalse(isGroupCallLive(record, now, ignoreDeviceKey = "me"))
        assertEquals(0, freshGroupCallPeopleCount(record, now, ignoreDeviceKey = "me"))
        assertNull(joinableGroupCall(record, now, anyCallActive = false, activeGroupCallIdHex = null, ownDeviceKey = "me"))

        // Another member present: the call is joinable and counts only them.
        val withBob = reduceGroupCall(record, GroupCallSignal.Join(id(1), 3u, "bob-phone", "bob"), now, now)!!
        assertEquals(withBob, joinableGroupCall(withBob, now, false, null, ownDeviceKey = "me"))
        assertEquals(1, freshGroupCallPeopleCount(withBob, now, ignoreDeviceKey = "me"))

        // In the call: the own entry counts.
        assertNull(groupCallIgnoredDeviceKey(record, ownDeviceKey = "me", activeGroupCallIdHex = id(1)))
        assertTrue(isGroupCallLive(record, now, ignoreDeviceKey = null))
    }

    @Test
    fun `a join or leave whose sender did not resolve changes nothing`() {
        val record = fold(listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u, "m2")))!!

        assertSame(record, reduceGroupCall(record, GroupCallSignal.Join(id(1), 3u, null), t0, t0))
        assertSame(record, reduceGroupCall(record, GroupCallSignal.Leave(id(1), 2u, null), t0, t0))
    }

    @Test
    fun `C-68 - a member added into a vacated leaf does not inherit the leaver's place`() {
        // Bob (leaf 2) joined; Bob is removed and Dave is added into leaf 2.
        val bobIn = fold(listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u, "bob")))!!

        // Dave's rows resolve to Dave's identity, even though the leaf is the same.
        val daveLeaves = reduceGroupCall(bobIn, GroupCallSignal.Leave(id(1), 2u, "dave"), t0, t0)!!
        assertEquals("Dave's Leave does not remove Bob", setOf("bob"), daveLeaves.joinedMembers.keys)
        val daveJoins = reduceGroupCall(bobIn, GroupCallSignal.Join(id(1), 2u, "dave"), t0, t0)!!
        assertEquals(setOf("bob", "dave"), daveJoins.joinedMembers.keys)

        // Bob's own Leave removes Bob only.
        val bobLeaves = reduceGroupCall(daveJoins, GroupCallSignal.Leave(id(1), 2u, "bob"), t0, t0)!!
        assertEquals(setOf("dave"), bobLeaves.joinedMembers.keys)
    }

    @Test
    fun `a row names the resolved sender, and a mismatched leaf is refused`() {
        val cid = ByteArray(16) { 3 }
        val join = groupCallSignalForRow(FfiCallSignalingBody.GroupJoin(cid, 4u, 1uL), senderLeaf = 4u, senderKey = "k4")
        assertEquals(GroupCallSignal.Join(callIdHexOf(cid), 4u, "k4", null), join)
        assertNull(groupCallSignalForRow(FfiCallSignalingBody.GroupJoin(cid, 4u, 1uL), senderLeaf = 5u, senderKey = "k5"))
    }

    @Test
    fun `a call whose last participant left stops being live after the grace`() {
        val record = fold(
            listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u, "m2"), GroupCallSignal.Leave(id(1), 2u, "m2")),
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
        val now = t0 + 10 * 60_000L
        // Joined at t0 and still heartbeating at `now`.
        val live = reduceGroupCall(
            fold(listOf(start(id(9)), GroupCallSignal.Join(id(9), 2u, "m2")))!!,
            GroupCallSignal.Join(id(9), 2u, "m2"),
            observedAtMs = now,
            nowMs = now,
        )!!
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
        val record = fold(listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u, "m2")))!!

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
        val alone = fold(listOf(start(id(1)), GroupCallSignal.Join(id(1), 2u, "m2")))!!
        assertTrue(shouldEndGroupCallAfterLeave(alone, id(1), ownMemberKey = "m2", nowMs = t0))

        val withOthers = reduceGroupCall(alone, GroupCallSignal.Join(id(1), 5u, "m5"), t0, t0)
        assertFalse(shouldEndGroupCallAfterLeave(withOthers, id(1), ownMemberKey = "m2", nowMs = t0))
        // Another member that stopped heartbeating is not "someone else in the call".
        assertTrue(
            shouldEndGroupCallAfterLeave(withOthers, id(1), ownMemberKey = "m2", nowMs = t0 + GROUP_CALL_MEMBER_FRESH_MS + 1),
        )

        // A record about a different call knows nothing about who is in ours.
        assertFalse(shouldEndGroupCallAfterLeave(alone, id(9), ownMemberKey = "m2", nowMs = t0))
        assertFalse(shouldEndGroupCallAfterLeave(null, id(1), ownMemberKey = "m2", nowMs = t0))
    }
}
