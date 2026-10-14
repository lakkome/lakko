// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import me.lakko.app.contacts.AcceptedContact
import me.lakko.app.contacts.IncomingContactRequest
import me.lakko.app.contacts.foundOneToOneWith
import me.lakko.app.ui.contacts.ContactRowModel
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `Invite contacts` on the call screen (`docs/ux/call-links.md` section 3):
 * the list is the person's contacts, and choosing one sends the invitation
 * into the one-to-one with them, founded first when there is none. The rows
 * and filter cases are the list of `apps/web/src/calls/invite-contacts.test.ts`.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class InviteContactsTest {

    // ---- rows and filter ---------------------------------------------------

    private val zoe = ContactRowModel(key = "k-zoe", name = "Zoe", username = "zoe_k")
    private val anna = ContactRowModel(key = "k-anna", name = "Anna", username = null)
    private val boris = ContactRowModel(key = "k-boris", name = "boris", username = "b0ris")

    private fun rows() = inviteContactRows(listOf(zoe, anna, boris))

    @Test
    fun `every contact is listed, by name`() {
        assertEquals(listOf("Anna", "boris", "Zoe"), rows().map { it.name })
    }

    @Test
    fun `a row carries the key the invitation is addressed to, and the username when known`() {
        assertEquals(anna, rows()[0])
        assertEquals(zoe, rows()[2])
    }

    @Test
    fun `a contact is listed once even if the record is doubled`() {
        assertEquals(1, inviteContactRows(listOf(anna, anna)).size)
    }

    @Test
    fun `the list is empty for a person with no contacts`() {
        assertEquals(emptyList<ContactRowModel>(), inviteContactRows(emptyList()))
    }

    @Test
    fun `the order is the Contacts list's own`() {
        // An unnamed contact sorts by its username; a name that starts with
        // no letter goes last, as under the Contacts list's `#`.
        val unnamed = ContactRowModel(key = "k-m", name = "Member", username = "carl", placeholder = true)
        val digits = ContactRowModel(key = "k-7", name = "7 of Nine", username = null)
        assertEquals(
            listOf("k-anna", "k-boris", "k-m", "k-zoe", "k-7"),
            inviteContactRows(listOf(digits, zoe, unnamed, boris, anna)).map { it.key },
        )
    }

    @Test
    fun `an empty query keeps everything`() {
        assertEquals(3, filterInviteContactRows(rows(), "  ").size)
    }

    @Test
    fun `a query matches by name or by username, ignoring case and a typed at sign`() {
        assertEquals(listOf("k-anna"), filterInviteContactRows(rows(), "ann").map { it.key })
        assertEquals(listOf("k-zoe"), filterInviteContactRows(rows(), "@ZOE_").map { it.key })
        assertEquals(listOf("k-boris"), filterInviteContactRows(rows(), "b0").map { it.key })
    }

    @Test
    fun `a query nobody matches keeps nothing`() {
        assertEquals(emptyList<ContactRowModel>(), filterInviteContactRows(rows(), "nobody"))
    }

    @Test
    fun `with no contacts the list says so, and a search with no match says that`() {
        assertEquals("call.invite.picker_empty", inviteContactsEmptyKey(contactCount = 0, visibleCount = 0))
        assertEquals("chats.search.no_results", inviteContactsEmptyKey(contactCount = 3, visibleCount = 0))
        assertNull(inviteContactsEmptyKey(contactCount = 3, visibleCount = 1))
    }

    // ---- sending -----------------------------------------------------------

    private fun contact(key: Int, accountId: ByteArray? = ByteArray(16) { key.toByte() }, linked: String? = null) =
        AcceptedContact(
            request = IncomingContactRequest(
                messageId = key.toLong(),
                fromPubkey = ByteArray(32) { key.toByte() },
                fromAccountId = accountId,
                fromPseudonym = ByteArray(32),
                validAfterMs = 1_700_000_000_000uL,
                intro = null,
                createdAt = "2026-10-02T10:00:00Z",
            ),
            groupIdHex = linked,
        )

    /** The contact list, the conversations and the send, as far as inviting
     *  goes. Founding runs the real [foundOneToOneWith]. */
    private class World(val contacts: Map<String, AcceptedContact>) : InviteContactPorts {
        /** contact message id -> the one-to-one this device holds with them. */
        val conversations = mutableMapOf<Long, String>()
        val created = mutableListOf<String>()
        val linked = mutableListOf<String>()
        val sent = mutableListOf<Pair<String, String>>()
        var failSend: Throwable? = null

        override fun contactFor(rowKey: String): AcceptedContact? = contacts[rowKey]

        override fun conversationWith(contact: AcceptedContact): String? =
            conversations[contact.request.messageId]

        override suspend fun foundOneToOne(contact: AcceptedContact): String =
            foundOneToOneWith(
                contact = contact,
                startOneToOne = { _, key ->
                    val id = "new-${key[0]}"
                    created += id
                    conversations[contact.request.messageId] = id
                    id
                },
                link = { key, groupIdHex -> linked += "${key[0]}:$groupIdHex" },
            )

        override suspend fun sendText(groupIdHex: String, text: String) {
            failSend?.let { throw it }
            sent += groupIdHex to text
        }
    }

    private class Harness(val host: LinkCallHost, val port: HostedCallsTest.FakeLinkPort)

    /** A live link call this device founded: the one that has a link to give. */
    private suspend fun TestScope.liveCall(): Harness {
        val port = HostedCallsTest.FakeLinkPort()
        val host = LinkCallHost(
            scope = backgroundScope,
            otherOccupant = { null },
            onSlotChanged = {},
            warn = { _, _ -> },
        )
        host.attach(port)
        runCurrent()
        host.start(
            LinkCallRequest(
                callId = ByteArray(16) { 0x11 },
                ownerAccountId = ByteArray(16),
                ownerSignatureKey = ByteArray(32),
                mediaVideo = false,
                isOwner = true,
                slug = "slug",
                accountIdentitySeed = ByteArray(64),
                sessionToken = "token",
                accountId = ByteArray(16),
            ),
        )
        return Harness(host, port)
    }

    private val identity = CallInviteIdentity(
        accountIdentitySeed = ByteArray(64),
        accountId = ByteArray(16),
        sessionToken = "token",
    )

    private suspend fun Harness.invite(world: World, rowKey: String, name: String): Boolean =
        inviteContactToCall(world, rowKey) { send -> host.sendInvite(identity, name, send) }

    @Test
    fun `the invitation goes into the one-to-one this device already holds`() = runTest {
        val h = liveCall()
        val world = World(mapOf("k-anna" to contact(1)))
        world.conversations[1L] = "chat-anna"

        assertTrue(h.invite(world, "k-anna", "Anna"))

        val url = mintedInviteUrl(h.host.invite.value)!!
        // One plain text message whose whole text is the call's link.
        assertEquals(listOf("chat-anna" to url), world.sent)
        assertTrue(world.created.isEmpty())
        assertEquals(InCallInvite.Ready(url, LinkDelivery.Sent("Anna")), h.host.invite.value)
    }

    @Test
    fun `with no conversation one is founded once, linked, and the invitation sent into it`() = runTest {
        val h = liveCall()
        val world = World(mapOf("k-anna" to contact(1)))

        assertTrue(h.invite(world, "k-anna", "Anna"))

        assertEquals(listOf("new-1"), world.created)
        assertEquals(listOf("1:new-1"), world.linked)
        assertEquals(listOf("new-1"), world.sent.map { it.first })
        // Inviting again finds the conversation it founded: nothing is
        // created twice.
        assertTrue(h.invite(world, "k-anna", "Anna"))
        assertEquals(listOf("new-1"), world.created)
        assertEquals(2, world.sent.size)
    }

    @Test
    fun `a contact record without an account id fails as not sent, and nothing is created`() = runTest {
        val h = liveCall()
        val world = World(mapOf("k-old" to contact(2, accountId = null)))

        assertFalse(h.invite(world, "k-old", "Old"))

        assertTrue(world.created.isEmpty())
        assertTrue(world.linked.isEmpty())
        assertTrue(world.sent.isEmpty())
        val url = mintedInviteUrl(h.host.invite.value)!!
        assertEquals(InCallInvite.Failed(CallInviteFailure.NOT_SENT, url), h.host.invite.value)
    }

    @Test
    fun `a failed send keeps the minted link, and the next attempt mints nothing`() = runTest {
        val h = liveCall()
        val world = World(mapOf("k-anna" to contact(1)))
        world.conversations[1L] = "chat-anna"
        world.failSend = IllegalStateException("offline")

        assertFalse(h.invite(world, "k-anna", "Anna"))
        val url = mintedInviteUrl(h.host.invite.value)!!
        assertEquals(InCallInvite.Failed(CallInviteFailure.NOT_SENT, url), h.host.invite.value)
        assertEquals(1, h.port.minted.size)

        // The row stays pressable; pressing it again sends the SAME link.
        world.failSend = null
        assertTrue(h.invite(world, "k-anna", "Anna"))
        assertEquals(1, h.port.minted.size)
        assertEquals(listOf("chat-anna" to url), world.sent)
    }

    @Test
    fun `a row whose contact is gone sends nothing and mints nothing`() = runTest {
        val h = liveCall()
        val world = World(emptyMap())

        assertFalse(h.invite(world, "k-nobody", "Nobody"))

        assertTrue(h.port.minted.isEmpty())
        assertEquals(InCallInvite.Idle, h.host.invite.value)
    }
}
