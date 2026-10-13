// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side unit tests for the persisted stranger-inbox state (the fix for
 * the 2026-09-16 live finding: a relaunch re-fetched the whole inbox from
 * cursor `0`). Run as `:app:testDebugUnitTest` — no emulator.
 *
 *   1. The store contract over [InMemoryContactsInboxStore] and the codec it
 *      shares with the `SharedPreferences` store: cursor + pending
 *      round-trip, the cursor never moving backwards, delete.
 *   2. The dedupe rule [mergeFetchedRequests]: a row already pending or a
 *      sender already accepted is dropped, order is preserved.
 *   3. [withoutAcceptedSenders] — one row per person once someone becomes a
 *      contact by another route.
 */
class ContactsInboxStoreTest {

    private val account = "YWNjb3VudC1pZC0xNi1ieXRlcw=="

    private fun request(
        messageId: Long,
        pubkeyByte: Int,
        accountId: ByteArray? = ByteArray(16) { pubkeyByte.toByte() },
        intro: String? = "hello from $pubkeyByte",
    ) = IncomingContactRequest(
        messageId = messageId,
        fromPubkey = ByteArray(32) { pubkeyByte.toByte() },
        fromAccountId = accountId,
        fromPseudonym = ByteArray(32) { (pubkeyByte + 1).toByte() },
        validAfterMs = 1_700_000_000_000uL + messageId.toULong(),
        intro = intro,
        createdAt = "2026-09-16T10:00:0${messageId % 10}Z",
    )

    private fun accepted(pubkeyByte: Int) =
        AcceptedContact(request = request(messageId = 1_000L + pubkeyByte, pubkeyByte = pubkeyByte))

    // -----------------------------------------------------------------
    // Store contract + codec
    // -----------------------------------------------------------------

    @Test
    fun `an unknown account loads the empty snapshot`() {
        val store = InMemoryContactsInboxStore()
        assertEquals(ContactsInboxSnapshot(), store.load(account))
    }

    @Test
    fun `pending round-trips through the store including a legacy row and a null intro`() {
        val store = InMemoryContactsInboxStore()
        val pending = listOf(
            request(24L, 0x0A),
            request(25L, 0x0B, accountId = null, intro = null),
            request(26L, 0x0C, intro = ""),
        )
        store.savePending(account, pending)
        assertEquals(pending, store.load(account).pending)
    }

    @Test
    fun `the codec is stable and refuses garbage`() {
        val pending = listOf(request(7L, 0x11), request(8L, 0x22, accountId = null))
        val raw = encodePendingRequests(pending)
        assertEquals(pending, decodePendingRequests(raw))
        assertEquals(raw, encodePendingRequests(decodePendingRequests(raw).orEmpty()))
        assertNull(decodePendingRequests("not json"))
        assertNull(decodePendingRequests("""[{"messageId":1}]"""))
    }

    @Test
    fun `an empty pending list is a valid persisted state`() {
        val store = InMemoryContactsInboxStore()
        store.savePending(account, listOf(request(1L, 0x01)))
        store.savePending(account, emptyList())
        assertTrue(store.load(account).pending.isEmpty())
    }

    @Test
    fun `the cursor round-trips and is per account`() {
        val store = InMemoryContactsInboxStore()
        store.saveCursor(account, 42L)
        assertEquals(42L, store.load(account).cursor)
        assertEquals(0L, store.load("b3RoZXItYWNjb3VudA==").cursor)
    }

    @Test
    fun `the cursor never moves backwards`() {
        assertEquals(10L, advanceInboxCursor(current = 10L, fetched = 5L))
        assertEquals(12L, advanceInboxCursor(current = 10L, fetched = 12L))
        assertEquals(10L, advanceInboxCursor(current = 10L, fetched = 10L))
        assertEquals(0L, advanceInboxCursor(current = 0L, fetched = -1L))

        val store = InMemoryContactsInboxStore()
        store.saveCursor(account, 30L)
        store.saveCursor(account, 12L)
        assertEquals(30L, store.load(account).cursor)
        store.saveCursor(account, 31L)
        assertEquals(31L, store.load(account).cursor)
    }

    @Test
    fun `delete drops both the cursor and the pending list`() {
        val store = InMemoryContactsInboxStore()
        store.saveCursor(account, 9L)
        store.savePending(account, listOf(request(9L, 0x09)))
        store.delete(account)
        assertEquals(ContactsInboxSnapshot(), store.load(account))
    }

    // -----------------------------------------------------------------
    // Dedupe on fetch
    // -----------------------------------------------------------------

    @Test
    fun `a fetched row already pending is dropped`() {
        val existing = request(24L, 0x0A)
        val merge = mergeFetchedRequests(
            pending = listOf(existing),
            accepted = emptyList(),
            fetched = listOf(request(24L, 0x0A), request(25L, 0x0B)),
        )
        assertEquals(listOf(24L, 25L), merge.pending.map { it.messageId })
        assertEquals(listOf(24L), merge.dropped.map { it.messageId })
        assertEquals(1, merge.pending.count { it.messageId == 24L })
    }

