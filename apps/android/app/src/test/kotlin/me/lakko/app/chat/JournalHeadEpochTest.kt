// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class JournalHeadEpochTest {
    // A "payload" here is one byte: 0 = not MLS, n = a row after which the
    // group is at epoch n. That is exactly the contract of `epochAfterMessage`.
    private fun row(id: Long, epoch: Int) = JournalHeadEpoch.Row(id, byteArrayOf(epoch.toByte()))

    private fun epochAfter(payload: ByteArray): ULong? =
        payload.firstOrNull()?.let { if (it.toInt() == 0) null else it.toULong() }

    @Test
    fun pagesToTheHeadInsteadOfTrustingTheFirstPage() = runTest {
        val pages = ArrayDeque(
            listOf(
                JournalHeadEpoch.Page(listOf(row(1, 1), row(2, 0), row(3, 2)), hasMore = true),
                JournalHeadEpoch.Page(listOf(row(4, 2), row(5, 0)), hasMore = true),
                JournalHeadEpoch.Page(listOf(row(6, 7)), hasMore = false),
            ),
        )
        val calls = mutableListOf<Pair<Long, Int>>()
        val epoch = JournalHeadEpoch.read(
            fetchPage = { after, limit ->
                calls += after to limit
                pages.removeFirstOrNull() ?: JournalHeadEpoch.Page(emptyList(), hasMore = false)
            },
            epochAfter = ::epochAfter,
        )
        assertEquals(7uL, epoch)
        assertEquals(listOf(0L, 3L, 5L), calls.map { it.first })
        assertEquals(setOf(JournalHeadEpoch.PAGE_SIZE), calls.map { it.second }.toSet())
    }

    @Test
    fun keepsTheLastMlsEpochWhenTheHeadRowsAreNotMls() = runTest {
        val epoch = JournalHeadEpoch.read(
            fetchPage = { _, _ -> JournalHeadEpoch.Page(listOf(row(1, 4), row(2, 0), row(3, 0)), hasMore = false) },
            epochAfter = ::epochAfter,
        )
        assertEquals(4uL, epoch)
    }

    @Test
    fun readsAnEmptyJournalAsEpochZero() = runTest {
        val epoch = JournalHeadEpoch.read(
            fetchPage = { _, _ -> JournalHeadEpoch.Page(emptyList(), hasMore = false) },
            epochAfter = ::epochAfter,
        )
        assertEquals(0uL, epoch)
    }

    @Test
    fun stopsWhenAPageDoesNotMoveTheCursor() = runTest {
        var calls = 0
        val epoch = JournalHeadEpoch.read(
            fetchPage = { _, _ ->
                calls += 1
                JournalHeadEpoch.Page(listOf(row(5, 3)), hasMore = true)
            },
            epochAfter = ::epochAfter,
        )
        assertEquals(3uL, epoch)
        assertEquals(2, calls)
    }

    @Test
    fun propagatesAFetchFailureInsteadOfAnsweringWithAStaleEpoch() = runTest {
        class Offline : RuntimeException("offline")
        try {
            JournalHeadEpoch.read(
                fetchPage = { _, _ -> throw Offline() },
                epochAfter = ::epochAfter,
            )
            fail("expected the fetch error to propagate")
        } catch (e: Offline) {
            assertTrue(e.message == "offline")
        }
    }

    @Test
    fun scanFindsARowOnALaterPageAndStopsThere() = runTest {
        val pages = ArrayDeque(
            listOf(
                JournalHeadEpoch.Page(listOf(row(1, 1), row(2, 2)), hasMore = true),
                JournalHeadEpoch.Page(listOf(row(3, 9), row(4, 5)), hasMore = true),
                JournalHeadEpoch.Page(listOf(row(5, 6)), hasMore = false),
            ),
        )
        var fetched = 0
        val hit = JournalHeadEpoch.scan(
            fetchPage = { _, _ ->
                fetched += 1
                pages.removeFirstOrNull() ?: JournalHeadEpoch.Page(emptyList(), hasMore = false)
            },
            visit = { row -> if (row.payload[0].toInt() == 9) row.id else null },
        )
        assertEquals(3L, hit)
        assertEquals(2, fetched)
    }

    @Test
    fun scanReturnsNullWhenNothingMatches() = runTest {
        val hit = JournalHeadEpoch.scan<Long>(
            fetchPage = { _, _ -> JournalHeadEpoch.Page(listOf(row(1, 1)), hasMore = false) },
            visit = { null },
        )
        assertEquals(null, hit)
    }
}
