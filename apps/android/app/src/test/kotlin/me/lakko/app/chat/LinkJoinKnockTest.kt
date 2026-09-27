// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0053 section 4 / client contract point 6 — the link joiner's two
 * knocks: a probe that learns the epoch, then the attribution and the
 * proposal built for that epoch, in that order. No journal read exists in
 * the sequence to begin with.
 */
class LinkJoinKnockTest {

    @Test
    fun `the probe learns the epoch the proposal is built for`() = runTest {
        val knocks = mutableListOf<List<ByteArray>>()
        val builtFor = mutableListOf<ULong>()
        val epoch = runLinkJoinKnock(
            knock = { payloads ->
                knocks += payloads
                17uL
            },
            buildProposal = { e ->
                builtFor += e
                byteArrayOf(0x50)
            },
            buildAttribution = { byteArrayOf(0x41) },
        )
        assertEquals(17uL, epoch)
        assertEquals(listOf(17uL), builtFor)
        assertEquals(2, knocks.size)
        assertTrue("the first knock is an empty probe", knocks[0].isEmpty())
        assertEquals(2, knocks[1].size)
        assertArrayEquals("attribution first", byteArrayOf(0x41), knocks[1][0])
        assertArrayEquals("then the proposal", byteArrayOf(0x50), knocks[1][1])
    }

    @Test
    fun `the proposal is built only after the probe answered`() = runTest {
        val order = mutableListOf<String>()
        runLinkJoinKnock(
            knock = { payloads ->
                order += if (payloads.isEmpty()) "probe" else "knock"
                1uL
            },
            buildProposal = {
                order += "proposal"
                byteArrayOf(1)
            },
            buildAttribution = { null },
        )
        assertEquals(listOf("probe", "proposal", "knock"), order)
    }

    @Test
    fun `a failed attribution does not stop the request`() = runTest {
        val knocks = mutableListOf<List<ByteArray>>()
        runLinkJoinKnock(
            knock = { payloads ->
                knocks += payloads
                3uL
            },
            buildProposal = { byteArrayOf(9) },
            buildAttribution = { throw IllegalStateException("seed unavailable") },
        )
        assertEquals(1, knocks[1].size)
        assertArrayEquals(byteArrayOf(9), knocks[1][0])
    }

    @Test
    fun `an unclaimed group builds no proposal and stores nothing`() {
        val knocks = mutableListOf<List<ByteArray>>()
        var built = false
        assertThrows(GroupNotAcceptingRequests::class.java) {
            runBlocking {
                runLinkJoinKnock(
                    knock = { payloads ->
                        knocks += payloads
                        null
                    },
                    buildProposal = {
                        built = true
                        byteArrayOf(1)
                    },
                    buildAttribution = { byteArrayOf(2) },
                )
            }
        }
        assertEquals("only the probe went out", 1, knocks.size)
        assertTrue(knocks[0].isEmpty())
        assertEquals(false, built)
    }

    // ------------------------------------------------ expired proof of work

    private fun expired() = me.lakko.app.ds.DsHttpError(401, "/ds/group/x/knock", "pow")

    @Test
    fun `a knock refused because its pow expired is retried once with a fresh challenge`() = runTest {
        val knocks = mutableListOf<List<ByteArray>>()
        var failSecondKnock = true
        runLinkJoinKnock(
            knock = { payloads ->
                knocks += payloads
                // Each call models fetch-challenge, solve, POST: the retry is
                // a whole new call, so it solves a new challenge.
                if (payloads.isNotEmpty() && failSecondKnock) {
                    failSecondKnock = false
                    throw expired()
                }
                5uL
            },
            buildProposal = { byteArrayOf(0x50) },
            buildAttribution = { null },
        )
        assertEquals(3, knocks.size)
        assertTrue(knocks[0].isEmpty())
        assertArrayEquals(knocks[1][0], knocks[2][0])
    }

    @Test
    fun `an expired probe is retried once too`() = runTest {
        var calls = 0
        val epoch = runLinkJoinKnock(
            knock = { payloads ->
                calls++
                if (calls == 1) throw expired()
                if (payloads.isEmpty()) 9uL else 9uL
            },
            buildProposal = { byteArrayOf(1) },
            buildAttribution = { null },
        )
        assertEquals(9uL, epoch)
        assertEquals(3, calls)
    }