    @Test
    fun `a fetched row from an accepted contact is dropped`() {
        val merge = mergeFetchedRequests(
            pending = emptyList(),
            accepted = listOf(accepted(0x0A)),
            // A fresh DS row (new messageId) from a person already accepted.
            fetched = listOf(request(40L, 0x0A), request(41L, 0x0B)),
        )
        assertEquals(listOf(41L), merge.pending.map { it.messageId })
        assertEquals(listOf(40L), merge.dropped.map { it.messageId })
    }

    @Test
    fun `a duplicate inside one batch is kept once`() {
        val merge = mergeFetchedRequests(
            pending = emptyList(),
            accepted = emptyList(),
            fetched = listOf(request(24L, 0x0A), request(24L, 0x0A)),
        )
        assertEquals(listOf(24L), merge.pending.map { it.messageId })
        assertEquals(1, merge.dropped.size)
    }

    @Test
    fun `a clean batch appends in arrival order after the existing rows`() {
        val merge = mergeFetchedRequests(
            pending = listOf(request(1L, 0x01)),
            accepted = listOf(accepted(0x0F)),
            fetched = listOf(request(3L, 0x03), request(2L, 0x02)),
        )
        assertEquals(listOf(1L, 3L, 2L), merge.pending.map { it.messageId })
        assertTrue(merge.dropped.isEmpty())
    }

    @Test
    fun `re-fetching the whole inbox after a relaunch produces no duplicate and no accepted person`() {
        // The pre-fix defect: the saved pending list + the accepted list,
        // then the full inbox comes back from cursor 0.
        val saved = listOf(request(24L, 0x0A))
        val contacts = listOf(accepted(0x0B), accepted(0x0C))
        val wholeInbox = listOf(
            request(10L, 0x0B), // accepted long ago
            request(11L, 0x0C), // accepted long ago
            request(24L, 0x0A), // still pending, already shown
        )
        val merge = mergeFetchedRequests(saved, contacts, wholeInbox)
        assertEquals(saved, merge.pending)
        assertEquals(listOf(10L, 11L, 24L), merge.dropped.map { it.messageId })
    }

    // -----------------------------------------------------------------
    // One row per person
    // -----------------------------------------------------------------

    @Test
    fun `withoutAcceptedSenders drops only the rows of people now accepted`() {
        val pending = listOf(request(1L, 0x01), request(2L, 0x02), request(3L, 0x03))
        val result = withoutAcceptedSenders(pending, listOf(accepted(0x02)))
        assertEquals(listOf(1L, 3L), result.map { it.messageId })
        assertEquals(pending, withoutAcceptedSenders(pending, emptyList()))
    }

    // -----------------------------------------------------------------
    // Accept acknowledgements
    // -----------------------------------------------------------------

    private fun ack(pubkeyByte: Int, messageId: Long = 7_000L + pubkeyByte) = IncomingContactAck(
        messageId = messageId,
        accepterPubkey = ByteArray(32) { pubkeyByte.toByte() },
        accepterAccountId = ByteArray(16) { (pubkeyByte + 40).toByte() },
        accepterX25519Pubkey = ByteArray(32) { 9 },
        ackToPseudonym = ByteArray(32) { 3 },
        acceptedAtMs = 1_700_000_000_000uL,
        createdAt = "2026-09-17T10:00:00Z",
    )

    private fun sentTo(pubkeyByte: Int) = SentContactRequest(
        recipientUsername = "user$pubkeyByte",
        recipientAccountIdB64 = java.util.Base64.getEncoder()
            .encodeToString(ByteArray(16) { (pubkeyByte + 40).toByte() }),
        messageId = 1L,
        createdAt = "2026-09-17T09:00:00Z",
    )

    @Test
    fun an_ack_from_a_stranger_adds_a_contact_with_their_account_id_and_retires_the_sent_request() {
        val applied = applyContactAcks(
            accepted = listOf(accepted(1)),
            sent = listOf(sentTo(5), sentTo(6)),
            acks = listOf(ack(5)),
        )
        assertTrue(applied.acceptedChanged)
        assertEquals(2, applied.accepted.size)
        val added = applied.accepted.last().request
        assertTrue(added.fromPubkey.contentEquals(ByteArray(32) { 5 }))
        assertTrue(added.fromAccountId!!.contentEquals(ByteArray(16) { 45 }))
        assertEquals(listOf(sentTo(6)), applied.sent)
    }

    @Test
    fun an_ack_fills_the_missing_account_id_of_a_known_contact() {
        val known = AcceptedContact(request = request(messageId = 1L, pubkeyByte = 5, accountId = null))
        val applied = applyContactAcks(accepted = listOf(known), sent = emptyList(), acks = listOf(ack(5)))
        assertTrue(applied.acceptedChanged)
        assertEquals(1, applied.accepted.size)
        assertTrue(applied.accepted.single().request.fromAccountId!!.contentEquals(ByteArray(16) { 45 }))
    }

