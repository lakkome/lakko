// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import java.io.IOException
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0074 section 3.5, "Channels" - the owner-key handover of a rotation:
 * what each answer of `PUT /v1/channels/{id}/owner` means, that a channel
 * without a verdict keeps the step owed, and which identity owner requests
 * are derived from afterwards. Fakes only; no server, no native core.
 */
class ChannelOwnerHandoverTest {

    private fun api(status: Int, code: String) = ChannelsApiException(status, code, "detail")

    @Test
    fun `the answer to the handover signed by the old key`() {
        assertEquals(AfterOldOwnerKey.OWNED, afterOldOwnerKey(null))
        // The old key no longer owns the channel: ask with the new one.
        assertEquals(AfterOldOwnerKey.PROBE_WITH_NEW_KEY, afterOldOwnerKey(api(401, "owner-proof-required")))
        assertEquals(AfterOldOwnerKey.PROBE_WITH_NEW_KEY, afterOldOwnerKey(api(409, "owner-changed")))
        // Not verdicts about ownership: owed.
        assertEquals(AfterOldOwnerKey.RETRY, afterOldOwnerKey(api(400, "bad-owner-pubkey")))
        assertEquals(AfterOldOwnerKey.RETRY, afterOldOwnerKey(api(429, "rate-limited")))
        assertEquals(AfterOldOwnerKey.RETRY, afterOldOwnerKey(api(503, "unknown")))
        assertEquals(AfterOldOwnerKey.RETRY, afterOldOwnerKey(IOException("down")))
    }

    @Test
    fun `the answer to the probe signed by the new key`() {
        assertEquals(ChannelHandoverOutcome.OWNED, afterNewOwnerKey(null))
        assertEquals(ChannelHandoverOutcome.NOT_OWNER, afterNewOwnerKey(api(401, "owner-proof-required")))
        // A conflict on the probe is no verdict: the owner moved again.
        assertEquals(ChannelHandoverOutcome.RETRY, afterNewOwnerKey(api(409, "owner-changed")))
        assertEquals(ChannelHandoverOutcome.RETRY, afterNewOwnerKey(api(400, "bad-owner-pubkey")))
        assertEquals(ChannelHandoverOutcome.RETRY, afterNewOwnerKey(api(500, "unknown")))
        assertEquals(ChannelHandoverOutcome.RETRY, afterNewOwnerKey(IOException("down")))
    }

    @Test
    fun `a handover the old key lands needs no probe`() = runTest {
        val signedWithNew = mutableListOf<Boolean>()
        val outcome = handOverChannelOwner { withNew -> signedWithNew += withNew }
        assertEquals(ChannelHandoverOutcome.OWNED, outcome)
        assertEquals(listOf(false), signedWithNew)
    }

    @Test
    fun `a handover that already landed is found by the probe under the new key`() = runTest {
        for (first in listOf(api(401, "owner-proof-required"), api(409, "owner-changed"))) {
            val signedWithNew = mutableListOf<Boolean>()
            val outcome = handOverChannelOwner { withNew ->
                signedWithNew += withNew
                if (!withNew) throw first
            }
            assertEquals(ChannelHandoverOutcome.OWNED, outcome)
            assertEquals(listOf(false, true), signedWithNew)
        }
    }

    @Test
    fun `a channel neither key owns is not this account's`() = runTest {
        val outcome = handOverChannelOwner { throw api(401, "owner-proof-required") }
        assertEquals(ChannelHandoverOutcome.NOT_OWNER, outcome)
    }

    @Test
    fun `no answer is never read as handed over`() = runTest {
        // Offline on the first request: the probe is not even tried.
        var calls = 0
        assertEquals(
            ChannelHandoverOutcome.RETRY,
            handOverChannelOwner {
                calls += 1
                throw IOException("down")
            },
        )
        assertEquals(1, calls)
        // Offline on the probe.
        assertEquals(
            ChannelHandoverOutcome.RETRY,
            handOverChannelOwner { withNew -> throw if (withNew) IOException("down") else api(409, "owner-changed") },
        )
        // A malformed key is a failure of this client, not a verdict.
        assertEquals(ChannelHandoverOutcome.RETRY, handOverChannelOwner { throw api(400, "bad-owner-pubkey") })
    }

