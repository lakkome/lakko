// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side unit tests for the rule "a person this account has a one-to-one
 * conversation with IS a contact" ([adoptingOneToOnePeer]), the pure half of
 * `ContactsRepository.adoptOneToOnePeer`. Plain Kotlin over the
 * `AcceptedContact` / `IncomingContactRequest` data classes, so no emulator
 * and no native library.
 *
 * The defect these state as a rule (seen live 2026-09-26): a device held a
 * named one-to-one chat with no contact row for the person, because the
 * conversation arrived by a Welcome rather than by Accept or a ContactAck.
 */
class OneToOneIsAContactTest {
    private val alice = ByteArray(32) { 0x0A }
    private val bob = ByteArray(32) { 0x0B }
    private val bobAccount = ByteArray(16) { 0x2B }
    private val chatWithBob = "0b0b0b0b"

    private fun row(
        key: ByteArray,
        messageId: Long,
        groupIdHex: String?,
        displayName: String? = null,
        accountId: ByteArray? = null,
    ): AcceptedContact =
        AcceptedContact(
            request = IncomingContactRequest(
                messageId = messageId,
                fromPubkey = key,
                fromAccountId = accountId,
                fromPseudonym = ByteArray(32) { 0x55 },
                validAfterMs = 1_000uL,
                intro = "hi",
                createdAt = "2026-09-01T00:00:00Z",
            ),
            groupIdHex = groupIdHex,
            displayName = displayName,
        )

    private fun adopt(accepted: List<AcceptedContact>, accountId: ByteArray? = bobAccount) =
        adoptingOneToOnePeer(
            accepted = accepted,
            signatureKey = bob,
            accountId = accountId,
            groupIdHex = chatWithBob,
            nowMs = 5_000uL,
            createdAt = "2026-09-26T10:00:00Z",
        )

    @Test
    fun `adds a row for a peer with no contact row`() {
        val before = listOf(row(alice, 7L, "a11ce"))
        val after = adopt(before)!!

        assertEquals(2, after.size)
        assertSame(before[0], after[0])
        val added = after[1]
        assertArrayEquals(bob, added.request.fromPubkey)
        assertArrayEquals(bobAccount, added.request.fromAccountId)
        assertEquals(chatWithBob, added.groupIdHex)
        assertNull(added.displayName)
        assertNull(added.request.intro)
        assertEquals(5_000uL, added.request.validAfterMs)
        assertEquals("2026-09-26T10:00:00Z", added.request.createdAt)
        assertArrayEquals(ByteArray(32), added.request.fromPseudonym)
        assertEquals(syntheticContactMessageId(bob), added.request.messageId)
    }

    @Test
    fun `adds a row without an account id when no profile named one`() {
        val added = adopt(emptyList(), accountId = null)!!.single()
        assertNull(added.request.fromAccountId)
        assertEquals(chatWithBob, added.groupIdHex)
    }

    @Test
    fun `completes a row with no recorded chat without losing the name or the id`() {
        val before = listOf(
            row(alice, 7L, "a11ce"),
            row(bob, 42L, groupIdHex = null, displayName = "Bobby", accountId = bobAccount),
        )
        val after = adopt(before, accountId = null)!!

        assertEquals(2, after.size)
        assertSame(before[0], after[0])
        val completed = after[1]
        assertEquals(chatWithBob, completed.groupIdHex)
        assertEquals("Bobby", completed.displayName)
        assertEquals(42L, completed.request.messageId)
        assertSame(before[1].request, completed.request)
        // The recorded account id is kept even when the caller offers none.
        assertArrayEquals(bobAccount, completed.request.fromAccountId)
    }

    @Test
    fun `leaves a complete row alone`() {
        val before = listOf(row(bob, 42L, "0ther", displayName = "Bobby"))
        assertNull(adopt(before))
    }

    @Test
    fun `never overwrites a recorded chat`() {
        // A row already naming a different chat is complete: the adoption
        // must not move it onto the conversation it was called for.
        val before = listOf(row(bob, 42L, "0ther"))
        assertNull(adopt(before))
        assertEquals("0ther", before[0].groupIdHex)
    }

    @Test
    fun `other rows are untouched`() {
        val carol = ByteArray(32) { 0x0C }
        val before = listOf(
            row(alice, 7L, "a11ce", displayName = "Alice"),
            row(carol, 9L, null, displayName = "Carol"),
        )
        val after = adopt(before)!!

        assertEquals(3, after.size)
        assertSame(before[0], after[0])
        assertSame(before[1], after[1])
        assertArrayEquals(bob, after[2].request.fromPubkey)
    }

    @Test
    fun `synthetic id is stable negative and per key`() {
        val idBob = syntheticContactMessageId(bob)
        assertEquals(idBob, syntheticContactMessageId(ByteArray(32) { 0x0B }))
        assertTrue(idBob < 0)
        assertTrue(syntheticContactMessageId(alice) < 0)
        assertNotEquals(idBob, syntheticContactMessageId(alice))
        // Adopting twice from an empty list mints the same row id.
        assertEquals(adopt(emptyList())!!.single().request.messageId, idBob)
    }
}
