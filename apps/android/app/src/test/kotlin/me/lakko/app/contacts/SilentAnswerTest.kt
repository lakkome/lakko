// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import me.lakko.app.auth.base64Encode
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Base scenario 7 (seen 2026-11-22): this account removed a person and asked
 * them again. Their device still has this account as a contact and answers
 * with the ack alone; the one-to-one both still hold is the answer. The ack,
 * with that one-to-one, makes them a contact again and retires the request.
 */
class SilentAnswerTest {

    private val key = ByteArray(32) { 5 }
    private val keyB64 = base64Encode(key)
    private val accountId = ByteArray(16) { 5 }
    private val groupHex = "ab".repeat(32)

    private fun ack(pubkey: ByteArray = key, account: ByteArray = accountId) = IncomingContactAck(
        messageId = 40L,
        accepterPubkey = pubkey,
        accepterAccountId = account,
        accepterX25519Pubkey = ByteArray(32),
        ackToPseudonym = ByteArray(32),
        acceptedAtMs = 1uL,
        createdAt = "",
    )

    private fun sent(recipientKeyB64: String?) = SentContactRequest(
        recipientUsername = "w17a0851",
        recipientAccountIdB64 = base64Encode(accountId),
        messageId = 7L,
        createdAt = "2026-11-22T09:19:00Z",
        recipientKeyB64 = recipientKeyB64,
    )

    /** The pass in `ContactsRepository.pollInbox`, pure: acks, the silent
     *  answers, then the answered requests. */
    private fun pass(
        sent: List<SentContactRequest>,
        acks: List<IncomingContactAck>,
        live: (ByteArray) -> String?,
    ): AnsweredRequests {
        val answered = acksAnsweredByLiveOneToOne(sent, acks, live)
        val applied = applyContactAcks(emptyList(), sent, acks)
        return applyAnsweringWelcomes(applied.accepted, applied.sent, answered, 1uL, "")
    }

    @Test
    fun `a silent answer from a person with a live one-to-one makes them a contact with it and retires Sent`() {
        val result = pass(listOf(sent(keyB64)), listOf(ack())) { if (it.contentEquals(key)) groupHex else null }
        assertEquals(1, result.accepted.size)
        assertTrue(result.accepted[0].request.fromPubkey.contentEquals(key))
        assertEquals(groupHex, result.accepted[0].groupIdHex)
        assertTrue(result.accepted[0].request.fromAccountId!!.contentEquals(accountId))
        assertTrue(result.sent.isEmpty())
    }

    @Test
    fun `a request that named the account but no key counts too`() {
        val result = pass(listOf(sent(null)), listOf(ack())) { groupHex }
        assertEquals(1, result.accepted.size)
        assertTrue(result.sent.isEmpty())
    }

    @Test
    fun `without a one-to-one the ack makes no contact by itself and asks for a chat to be founded`() {
        val result = pass(listOf(sent(keyB64)), listOf(ack())) { null }
        assertTrue(result.accepted.isEmpty())
        assertEquals(1, result.sent.size)
        val needs = silentAnswersNeedingChat(listOf(sent(keyB64)), listOf(ack(), ack())) { null }
        assertEquals(1, needs.size)
        assertTrue(needs[0].accepterPubkey.contentEquals(key))
    }

    @Test
    fun `nothing is founded when a one-to-one is held, or for a person never asked`() {
        assertTrue(silentAnswersNeedingChat(listOf(sent(keyB64)), listOf(ack())) { groupHex }.isEmpty())
        val stray = ack(pubkey = ByteArray(32) { 9 }, account = ByteArray(16) { 9 })
        assertTrue(silentAnswersNeedingChat(listOf(sent(keyB64)), listOf(stray)) { null }.isEmpty())
        assertTrue(silentAnswersNeedingChat(listOf(sent(keyB64)), listOf(ack(pubkey = ByteArray(32) { 9 }))) { null }.isEmpty())
        assertTrue(silentAnswersNeedingChat(emptyList(), listOf(ack())) { null }.isEmpty())
    }

    @Test
    fun `an ack naming the asked account under another key brings nobody back`() {
        val other = ByteArray(32) { 9 }
        assertTrue(acksAnsweredByLiveOneToOne(listOf(sent(keyB64)), listOf(ack(pubkey = other))) { groupHex }.isEmpty())
    }

    @Test
    fun `a person this device never asked is not brought back by their one-to-one`() {
        val stray = ack(pubkey = ByteArray(32) { 9 }, account = ByteArray(16) { 9 })
        assertTrue(acksAnsweredByLiveOneToOne(listOf(sent(keyB64)), listOf(stray)) { groupHex }.isEmpty())
        assertTrue(acksAnsweredByLiveOneToOne(emptyList(), listOf(ack())) { groupHex }.isEmpty())
    }
}