    @Test
    fun `the step is settled only when every channel has a verdict`() {
        assertTrue(channelHandoversSettled(emptyList()))
        assertTrue(channelHandoversSettled(listOf(ChannelHandoverOutcome.OWNED, ChannelHandoverOutcome.NOT_OWNER)))
        assertFalse(channelHandoversSettled(listOf(ChannelHandoverOutcome.OWNED, ChannelHandoverOutcome.RETRY)))
    }

    @Test
    fun `one channel without a verdict keeps the step owed and the others are still handed over`() = runTest {
        val attempted = mutableListOf<String>()
        val settled = handOverChannels(
            channelIds = listOf("aa", "bb", "cc"),
            handOverOne = { id ->
                attempted += id
                if (id == "bb") ChannelHandoverOutcome.RETRY else ChannelHandoverOutcome.OWNED
            },
        )
        assertFalse(settled)
        assertEquals(listOf("aa", "bb", "cc"), attempted)
        // The retry lands: every channel is asked again (a handover naming
        // the key that already owns the channel changes nothing).
        attempted.clear()
        assertTrue(
            handOverChannels(
                channelIds = listOf("aa", "bb", "cc"),
                handOverOne = { id ->
                    attempted += id
                    ChannelHandoverOutcome.OWNED
                },
            ),
        )
        assertEquals(listOf("aa", "bb", "cc"), attempted)
    }

    @Test
    fun `owner requests follow the current key only for the conversation identity a rotation succeeded`() {
        val conversation = ByteArray(32) { 1 }
        val other = ByteArray(32) { 2 }
        // Never rotated: the identity asked is the one that signs.
        assertFalse(ownerKeySucceeds(conversation, conversationKey = null))
        // Rotated: the conversation identity's owner requests move.
        assertTrue(ownerKeySucceeds(conversation.copyOf(), conversationKey = conversation))
        // Any other identity (the old or the new one handed in by the
        // handover itself) signs as itself.
        assertFalse(ownerKeySucceeds(other, conversationKey = conversation))
    }

    // ---- 2026-11-20: the owner key repair after a refused owner proof ----

    private val refused = ChannelsApiException(401, "owner-proof-required", "")

    @Test
    fun `the repair tries the predecessors newest first, without the current seed`() {
        val a = ByteArray(64) { 1 }
        val b = ByteArray(64) { 2 }
        val c = ByteArray(64) { 3 }
        val out = ChannelOwnerRepair.candidates(listOf(a, b, b, c), c)
        assertEquals(2, out.size)
        assertTrue(out[0].contentEquals(b))
        assertTrue(out[1].contentEquals(a))
    }

    @Test
    fun `the repair hands the channel from the predecessor that owns it to the current key`() = runTest {
        var owner = "oldest"
        var puts = 0
        val outcome = ChannelOwnerRepair.repair(listOf("newer", "oldest"), "current", { x, y -> x == y }) { signing, naming ->
            puts += 1
            if (signing != owner) throw refused
            owner = naming
        }
        assertEquals(ChannelHandoverOutcome.OWNED, outcome)
        assertEquals("current", owner)
        assertEquals(3, puts)
        assertEquals(
            ChannelHandoverOutcome.NOT_OWNER,
            ChannelOwnerRepair.repair(listOf("oldest"), "current", { x, y -> x == y }) { _, _ -> throw refused },
        )
        assertEquals(
            ChannelHandoverOutcome.RETRY,
            ChannelOwnerRepair.repair(listOf("oldest"), "current", { x, y -> x == y }) { _, _ -> throw IOException("offline") },
        )
    }

    @Test
    fun `the epoch mapping is re-anchored only when the group ran more than one epoch ahead`() {
        assertEquals(null, reanchoredOffset(5uL, 0L, 5u))
        assertEquals(null, reanchoredOffset(6uL, 0L, 5u))
        assertEquals(null, reanchoredOffset(2uL, 8L, 9u))
        assertEquals(-153L, reanchoredOffset(160uL, 0L, 6u))
        assertEquals(7u, channelEpochFor(160uL, -153L))
    }
}

