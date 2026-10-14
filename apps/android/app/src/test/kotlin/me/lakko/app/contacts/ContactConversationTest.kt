// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.fail
import org.junit.Test

/**
 * Which conversation is THE one-to-one with a contact, and how one is
 * founded (`ContactConversation.kt`). The rule cases are the list of
 * `apps/web/src/contacts/contact-conversation.test.ts`.
 */
class ContactConversationTest {

    private data class Conv(val id: String, val people: List<String>, val oneToOne: Boolean)

    private val me = "me"
    private val anna = "anna"

    private fun find(contactKey: String, linked: String?, conversations: List<Conv>, own: String? = me): Conv? =
        conversationWithContact(
            contactKey = contactKey,
            linkedId = linked,
            conversations = conversations,
            idOf = { it.id },
            ownKey = own,
            peopleOf = { it.people },
            isOneToOne = { it.oneToOne },
        )

    @Test
    fun `the conversation linked on the contact answers while the registry holds it`() {
        val linked = Conv("g1", listOf(me, anna), oneToOne = true)
        assertSame(linked, find(anna, "g1", listOf(linked)))
    }

    @Test
    fun `a linked id whose chat is gone is not an address`() {
        assertNull(find(anna, "gone", emptyList()))
    }

    @Test
    fun `else the one-to-one whose two people are this account and the contact`() {
        val other = Conv("g0", listOf(me, "boris"), oneToOne = true)
        val mine = Conv("g2", listOf(anna, me), oneToOne = true)
        assertSame(mine, find(anna, "gone", listOf(other, mine)))
        assertSame(mine, find(anna, null, listOf(other, mine)))
    }

    @Test
    fun `never a group that happens to hold the same two people`() {
        // Writing there is not writing to the contact.
        val group = Conv("g3", listOf(me, anna), oneToOne = false)
        assertNull(find(anna, null, listOf(group)))
    }

    @Test
    fun `never a one-to-one with someone else, or one this account is not in`() {
        assertNull(find(anna, null, listOf(Conv("g", listOf(me, "boris"), oneToOne = true))))
        assertNull(find(anna, null, listOf(Conv("g", listOf(anna, "boris"), oneToOne = true))))
    }

    @Test
    fun `a torn conversation is no match`() {
        assertNull(find(anna, null, listOf(Conv("g", emptyList(), oneToOne = true))))
    }

    @Test
    fun `before a session knows its own key only the linked conversation answers`() {
        val conv = Conv("g1", listOf(me, anna), oneToOne = true)
        assertNull(find(anna, null, listOf(conv), own = null))
        assertSame(conv, find(anna, "g1", listOf(conv), own = null))
    }

    // ---- founding ----------------------------------------------------------

    private fun contact(accountId: ByteArray?, groupIdHex: String? = null) = AcceptedContact(
        request = IncomingContactRequest(
            messageId = 1L,
            fromPubkey = ByteArray(32) { 7 },
            fromAccountId = accountId,
            fromPseudonym = ByteArray(32),
            validAfterMs = 1_700_000_000_000uL,
            intro = null,
            createdAt = "2026-10-02T10:00:00Z",
        ),
        groupIdHex = groupIdHex,
    )

    @Test
    fun `founding creates the one-to-one, then links it on the contact`() = runTest {
        val calls = mutableListOf<String>()
        val id = foundOneToOneWith(
            contact = contact(accountId = ByteArray(16) { 3 }),
            startOneToOne = { accountId, key ->
                calls += "start:${accountId[0]}:${key[0]}"
                "aa11"
            },
            link = { key, groupIdHex -> calls += "link:${key[0]}:$groupIdHex" },
        )
        assertEquals("aa11", id)
        assertEquals(listOf("start:3:7", "link:7:aa11"), calls)
    }

    @Test
    fun `a contact record with no account id founds nothing`() = runTest {
        val calls = mutableListOf<String>()
        try {
            foundOneToOneWith(
                contact = contact(accountId = null),
                startOneToOne = { _, _ ->
                    calls += "start"
                    "aa11"
                },
                link = { _, _ -> calls += "link" },
            )
            fail("there is no account to fetch the person's devices by")
        } catch (expected: ContactHasNoAccountId) {
            // The caller reports its own failure.
        }
        assertEquals(emptyList<String>(), calls)
    }

    @Test
    fun `a creation that fails links nothing`() = runTest {
        val calls = mutableListOf<String>()
        try {
            foundOneToOneWith(
                contact = contact(accountId = ByteArray(16)),
                startOneToOne = { _, _ -> throw IllegalStateException("no KeyPackage") },
                link = { _, _ -> calls += "link" },
            )
            fail("the creation's failure propagates")
        } catch (expected: IllegalStateException) {
            assertEquals("no KeyPackage", expected.message)
        }
        assertEquals(emptyList<String>(), calls)
    }
}
