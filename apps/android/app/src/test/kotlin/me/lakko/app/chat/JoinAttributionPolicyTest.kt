// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Test

/** ADR-0053 — when the members sheet may re-read the join attributions. */
class JoinAttributionPolicyTest {
    private val now = 1_000_000L

    @Test
    fun `an empty pending set clears with no fetch`() {
        assertEquals(JoinAttributionDecision.Clear, joinAttributionDecision(emptySet(), setOf("a"), null, now))
        assertEquals(JoinAttributionDecision.Clear, joinAttributionDecision(emptySet(), null, now - 1, now))
    }

    @Test
    fun `the same pending set never refetches, however long ago`() {
        assertEquals(JoinAttributionDecision.Keep, joinAttributionDecision(setOf("a", "b"), setOf("b", "a"), now - 1, now))
        assertEquals(JoinAttributionDecision.Keep, joinAttributionDecision(setOf("a"), setOf("a"), now - 3_600_000, now))
    }

    @Test
    fun `a changed set fetches when the group was not read in the last 10 s`() {
        assertEquals(JoinAttributionDecision.Fetch, joinAttributionDecision(setOf("a"), null, null, now))
        assertEquals(JoinAttributionDecision.Fetch, joinAttributionDecision(setOf("a", "b"), setOf("a"), now - 10_000, now))
    }

    @Test
    fun `a changed set inside the floor is deferred to the end of the window`() {
        assertEquals(
            JoinAttributionDecision.Defer(6_000),
            joinAttributionDecision(setOf("a", "b"), setOf("a"), now - 4_000, now),
        )
    }

    @Test
    fun `a clock that went backwards defers a full window`() {
        assertEquals(
            JoinAttributionDecision.Defer(JOIN_ATTRIBUTION_MIN_INTERVAL_MS),
            joinAttributionDecision(setOf("a"), null, now + 5_000, now),
        )
    }

    @Test
    fun `a stream of refreshes with one pending request reads once`() {
        var fetchedFor: Set<String>? = null
        var last: Long? = null
        var reads = 0
        // 45 refreshes over a minute, the iOS finding's rate.
        for (i in 0 until 45) {
            val t = now + i * 1_333L
            if (joinAttributionDecision(setOf("a"), fetchedFor, last, t) == JoinAttributionDecision.Fetch) {
                reads++
                fetchedFor = setOf("a")
                last = t
            }
        }
        assertEquals(1, reads)
    }
}
