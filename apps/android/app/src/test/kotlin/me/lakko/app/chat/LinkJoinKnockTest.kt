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
}
