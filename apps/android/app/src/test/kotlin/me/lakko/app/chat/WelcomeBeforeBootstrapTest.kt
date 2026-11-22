// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Seen live 2026-11-22: the app was launched while a group Welcome waited in
 * the account mailbox, the inbox pass ran before the chat layer finished its
 * bootstrap, the join threw "not bootstrapped" and the cursor moved past the
 * Welcome, so the person never joined the group. A Welcome the chat layer is
 * not ready to join is never consumed: the pass waits for the layer within
 * its budget, and otherwise keeps the Welcome ahead of the cursor.
 */
@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class WelcomeBeforeBootstrapTest {

    @Test
    fun `a join refused because the chat layer is not ready is held, not reported`() {
        val notReady = ChatLayerNotReady("ChatRepository not bootstrapped")
        assertTrue(isChatLayerNotReady(notReady))
        // Found through a wrapper too.
        assertTrue(isChatLayerNotReady(RuntimeException("join failed", notReady)))
        assertEquals(WelcomeOutcome.HELD_CHAT_NOT_READY, welcomeOutcomeOf(notReady))
        assertFalse(welcomeFailureRaisesBanner(notReady, awaitingCoPublishWelcome = false))
        // Typed, never by message: a plain error with the same words is a
        // refusal, which keeps today's behaviour.
        val plain = IllegalStateException("ChatRepository not bootstrapped")
        assertFalse(isChatLayerNotReady(plain))
        assertEquals(WelcomeOutcome.REFUSED, welcomeOutcomeOf(plain))
        assertTrue(welcomeFailureRaisesBanner(plain, awaitingCoPublishWelcome = false))
    }

    @Test
    fun `the chat layer is ready only once bootstrapped for this account`() {
        assertTrue(chatLayerReadyFor(bootstrapped = true, chatAccountB64 = "A", hasIdentity = true, wantedAccountB64 = "A"))
        assertFalse(chatLayerReadyFor(bootstrapped = false, chatAccountB64 = "A", hasIdentity = true, wantedAccountB64 = "A"))
        assertFalse(chatLayerReadyFor(bootstrapped = true, chatAccountB64 = null, hasIdentity = true, wantedAccountB64 = "A"))
        assertFalse(chatLayerReadyFor(bootstrapped = true, chatAccountB64 = "A", hasIdentity = false, wantedAccountB64 = "A"))
        // Bootstrapped for another account (a session switch): not ready.
        assertFalse(chatLayerReadyFor(bootstrapped = true, chatAccountB64 = "B", hasIdentity = true, wantedAccountB64 = "A"))
    }

    @Test
    fun `the cursor stops just before the earliest held Welcome`() {
        // Nothing held: as before, never backward.
        assertEquals(40L, inboxCursorAfterPass(current = 10L, fetched = 40L, heldWelcomeIds = emptyList()))
        assertEquals(50L, inboxCursorAfterPass(current = 50L, fetched = 40L, heldWelcomeIds = emptyList()))
        // The defect: one row, the Welcome, at id 11. The cursor must not
        // reach 11, so the next pass reads it again.
        assertEquals(10L, inboxCursorAfterPass(current = 10L, fetched = 11L, heldWelcomeIds = listOf(11L)))
        // Several held: the earliest decides; rows before it are consumed.
        assertEquals(14L, inboxCursorAfterPass(current = 10L, fetched = 30L, heldWelcomeIds = listOf(20L, 15L)))
        // Never backward, even when a held id is at or below the cursor.
        assertEquals(10L, inboxCursorAfterPass(current = 10L, fetched = 30L, heldWelcomeIds = listOf(5L)))
    }

    @Test
    fun `the pass waits for the bootstrap that finishes within its budget`() = runTest {
        val bootstrapped = MutableStateFlow(false)
        val waited = async { awaitChatLayerReady({ bootstrapped.value }, bootstrapped, timeoutMs = 30_000L) }
        advanceTimeBy(2_000L)
        bootstrapped.value = true
        assertTrue(waited.await())
    }

    @Test
    fun `a bootstrap that does not finish keeps the pass waiting for its whole budget`() = runTest {
        val bootstrapped = MutableStateFlow(false)
        val start = testScheduler.currentTime
        assertFalse(awaitChatLayerReady({ bootstrapped.value }, bootstrapped, timeoutMs = 30_000L))
        // It spent the budget rather than returning at once: a held Welcome
        // is read again once per budget, never in a tight loop.
        assertEquals(30_000L, testScheduler.currentTime - start)
    }

    @Test
    fun `ready at once does not wait, and a zero budget does not wait`() = runTest {
        val ready = MutableStateFlow(true)
        val start = testScheduler.currentTime
        assertTrue(awaitChatLayerReady({ ready.value }, ready, timeoutMs = 30_000L))
        val notReady = MutableStateFlow(false)
        assertFalse(awaitChatLayerReady({ notReady.value }, notReady, timeoutMs = 0L))
        assertEquals(0L, testScheduler.currentTime - start)
    }

    @Test
    fun `a Welcome waiting at launch is joined on the pass after the bootstrap`() = runTest {
        // A model of two inbox passes over one mailbox row (the Welcome, id
        // 11) with the cursor at 10, using the same steps the pass runs.
        val bootstrapped = MutableStateFlow(false)
        var cursor = 10L
        val joined = ArrayList<Long>()
        suspend fun pass(budgetMs: Long) {
            val rows = listOf(11L).filter { it > cursor }
            val held = ArrayList<Long>()
            val ready = rows.isEmpty() || awaitChatLayerReady({ bootstrapped.value }, bootstrapped, budgetMs)
            for (id in rows) {
                if (!ready) {
                    held += id
                    continue
                }
                joined += id
            }
            cursor = inboxCursorAfterPass(cursor, rows.maxOrNull() ?: cursor, held)
        }
        // First pass: the bootstrap has not finished within the budget.
        pass(budgetMs = 1_000L)
        assertEquals(emptyList<Long>(), joined)
        assertEquals(10L, cursor)
        // The bootstrap finishes; the next pass joins the same Welcome.
        bootstrapped.value = true
        pass(budgetMs = 1_000L)
        assertEquals(listOf(11L), joined)
        assertEquals(11L, cursor)
    }
}
