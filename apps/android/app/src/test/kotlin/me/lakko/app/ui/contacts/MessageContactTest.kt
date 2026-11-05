// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import me.lakko.app.contacts.AcceptedContact
import me.lakko.app.contacts.IncomingContactRequest
import me.lakko.app.contacts.MessageOutcome
import me.lakko.app.contacts.foundOneToOneWith
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `Message` on a contact's card (maintainer, 2026-11-05): open the chat when
 * there is one, found a one-to-one when there is none and the account id is
 * known, and only a contact with no account id cannot be messaged.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class MessageContactTest {

    private fun contact(accountId: ByteArray? = ByteArray(16) { 7 }, linked: String? = null) = AcceptedContact(
        request = IncomingContactRequest(
            messageId = 7L,
            fromPubkey = ByteArray(32) { 7 },
            fromAccountId = accountId,
            fromPseudonym = ByteArray(32),
            validAfterMs = 1_700_000_000_000uL,
            intro = null,
            createdAt = "2026-11-05T10:00:00Z",
        ),
        groupIdHex = linked,
    )

    @Test
    fun `a chat on this device opens`() {
        assertEquals(MessageAction.Open("aa"), messageAction("aa", ByteArray(16)))
        assertEquals(MessageAction.Open("aa"), messageAction("aa", null))
    }

    @Test
    fun `no chat and an account id founds one`() {
        assertEquals(MessageAction.Found, messageAction(null, ByteArray(16)))
    }

    @Test
    fun `no account id cannot be messaged`() {
        assertEquals(MessageAction.Unavailable, messageAction(null, null))
        assertEquals(MessageAction.Unavailable, messageAction(null, ByteArray(3)))
    }

    @Test
    fun `Message founds a one-to-one, links it to the contact and returns it`() = runTest {
        val started = mutableListOf<Pair<ByteArray, ByteArray>>()
        val links = mutableListOf<Pair<ByteArray, String>>()
        val starter = ContactChatStarter(found = { c ->
            MessageOutcome.Opened(
                foundOneToOneWith(
                    contact = c,
                    startOneToOne = { accountId, key ->
                        started += accountId to key
                        "beef"
                    },
                    link = { key, hex -> links += key to hex },
                ),
            )
        }, logFailure = {})
        val contact = contact()

        assertEquals("beef", starter.start("route", contact))
        assertEquals(1, started.size)
        assertTrue(started.single().first.contentEquals(contact.request.fromAccountId!!))
        assertTrue(started.single().second.contentEquals(contact.request.fromPubkey))
        // Linked onto the existing contact (no second row is made here).
        assertTrue(links.single().first.contentEquals(contact.request.fromPubkey))
        assertEquals("beef", links.single().second)
        assertTrue(starter.starting.value.isEmpty())
        assertTrue(starter.notices.value.isEmpty())
    }

    @Test
    fun `progress shows while founding and a second tap founds nothing`() = runTest {
        val gate = CompletableDeferred<String>()
        var calls = 0
        val starter = ContactChatStarter(found = {
            calls++
            MessageOutcome.Opened(gate.await())
        }, logFailure = {})
        val first = launch { assertEquals("cafe", starter.start("route", contact())) }
        runCurrent()
        assertEquals(setOf("route"), starter.starting.value)
        assertNull(starter.start("route", contact()))
        gate.complete("cafe")
        first.join()
        assertEquals(1, calls)
        assertTrue(starter.starting.value.isEmpty())
    }

    @Test
    fun `a failed founding is reported and Message works again`() = runTest {
        var fail = true
        val starter = ContactChatStarter(found = {
            if (fail) error("directory down")
            MessageOutcome.Opened("f00d")
        }, logFailure = {})

        assertNull(starter.start("route", contact()))
        assertEquals(mapOf("route" to MessageNotice.START_FAILED), starter.notices.value)
        assertTrue(starter.starting.value.isEmpty())

        fail = false
        assertEquals("f00d", starter.start("route", contact()))
        assertTrue(starter.notices.value.isEmpty())
    }

    @Test
    fun `a request sent inside a group opens nothing and leaves no notice`() = runTest {
        val starter = ContactChatStarter(found = { MessageOutcome.RequestSent }, logFailure = {})
        assertNull(starter.start("route", contact()))
        assertTrue(starter.notices.value.isEmpty())
    }

    @Test
    fun `nothing sendable says so`() = runTest {
        val starter = ContactChatStarter(found = { MessageOutcome.CannotMessage }, logFailure = {})
        assertNull(starter.start("route", contact()))
        assertEquals(mapOf("route" to MessageNotice.CANNOT_MESSAGE), starter.notices.value)
    }

    @Test
    fun `a request that could not be sent says so and Message works again`() = runTest {
        var outcome: MessageOutcome = MessageOutcome.RequestFailed
        val starter = ContactChatStarter(found = { outcome }, logFailure = {})
        assertNull(starter.start("route", contact()))
        assertEquals(mapOf("route" to MessageNotice.REQUEST_FAILED), starter.notices.value)
        outcome = MessageOutcome.RequestSent
        assertNull(starter.start("route", contact()))
        assertTrue(starter.notices.value.isEmpty())
    }

    @Test
    fun `the line under Message`() {
        assertNull(foundLine(starting = true, notice = MessageNotice.START_FAILED, waiting = true))
        assertEquals("contacts.error.start_chat", foundLine(false, MessageNotice.START_FAILED, false)?.key)
        assertEquals("contacts.detail.request_failed", foundLine(false, MessageNotice.REQUEST_FAILED, false)?.key)
        val cannot = foundLine(false, MessageNotice.CANNOT_MESSAGE, false)!!
        assertEquals("contacts.detail.cannot_message", cannot.key)
        assertTrue(cannot.namesPerson)
        val sent = foundLine(false, null, waiting = true)!!
        assertEquals("contacts.detail.request_sent", sent.key)
        assertTrue(sent.namesPerson)
        assertNull(foundLine(false, null, false))
    }
}
