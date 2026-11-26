// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.account.AccountStoreGate
import me.lakko.app.auth.MarkerStorage
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/** ADR-0101 section 9 - who answers a history request, and how the
 *  requester pages, newest first. Mirror of
 *  `apps/web/src/chat/history-return.test.ts` and
 *  `apps/web/src/devices/history-page.test.ts`. */
class HistoryReturnTest {

    private class MapStorage : MarkerStorage {
        val rows = HashMap<String, String>()
        override fun get(key: String): String? = rows[key]
        override fun set(key: String, value: String) {
            rows[key] = value
        }
        override fun remove(key: String) {
            rows.remove(key)
        }
    }

    @Before
    fun setUp() = AccountStoreGate.resetForTest()

    @After
    fun tearDown() = AccountStoreGate.resetForTest()

    private fun allowed(
        fromSelf: Boolean = false,
        accepted: Boolean = true,
        wasIn: Boolean = true,
        held: Boolean = true,
        kind: ConversationKind = ConversationKind.Chat,
        admin: Boolean = false,
    ) = historyAnswerAllowed(fromSelf, accepted, wasIn, held, kind, admin)

    @Test
    fun `the other person of a one-to-one answers`() {
        assertTrue(allowed())
    }

    @Test
    fun `an owner or admin of a group answers, not a plain member`() {
        assertTrue(allowed(kind = ConversationKind.Group, admin = true))
        assertFalse(allowed(kind = ConversationKind.Group, admin = false))
    }

    @Test
    fun `never to itself, a person whose reset was not accepted, or someone not in the earlier group`() {
        assertFalse(allowed(fromSelf = true))
        assertFalse(allowed(accepted = false))
        assertFalse(allowed(wasIn = false))
        assertFalse(allowed(held = false))
    }

    @Test
    fun `paging moves before to the oldest id of each page and stops at an empty page`() {
        val one = nextHistoryStep(FIRST_HISTORY_REQUEST, listOf(900L, 500L, 700L))
        assertEquals(HistoryRequestState(before = "500", pages = 1, sentAtMs = 0L, asks = 0, done = false), one)
        val two = nextHistoryStep(one, listOf(499L, 300L))
        assertEquals("300", two.before)
        assertTrue(nextHistoryStep(two, emptyList()).done)
    }

    @Test
    fun `paging stops on a page that does not go older, and at the page cap`() {
        val s = HistoryRequestState(before = "300", pages = 3, sentAtMs = 1L, done = false)
        assertTrue(nextHistoryStep(s, listOf(300L, 400L)).done)
        assertTrue(nextHistoryStep(s.copy(pages = HISTORY_MAX_PAGES - 1), listOf(10L)).done)
        // A done state stays as it is.
        val done = s.copy(done = true)
        assertEquals(done, nextHistoryStep(done, listOf(1L)))
    }

    @Test
    fun `a request is posted once, again after the resend time, never when done`() {
        assertTrue(historyRequestDue(FIRST_HISTORY_REQUEST, 5L))
        val sent = FIRST_HISTORY_REQUEST.copy(sentAtMs = 1000L)
        assertFalse(historyRequestDue(sent, 1000L + HISTORY_RESEND_MS - 1))
        assertTrue(historyRequestDue(sent, 1000L + HISTORY_RESEND_MS))
        assertFalse(historyRequestDue(sent.copy(done = true), Long.MAX_VALUE))
        // A clock that moved back reads as due.
        assertTrue(historyRequestDue(sent, 500L))
    }

    @Test
    fun `asking stops after the ask cap with no answer, and an answered page asks afresh`() {
        var s = FIRST_HISTORY_REQUEST
        for (i in 0 until HISTORY_MAX_ASKS) {
            val t = (i + 1) * HISTORY_RESEND_MS * 2
            assertTrue(historyRequestDue(s, t))
            s = historyAsked(s, t)
        }
        assertFalse(historyRequestDue(s, Long.MAX_VALUE))
        assertTrue(historyRequestDue(nextHistoryStep(s, listOf(7L)), Long.MAX_VALUE))
    }

    @Test
    fun `the states start once and round-trip per account`() {
        val store = HistoryReturnStore(MapStorage())
        store.noteReturned("a", "g")
        store.put("a", "g", HistoryRequestState(before = "5", pages = 2, sentAtMs = 3L, done = false))
        store.noteReturned("a", "g")
        assertEquals(mapOf("g" to HistoryRequestState(before = "5", pages = 2, sentAtMs = 3L, asks = 0, done = false)), store.all("a"))
        assertNull(store.get("b", "g"))
        store.clear("a")
        assertTrue(store.all("a").isEmpty())
    }

    @Test
    fun `a page is the newest rows below before, oldest first, unparseable ids left out`() {
        val ids = listOf(10L, 3L, 7L, null, 0L, 12L, 5L)
        val idOf: (Long?) -> Long? = { it }
        assertEquals(listOf(7L, 10L, 12L), historyPageRows(ids, idOf, 0L, 3))
        assertEquals(listOf(3L, 5L), historyPageRows(ids, idOf, 7L, 10))
        assertEquals(emptyList<Long?>(), historyPageRows(ids, idOf, 3L, 10))
        assertEquals(emptyList<Long?>(), historyPageRows(ids, idOf, 0L, 0))
    }

    @Test
    fun `a history request is recognised by its domain and fixed length only`() {
        val good = HISTORY_REQUEST_ENVELOPE_DOMAIN + byteArrayOf(1) + ByteArray(32) + ByteArray(8)
        assertTrue(looksLikeHistoryRequestEnvelope(good))
        assertFalse(looksLikeHistoryRequestEnvelope(good.copyOf(good.size - 1)))
        val other = good.copyOf().also { it[0] = 'x'.code.toByte() }
        assertFalse(looksLikeHistoryRequestEnvelope(other))
        assertEquals(66, HISTORY_REQUEST_ENVELOPE_LEN)
    }
}