    @Test
    fun `a second expiry on the same knock propagates`() {
        var calls = 0
        assertThrows(me.lakko.app.ds.DsHttpError::class.java) {
            runBlocking {
                runLinkJoinKnock(
                    knock = { payloads ->
                        calls++
                        if (payloads.isNotEmpty()) throw expired()
                        1uL
                    },
                    buildProposal = { byteArrayOf(1) },
                    buildAttribution = { null },
                )
            }
        }
        assertEquals("probe + knock + one retry", 3, calls)
    }

    @Test
    fun `a failure that is not an expired pow is not retried`() {
        var calls = 0
        assertThrows(IllegalStateException::class.java) {
            runBlocking {
                runLinkJoinKnock(
                    knock = {
                        calls++
                        throw IllegalStateException("offline")
                    },
                    buildProposal = { byteArrayOf(1) },
                    buildAttribution = { null },
                )
            }
        }
        assertEquals(1, calls)
        assertTrue(isKnockPowExpired(expired()))
        assertTrue(!isKnockPowExpired(me.lakko.app.ds.DsHttpError(429, "/x", "")))
    }

    // ------------------------------------------------ join screen copy

    @Test
    fun `the join failure copy is one of the fixed sentences`() {
        assertEquals(
            "This group isn't taking requests yet. Try again later.",
            joinFailureCopy(ChatError(GROUP_NOT_ACCEPTING_REQUESTS, GroupNotAcceptingRequests())),
        )
        assertEquals(
            "Check this device's date and time.",
            joinFailureCopy(ChatError("x", me.lakko.app.ds.DsProofRefusedError("/ds/x", clockSkewed = true, body = ""))),
        )
        val generic = "Your request didn't go through. Check your connection and try again."
        assertEquals(generic, joinFailureCopy(ChatError("DS knock (external-join request) failed", expired())))
        assertEquals(generic, joinFailureCopy(me.lakko.app.ds.DsProofRefusedError("/ds/x", clockSkewed = false, body = "")))
        assertEquals(generic, joinFailureCopy(RuntimeException("raw protocol text")))
        assertEquals("You joined the group.", JOIN_SUCCESS_COPY)
        assertEquals("Open chat", JOIN_OPEN_CHAT)
        assertEquals("Try again", JOIN_TRY_AGAIN)
    }

    // ------------------------------------------------ pending request label

    @Test
    fun `a pending request is named through the joiner's account key`() {
        val leaf = ByteArray(32) { 1 }
        val account = ByteArray(32) { 2 }
        val label = pendingJoinLabel(
            accountKey = account,
            leafKey = leaf,
            knownName = { key -> if (key.contentEquals(account)) "Anna" else null },
            fallbackLabel = { "Member" },
        )
        assertEquals("Anna", label)
    }

    @Test
    fun `a joiner unknown under the account key keeps the existing label`() {
        val leaf = ByteArray(32) { 1 }
        assertEquals(
            "ab12 cd34",
            pendingJoinLabel(ByteArray(32) { 3 }, leaf, knownName = { null }, fallbackLabel = { "ab12 cd34" }),
        )
        assertEquals(
            "ab12 cd34",
            pendingJoinLabel(null, leaf, knownName = { "never" }, fallbackLabel = { "ab12 cd34" }),
        )
    }

    @Test
    fun `only the general failure offers Try again`() {
        assertTrue(joinFailureOffersRetry(JOIN_FAILED_COPY))
        assertTrue(!joinFailureOffersRetry(GROUP_NOT_ACCEPTING_REQUESTS))
        assertTrue(!joinFailureOffersRetry(me.lakko.app.ds.DS_CLOCK_SKEW_COPY))
        assertTrue(!joinFailureOffersRetry(joinFailureCopy(GroupNotAcceptingRequests())))
        assertTrue(joinFailureOffersRetry(joinFailureCopy(RuntimeException("offline"))))
    }
}
