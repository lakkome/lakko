// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import me.lakko.app.l10n.UiText
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
 * Ending a group call for everyone (`docs/ux/calls.md` section 4.1.2): the
 * rules of `EndForEveryone.kt`, the starter the ledger records, and its round
 * trip through the store. The Kotlin twin of the web
 * `end-for-everyone.test.ts`.
 *
 * Pure JVM: the `FfiCallSignalingBody` variants are plain Kotlin data classes,
 * so constructing them never touches the UniFFI `.so`. What the web suite
 * checks through the real core (the reason surviving compose and parse) has no
 * host-JVM counterpart here.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class EndForEveryoneTest {

    private val t0 = 1_800_000_000_000L
    private val starter = "c3RhcnRlcg=="
    private val other = "b3RoZXI="
    private val callId = ByteArray(16) { 0x2a }
    private val callHex = callIdHexOf(callId)
    private val groupA = "aa".repeat(16)

    private fun record(initiatorPersonKey: String? = starter, callIdHex: String = callHex) = GroupCallRecord(
        callIdHex = callIdHex,
        audio = true,
        video = false,
        initiatorLeaf = 4u,
        startedAtMs = t0,
        joinedMembers = mapOf("bob-phone" to GroupCallMember("bob", t0 + 1_000L)),
        lastActivityMs = t0 + 1_000L,
        initiatorPersonKey = initiatorPersonKey,
    )

    private fun end(reason: FfiCallEndReason, id: ByteArray = callId) = FfiCallSignalingBody.End(id, reason)

    // ---- endOpensMenu ---------------------------------------------------------

    @Test
    fun `End opens the choice for the starter while someone else is in the call`() {
        assertTrue(endOpensMenu(isStarter = true, othersInCall = 1))
        assertTrue(endOpensMenu(isStarter = true, othersInCall = 5))
    }

    @Test
    fun `End ends at once for the starter alone in the call`() {
        assertFalse(endOpensMenu(isStarter = true, othersInCall = 0))
    }

    @Test
    fun `End never opens the choice for anyone else`() {
        assertFalse(endOpensMenu(isStarter = false, othersInCall = 3))
        assertFalse(endOpensMenu(isStarter = false, othersInCall = 0))
    }

    // ---- endsTheCallForMe -----------------------------------------------------

    @Test
    fun `the starter's end for everyone takes the device out`() {
        assertTrue(endsTheCallForMe(FfiCallEndReason.ENDED_FOR_ALL, starter, starter))
    }

    @Test
    fun `the same message from anyone else is ignored`() {
        assertFalse(endsTheCallForMe(FfiCallEndReason.ENDED_FOR_ALL, other, starter))
    }

    @Test
    fun `an ordinary end is ignored, even from the starter`() {
        for (reason in FfiCallEndReason.entries) {
            if (reason == FfiCallEndReason.ENDED_FOR_ALL) continue
            assertFalse("$reason took the device out", endsTheCallForMe(reason, starter, starter))
        }
    }

    @Test
    fun `an unknown sender or an unknown starter ends nothing`() {
        assertFalse(endsTheCallForMe(FfiCallEndReason.ENDED_FOR_ALL, null, starter))
        assertFalse(endsTheCallForMe(FfiCallEndReason.ENDED_FOR_ALL, "", ""))
        assertFalse(endsTheCallForMe(FfiCallEndReason.ENDED_FOR_ALL, starter, null))
        assertFalse(endsTheCallForMe(FfiCallEndReason.ENDED_FOR_ALL, null, null))
    }

    // ---- The line -------------------------------------------------------------

    @Test
    fun `the line names the starter when the device can`() {
        assertEquals(
            UiText("call.ended.for_all_by", texts = mapOf("name" to "Anna")),
            endedForEveryoneLine("Anna"),
        )
        assertEquals(
            UiText("call.ended.for_all_by", texts = mapOf("name" to "Anna")),
            endedForEveryoneLine("  Anna "),
        )
    }

    @Test
    fun `the line says it without a name otherwise`() {
        assertEquals(UiText("call.ended.for_all"), endedForEveryoneLine(null))
        assertEquals(endedForEveryoneLine(null), endedForEveryoneLine("  "))
        assertEquals(endedForEveryoneLine(null), endedForEveryoneLine(""))
    }

    // ---- The starter in the ledger --------------------------------------------

    private val startBody = FfiCallSignalingBody.GroupStart(
        callId = callId,
        initiatorLeaf = 4u,
        createdAtMs = t0.toULong(),
        media = FfiCallMediaFlags(audio = true, video = false),
    )

    @Test
    fun `a received start records the person its MLS sender resolved to`() {
        val signal = groupCallSignalForRow(startBody, senderLeaf = 4u, senderKey = "anna-phone", senderPersonKey = starter)!!
        val made = reduceGroupCall(null, signal, t0, t0)!!
        assertEquals(starter, made.initiatorPersonKey)
    }

    @Test
    fun `a start whose person did not resolve records the sending device`() {
        val signal = groupCallSignalForRow(startBody, senderLeaf = 4u, senderKey = "anna-phone", senderPersonKey = null)!!
        assertEquals("anna-phone", reduceGroupCall(null, signal, t0, t0)!!.initiatorPersonKey)
    }

    @Test
    fun `a start with no resolved sender leaves the starter unknown`() {
        val signal = groupCallSignalForRow(startBody, senderLeaf = 4u)!!
        assertNull(reduceGroupCall(null, signal, t0, t0)!!.initiatorPersonKey)
        // The leaf the body names is never taken for the starter.
        assertNull(groupCallSignalOf(startBody).let { it as GroupCallSignal.Start }.initiatorPersonKey)
    }

    @Test
    fun `a start this device published records its own person key`() {
        val own = groupCallSignalOf(startBody)!!.withSender("own-phone", "own-person")
        assertEquals("own-person", reduceGroupCall(null, own, t0, t0)!!.initiatorPersonKey)
    }

    @Test
    fun `a re-delivered start from someone else does not replace the starter`() {
        val first = reduceGroupCall(null, groupCallSignalOf(startBody)!!.withSender("anna-phone", starter), t0, t0)!!
        val again = reduceGroupCall(first, groupCallSignalOf(startBody)!!.withSender("mallory", other), t0, t0)
        assertSame(first, again)
    }

    @Test
    fun `joins and leaves keep the starter`() {
        val made = reduceGroupCall(null, groupCallSignalOf(startBody)!!.withSender("anna-phone", starter), t0, t0)!!
        val joined = reduceGroupCall(made, GroupCallSignal.Join(callHex, 2u, "bob-phone", "bob"), t0, t0)!!
        val left = reduceGroupCall(joined, GroupCallSignal.Leave(callHex, 2u, "bob-phone", "bob"), t0, t0)!!
        assertEquals(starter, joined.initiatorPersonKey)
        assertEquals(starter, left.initiatorPersonKey)
    }

    // ---- Persistence ----------------------------------------------------------

    @Test
    fun `the starter survives the store round trip`() {
        val store = InMemoryGroupCallStore()
        val rec = record()
        store.save("acct", groupA, rec.toRow())
        val seeded = groupCallRecordsToSeed(store.loadAccount("acct"), nowMs = t0 + 5_000L)
        assertEquals(rec, seeded[groupA])
        assertEquals(starter, seeded[groupA]!!.initiatorPersonKey)
        assertEquals(rec.toRow(), decodeGroupCallRow(encodeGroupCallRow(rec.toRow())))
    }

    @Test
    fun `a stored record without a starter reads as unknown and can never be ended for everyone`() {
        // A row written before the starter was recorded.
        val legacy = """{"callIdHex":"$callHex","audio":true,"video":false,"initiatorLeaf":4,""" +
            """"startedAtMs":$t0,"joinedMembers":{},"lastActivityMs":$t0}"""
        val restored = decodeGroupCallRow(legacy)!!.toRecord()!!
        assertNull(restored.initiatorPersonKey)
        assertNull(groupCallEndedForEveryoneBy(restored, end(FfiCallEndReason.ENDED_FOR_ALL), starter))
        assertFalse(groupCallStartedBy(restored, callHex, starter))

        // An empty stored starter is unknown too, not a key that matches "".
        val empty = record().toRow().copy(initiatorPersonKey = "").toRecord()!!
        assertNull(empty.initiatorPersonKey)
        assertNull(groupCallEndedForEveryoneBy(empty, end(FfiCallEndReason.ENDED_FOR_ALL), ""))
    }

    // ---- Who started the call -------------------------------------------------

    @Test
    fun `the call is this account's when the record names its person key`() {
        assertTrue(groupCallStartedBy(record(), callHex, starter))
        assertFalse(groupCallStartedBy(record(), callHex, other))
        assertFalse(groupCallStartedBy(record(), callHex, null))
        assertFalse("another call's record", groupCallStartedBy(record(), "11".repeat(16), starter))
        assertFalse(groupCallStartedBy(null, callHex, starter))
        assertFalse(groupCallStartedBy(record(initiatorPersonKey = null), callHex, null))
    }

    // ---- A received end, judged against the record ----------------------------

    @Test
    fun `only the starter's end for everyone names a starter to act on`() {
        assertEquals(starter, groupCallEndedForEveryoneBy(record(), end(FfiCallEndReason.ENDED_FOR_ALL), starter))
        assertNull(groupCallEndedForEveryoneBy(record(), end(FfiCallEndReason.ENDED_FOR_ALL), other))
        assertNull(groupCallEndedForEveryoneBy(record(), end(FfiCallEndReason.ENDED_FOR_ALL), null))
        assertNull(groupCallEndedForEveryoneBy(record(), end(FfiCallEndReason.HUNG_UP), starter))
        assertNull(groupCallEndedForEveryoneBy(record(), end(FfiCallEndReason.ERROR), starter))
    }

    @Test
    fun `an end for another call, or with no record, takes nobody out`() {
        val elsewhere = end(FfiCallEndReason.ENDED_FOR_ALL, ByteArray(16) { 0x11 })
        assertNull(groupCallEndedForEveryoneBy(record(), elsewhere, starter))
        assertNull(groupCallEndedForEveryoneBy(null, end(FfiCallEndReason.ENDED_FOR_ALL), starter))
    }

    @Test
    fun `only an End can end a call for everyone`() {
        val leave = FfiCallSignalingBody.GroupLeave(callId, 4u, FfiCallEndReason.ENDED_FOR_ALL)
        assertNull(groupCallEndedForEveryoneBy(record(), leave, starter))
        assertNull(groupCallEndedForEveryoneBy(record(), startBody, starter))
    }

    @Test
    fun `an end from anyone still closes the record as it does today`() {
        val closed = reduceGroupCall(record(), groupCallSignalOf(end(FfiCallEndReason.ENDED_FOR_ALL))!!, t0, t0)
        assertNull(closed)
    }

    // ---- Link call ------------------------------------------------------------

    @Test
    fun `a sealed message is an end for everyone only as an End of this call with that reason`() {
        assertTrue(isEndForEveryone(end(FfiCallEndReason.ENDED_FOR_ALL), callId))
        assertFalse(isEndForEveryone(end(FfiCallEndReason.HUNG_UP), callId))
        assertFalse(isEndForEveryone(end(FfiCallEndReason.ENDED_FOR_ALL, ByteArray(16) { 0x11 }), callId))
        assertFalse(isEndForEveryone(startBody, callId))
        assertFalse("not call signaling at all", isEndForEveryone(null, callId))
    }

    @Test
    fun `only the leaf holding the capability's owner key speaks for the owner`() {
        val ownerKeyHex = "0a".repeat(32)
        assertTrue(linkCallSenderIsOwner(ownerKeyHex, ownerKeyHex))
        assertFalse(linkCallSenderIsOwner("0b".repeat(32), ownerKeyHex))
        assertFalse("an unresolved sender leaf", linkCallSenderIsOwner(null, ownerKeyHex))
        assertFalse(linkCallSenderIsOwner("", ""))
    }

    // ---- The notice -----------------------------------------------------------

    @Test
    fun `the notice shows the line for a few seconds and then takes it down`() = runTest {
        val notice = EndedForEveryoneNotice(backgroundScope)
        assertNull(notice.line.value)

        notice.show("Anna")
        runCurrent()
        assertEquals(endedForEveryoneLine("Anna"), notice.line.value)

        advanceTimeBy(ENDED_FOR_EVERYONE_NOTICE_MS - 1)
        runCurrent()
        assertEquals(endedForEveryoneLine("Anna"), notice.line.value)

        advanceTimeBy(1)
        runCurrent()
        assertNull(notice.line.value)
    }

    @Test
    fun `a second end inside the window restarts the hold, and a reset clears it`() = runTest {
        val notice = EndedForEveryoneNotice(backgroundScope)
        notice.show("Anna")
        advanceTimeBy(ENDED_FOR_EVERYONE_NOTICE_MS - 1_000L)
        notice.show(null)
        advanceTimeBy(ENDED_FOR_EVERYONE_NOTICE_MS - 1)
        runCurrent()
        assertEquals("the first timer took the second line down", endedForEveryoneLine(null), notice.line.value)

        notice.reset()
        assertNull(notice.line.value)
    }
}