    @Test
    fun an_ack_never_overwrites_an_account_id_a_contact_already_has() {
        val known = accepted(5)
        val applied = applyContactAcks(accepted = listOf(known), sent = emptyList(), acks = listOf(ack(5)))
        assertEquals(false, applied.acceptedChanged)
        assertEquals(listOf(known), applied.accepted)
    }

    @Test
    fun a_repeated_ack_adds_the_person_once() {
        val applied = applyContactAcks(
            accepted = emptyList(),
            sent = emptyList(),
            acks = listOf(ack(5, messageId = 1L), ack(5, messageId = 2L)),
        )
        assertEquals(1, applied.accepted.size)
    }

    @Test
    fun no_acks_change_nothing() {
        val applied = applyContactAcks(listOf(accepted(1)), listOf(sentTo(2)), emptyList())
        assertEquals(false, applied.acceptedChanged)
        assertEquals(listOf(sentTo(2)), applied.sent)
    }

    // -----------------------------------------------------------------
    // ADR-0074 section 3.5a rule 5 - one request per person across a
    // proven rotation
    // -----------------------------------------------------------------

    /** Key byte 2 is the proven successor of key byte 1 (and 3 of 2). */
    private val rotated: (ByteArray, ByteArray) -> Boolean = { newKey, oldKey ->
        newKey[0] > oldKey[0] && newKey[0] <= 3 && oldKey[0] >= 1
    }

    @Test
    fun a_request_from_the_proven_successor_replaces_the_pending_row_in_place() {
        val pending = listOf(request(10, 9), request(11, 1), request(12, 8))
        val again = request(20, 2)
        val merge = mergeFetchedRequests(pending, emptyList(), listOf(again), rotated)
        assertEquals(listOf(10L, 20L, 12L), merge.pending.map { it.messageId })
        assertTrue(merge.dropped.isEmpty())
    }

    @Test
    fun an_unproven_key_change_is_two_people_and_two_rows() {
        val pending = listOf(request(11, 1))
        val merge = mergeFetchedRequests(pending, emptyList(), listOf(request(20, 2)))
        assertEquals(listOf(11L, 20L), merge.pending.map { it.messageId })
    }

    @Test
    fun a_request_from_a_key_the_person_already_left_is_dropped() {
        // A re-poll of the old row after the successor's request landed.
        val pending = listOf(request(20, 2))
        val merge = mergeFetchedRequests(pending, emptyList(), listOf(request(11, 1)), rotated)
        assertEquals(listOf(20L), merge.pending.map { it.messageId })
        assertEquals(listOf(11L), merge.dropped.map { it.messageId })
    }

    @Test
    fun both_requests_in_one_batch_leave_one_row_whatever_the_order() {
        val forward = mergeFetchedRequests(emptyList(), emptyList(), listOf(request(11, 1), request(20, 2)), rotated)
        assertEquals(listOf(20L), forward.pending.map { it.messageId })
        val backward = mergeFetchedRequests(emptyList(), emptyList(), listOf(request(20, 2), request(11, 1)), rotated)
        assertEquals(listOf(20L), backward.pending.map { it.messageId })
    }

    @Test
    fun a_second_rotation_collapses_every_earlier_row_of_the_person() {
        val pending = listOf(request(11, 1), request(15, 9), request(12, 2))
        val merge = mergeFetchedRequests(pending, emptyList(), listOf(request(30, 3)), rotated)
        assertEquals(listOf(30L, 15L), merge.pending.map { it.messageId })
    }

    @Test
    fun a_request_from_the_successor_of_an_accepted_contact_is_not_a_request() {
        val merge = mergeFetchedRequests(emptyList(), listOf(accepted(1)), listOf(request(20, 2)), rotated)
        assertTrue(merge.pending.isEmpty())
        assertEquals(listOf(20L), merge.dropped.map { it.messageId })
    }

    @Test
    fun only_a_claimed_same_account_under_another_key_is_put_to_a_proof() {
        val accountId = ByteArray(16) { 7 }
        val pending = listOf(request(11, 1, accountId = accountId), request(12, 8))
        val fetched = listOf(
            request(20, 2, accountId = accountId),
            // The same key again: nothing to prove.
            request(21, 1, accountId = accountId),
            // Another account, and a legacy request without an id.
            request(22, 5),
            request(23, 6, accountId = null),
        )
        val candidates = successorCandidates(pending, fetched)
        assertEquals(1, candidates.size)
        val (id, newKey, oldKey) = candidates.single()
        assertArrayEquals(accountId, id)
        assertEquals(2, newKey[0].toInt())
        assertEquals(1, oldKey[0].toInt())
        // The claim alone replaces nothing: without a proof it is two rows.
        assertEquals(5, mergeFetchedRequests(pending, emptyList(), fetched.take(1) + fetched.drop(2)).pending.size)
    }
}
