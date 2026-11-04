// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** who-can-find-me.md 6.1 — a text asked in the last 60 seconds is not asked again. */
class RecentAnswersTest {

    @Test
    fun `the same text is asked once within the window and again after it`() = runBlocking {
        var clock = 1_000L
        val memo = RecentAnswers<Boolean>(ttlMs = 60_000L, now = { clock })
        var asked = 0
        val ask: suspend () -> Boolean = { asked += 1; true }
        memo.getOrAsk("hiking", ask)
        clock += 59_999L
        memo.getOrAsk("hiking", ask)
        assertEquals(1, asked)
        clock += 1L
        memo.getOrAsk("hiking", ask)
        assertEquals(2, asked)
        memo.getOrAsk("sailing", ask)
        assertEquals(3, asked)
    }

    @Test
    fun `a failed question is not kept`() = runBlocking {
        val memo = RecentAnswers<Boolean>(now = { 0L })
        try {
            memo.getOrAsk("x") { throw IllegalStateException("429") }
        } catch (_: IllegalStateException) {
        }
        assertNull(memo.get("x"))
    }

    @Test
    fun `the oldest answer leaves first when full`() {
        val memo = RecentAnswers<Int>(capacity = 2, now = { 0L })
        memo.put("a", 1)
        memo.put("b", 2)
        memo.put("c", 3)
        assertNull(memo.get("a"))
        assertEquals(3, memo.get("c"))
    }

    @Test
    fun `a 429 from discovery reads as the request limit, never as could not save`() {
        val copy = me.lakko.app.chat.handleWriteFailure(DiscoveryHttpError(429, "/discovery/oprf", "{}"))
        assertEquals(me.lakko.app.chat.HANDLE_RATE_LIMITED, copy)
    }
}
