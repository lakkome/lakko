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

/**
 * ADR-0101 live walk of 2026-11-26: the rules behind D2 (a returned history
 * row authored under this account's previous key is its own), D3 (a re-sent
 * message and the history page's copy of it show once) and D4 (the
 * successor's name is posted until it went out, a founder that was no admin
 * hands the ownership over only once the history was served, and a group
 * name is judged against the role table read at receipt).
 */
class ResetWalkFixesTest {

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

    private val previousOwn = "UFJFVklPVVNfT1dOX0tFWV8zMl9CWVRFU19MT05HISE="
    private val currentOwn = "Q1VSUkVOVF9PV05fS0VZXzMyX0JZVEVTX0xPTkdfISE="
    private val peer = "UEVFUl9LRVlfMzJfQllURVNfTE9OR19fX19fX19fXyE="

    private fun row(id: Long, direction: String = "incoming") = PersistedMessage(
        messageId = id,
        direction = direction,
        text = "m$id",
        attachments = emptyList(),
        createdAt = "2026-11-26T10:55:00Z",
    )

    // -- D2 ------------------------------------------------------------------

    @Test
    fun `a history row authored under the previous own key is drawn as own`() {
        val own = setOf(currentOwn, previousOwn)
        val r = reentryBundleRow(row(10), previousOwn, own, ReentryRouteKind.MAILBOX)
        assertEquals("outgoing", r.direction)
        assertEquals(previousOwn, r.authorKeyB64)
    }

    @Test
    fun `a history row by the other person stays received`() {
        val r = reentryBundleRow(row(11, "outgoing"), peer, setOf(currentOwn, previousOwn), ReentryRouteKind.MAILBOX)
        assertEquals("incoming", r.direction)
        assertEquals(peer, r.authorKeyB64)
    }

    @Test
    fun `without the previous key the old own row reads as received, as before`() {
        val r = reentryBundleRow(row(12), previousOwn, currentOwn, ReentryRouteKind.MAILBOX)
        assertEquals("incoming", r.direction)
    }

    @Test
    fun `a system row names nobody whatever the keys`() {
        val r = reentryBundleRow(row(13, "system"), previousOwn, setOf(previousOwn), ReentryRouteKind.MAILBOX)
        assertEquals("system", r.direction)
        assertNull(r.authorKeyB64)
    }

    // -- D3 ------------------------------------------------------------------

    private val identity: (String) -> String = { it }

    @Test
    fun `a re-send of a row the receiving group already holds by the same author is a duplicate`() {
        val marker = ResendOrigin(groupIdB64 = "b2xk", messageId = 42L)
        val held = HeldRow(HeldRowDirection.INCOMING, peer)
        assertTrue(resendDuplicatesHeldRow(marker, held, currentOwn, peer, identity))
    }

    @Test
    fun `a re-send naming another author's row, or a row not held, is drawn`() {
        val marker = ResendOrigin(groupIdB64 = "b2xk", messageId = 42L)
        assertFalse(resendDuplicatesHeldRow(marker, HeldRow(HeldRowDirection.INCOMING, previousOwn), currentOwn, peer, identity))
        assertFalse(resendDuplicatesHeldRow(marker, null, currentOwn, peer, identity))
        assertFalse(resendDuplicatesHeldRow(null, HeldRow(HeldRowDirection.INCOMING, peer), currentOwn, peer, identity))
        assertFalse(resendDuplicatesHeldRow(marker.copy(messageId = 0L), HeldRow(HeldRowDirection.INCOMING, peer), currentOwn, peer, identity))
        // A system row is no one's.
        assertFalse(resendDuplicatesHeldRow(marker, HeldRow(HeldRowDirection.SYSTEM, peer), currentOwn, peer, identity))
    }

    @Test
    fun `authors are compared through their current key`() {
        val oldPeer = "T0xEX1BFRVJfS0VZXzMyX0JZVEVTX0xPTkdfX19fXyE="
        val canonical: (String) -> String = { if (it == oldPeer) peer else it }
        val marker = ResendOrigin(groupIdB64 = "b2xk", messageId = 7L)
        assertTrue(resendDuplicatesHeldRow(marker, HeldRow(HeldRowDirection.INCOMING, oldPeer), currentOwn, peer, canonical))
        assertTrue(historyRowAlreadyResent(7L, oldPeer, listOf(DrawnResend(7L, peer)), canonical))
    }

