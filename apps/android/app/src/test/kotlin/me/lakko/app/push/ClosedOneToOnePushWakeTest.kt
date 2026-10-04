// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import kotlinx.coroutines.test.runTest
import me.lakko.app.calls.CallRingSignal
import me.lakko.app.calls.TEST_RING_WINDOW_MS
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0065 phase 2 / UX 16.4 on the push path — a wake for a closed
 * one-to-one (the other person reset their account keys) is inert: nothing is
 * fetched, no call rings, no banner is shown.
 */
class ClosedOneToOnePushWakeTest {

    private val accountId = "acct-1"
    private val groupHex = "ab".repeat(32)
    private val groupIdBytes = ByteArray(32) { 0xAB.toByte() }
    private val callIdHex = "11".repeat(16)
    private val now = 1_800_000_000_000L

    private class FakeGroup(private val row: ProcessedRow) : ThrowawayGroup {
        override fun process(payload: ByteArray): ProcessedRow = row
        override fun close() = Unit
    }

    private class FakeDeps(
        val row: ProcessedRow,
        val closed: Boolean,
        val closedThrows: Boolean = false,
    ) : PushDecryptDeps {
        var fetches = 0
        var seedReads = 0
        val closedAsked = mutableListOf<Pair<String, String>>()

        override fun lookupRoute(accountIdB64: String, pushGidHex: String): String? = null
        override fun accountKnowsGroup(accountIdB64: String, groupHex: String): Boolean = true
        override fun loadIdentitySeed(accountIdB64: String): ByteArray {
            seedReads += 1
            return ByteArray(64) { 1 }
        }
        override fun loadSnapshot(accountIdB64: String): ByteArray = ByteArray(8) { 2 }
        override fun openThrowawayGroup(
            seed: ByteArray,
            snapshot: ByteArray,
            groupIdBytes: ByteArray,
        ): ThrowawayGroup = FakeGroup(row)

        override suspend fun dsFetch(
            groupIdBytes: ByteArray,
            afterId: Long,
            signer: me.lakko.app.ds.DsJournalSigner?,
        ): List<FetchedRow> {
            fetches += 1
            return listOf(FetchedRow(10, byteArrayOf(1)))
        }
        override fun startCursor(accountIdB64: String, groupHex: String): Long? = null

        override fun isClosedOneToOne(accountIdB64: String, groupHex: String): Boolean {
            closedAsked += accountIdB64 to groupHex
            if (closedThrows) throw RuntimeException("closed store failure")
            return closed
        }
    }

    private val offer = ProcessedRow(
        ProcessedKind.CALL_SIGNALING,
        "",
        0,
        callSignal = CallRingSignal.Offer(callIdHex, video = false, offeredAtMs = now - 1_000),
    )
    private val chat = ProcessedRow(ProcessedKind.CHAT, "hello", 0)

    private suspend fun wake(d: FakeDeps) = decryptPushWake(
        PushWake(groupHex = groupHex, pointer = 11, accountIdB64 = accountId),
        d,
        nowMs = now,
        ringWindowMs = TEST_RING_WINDOW_MS,
    )

    @Test
    fun `a call offered into a closed one-to-one never rings and nothing is fetched`() = runTest {
        val d = FakeDeps(offer, closed = true)
        val outcome = wake(d)

        assertTrue(outcome.suppressed)
        assertNull(outcome.ring)
        assertNull(outcome.preview)
        assertEquals("the journal of a closed chat is not read", 0, d.fetches)
        assertEquals("decided before the seed is touched", 0, d.seedReads)
        assertEquals(listOf(accountId to groupHex), d.closedAsked)
    }

    @Test
    fun `a message into a closed one-to-one shows no preview and is marked suppressed`() = runTest {
        val outcome = wake(FakeDeps(chat, closed = true))
        assertTrue("suppressed, so the service posts no generic banner either", outcome.suppressed)
        assertNull(outcome.preview)
    }

    @Test
    fun `an open conversation is untouched by the rule`() = runTest {
        val ring = wake(FakeDeps(offer, closed = false))
        assertFalse(ring.suppressed)
        assertEquals(callIdHex, ring.ring?.callIdHex)

        val message = wake(FakeDeps(chat, closed = false))
        assertFalse(message.suppressed)
        assertEquals("hello", message.preview?.body)
    }

    @Test
    fun `a closed-state store that fails does not silence a live conversation`() = runTest {
        val outcome = wake(FakeDeps(offer, closed = true, closedThrows = true))
        assertFalse(outcome.suppressed)
        assertEquals(callIdHex, outcome.ring?.callIdHex)
    }
}