    @Test
    fun `a history row whose original was already re-sent is left out, only for that author and id`() {
        val drawn = listOf(DrawnResend(originMessageId = 42L, senderKeyB64 = peer))
        assertTrue(historyRowAlreadyResent(42L, peer, drawn, identity))
        assertFalse(historyRowAlreadyResent(43L, peer, drawn, identity))
        assertFalse(historyRowAlreadyResent(42L, previousOwn, drawn, identity))
        assertFalse(historyRowAlreadyResent(42L, null, drawn, identity))
        assertFalse(historyRowAlreadyResent(42L, peer, emptyList(), identity))
    }

    @Test
    fun `drawn re-sends are kept per account and group, once each`() {
        val storage = MapStorage()
        val store = DrawnResendStore(storage)
        store.record("acct", "ABCD", DrawnResend(5L, peer))
        store.record("acct", "abcd", DrawnResend(5L, peer))
        store.record("acct", "abcd", DrawnResend(6L, peer))
        store.record("acct", "ef01", DrawnResend(0L, peer))
        assertEquals(listOf(DrawnResend(5L, peer), DrawnResend(6L, peer)), store.drawn("acct", "abcd"))
        assertTrue(store.drawn("acct", "ef01").isEmpty())
        assertTrue(store.drawn("other", "abcd").isEmpty())
        // Read back by a fresh instance from what was written.
        assertEquals(2, DrawnResendStore(storage).drawn("acct", "abcd").size)
    }

    // -- D4 ------------------------------------------------------------------

    @Test
    fun `the old name is stored and posted, re-posted while it has not gone out, never over a later rename`() {
        assertEquals(SuccessorNameStep.SAVE_AND_SEND, successorNameStep(100uL, null))
        assertEquals(SuccessorNameStep.SAVE_AND_SEND, successorNameStep(100uL, 50uL))
        assertEquals(SuccessorNameStep.SEND, successorNameStep(100uL, 100uL))
        assertEquals(SuccessorNameStep.DONE, successorNameStep(100uL, 200uL))
    }

    @Test
    fun `the hand-over waits for the served history, or three days`() {
        val storage = MapStorage()
        val store = HistoryServedStore(storage)
        // The first ask starts the wait and is not due.
        assertFalse(store.transferDue("acct", "ABCD", 1_000L))
        assertFalse(store.transferDue("acct", "abcd", 1_000L + HistoryServedStore.WAIT_MS - 1))
        assertTrue(store.transferDue("acct", "abcd", 1_000L + HistoryServedStore.WAIT_MS))
        // Served: due at once, whatever the time.
        assertFalse(store.transferDue("acct", "ef01", 5L))
        store.markServed("acct", "EF01", 6L)
        assertTrue(store.transferDue("acct", "ef01", 7L))
        // Served before it was ever held back: due too.
        store.markServed("acct", "1234", 9L)
        assertTrue(HistoryServedStore(storage).transferDue("acct", "1234", 10L))
        // A clock that went back reads as just held back.
        assertFalse(HistoryServedStore.transferDue(HistoryServedStore.Entry(firstHeldMs = 100L), 50L))
        assertFalse(HistoryServedStore.transferDue(null, 50L))
    }

    @Test
    fun `a plain member of a group never answers a history request`() {
        assertFalse(
            historyAnswerAllowed(
                fromSelf = false,
                requesterResetAccepted = true,
                requesterWasInEarlierGroup = true,
                earlierGroupHeld = true,
                kind = ConversationKind.Group,
                ownIsOwnerOrAdmin = false,
            ),
        )
    }

    @Test
    fun `a group name is judged against the table read at receipt, else the live one`() {
        val founder = ByteArray(32) { 1 }
        val reset = ByteArray(32) { 2 }
        val atReceipt = me.lakko.app.push.GroupPushRouting.OwnerLookup.Known(founder)
        // After the batch the founder handed the ownership over.
        val liveAfterBatch = me.lakko.app.push.GroupPushRouting.OwnerLookup.Known(reset)
        val incoming = StoredGroupName("Reset group", 100uL)
        val judged = groupNameAuthority(atReceipt) { liveAfterBatch }
        assertEquals(
            GroupNameVerdict.ACCEPT_OWNER,
            groupNameVerdict(founder, judged, incoming, null) { a, b -> a > b },
        )
        // The live table alone would have refused the founder's name.
        assertEquals(
            GroupNameVerdict.REJECT_NOT_AUTHORIZED,
            groupNameVerdict(founder, groupNameAuthority(null) { liveAfterBatch }, incoming, null) { a, b -> a > b },
        )
    }
}
