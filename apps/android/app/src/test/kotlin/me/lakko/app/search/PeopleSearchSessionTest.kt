// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.search

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The one search field (`docs/ux/who-can-find-me.md` section 6.1): the field
 * asks the network on its own 500 ms after the last keystroke, from a text the
 * network can be asked with, never twice for one answered text, keeps what it
 * showed until the new answer arrives, and sends a contact request from a row
 * at once.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class PeopleSearchSessionTest {

    private fun person(name: String, reach: PersonReach = PersonReach.OPEN, keyByte: Int = 1) = PersonResult(
        keyB64 = "k$keyByte",
        keyHex = "%02x".format(keyByte).repeat(32),
        accountIdHex = "01".repeat(16),
        name = name,
        username = null,
        reach = reach,
        grantWireB64 = null,
    )

    private fun found(name: String) = PeopleSearchOutcome(listOf(person(name)), null, PeopleSearchState.FOUND)

    private class Harness(scope: TestScope, memory: PeopleSearchMemory = PeopleSearchMemory()) {
        val asked = mutableListOf<String>()
        var answer: (String) -> PeopleSearchOutcome = { _ ->
            PeopleSearchOutcome(emptyList(), null, PeopleSearchState.NOT_FOUND)
        }
        var gate: CompletableDeferred<Unit>? = null
        val sent = mutableListOf<String>()
        var sendOutcome: SendOutcome = SendOutcome.Sent
        var sendGate: CompletableDeferred<Unit>? = null
        val session = PeopleSearchSession(
            scope = scope,
            memory = memory,
            asks = { it.length >= 3 },
            search = { q ->
                asked += q
                gate?.await()
                answer(q)
            },
            send = { p ->
                sent += p.keyHex
                sendGate?.await()
                sendOutcome
            },
        )
    }

    @Test
    fun `a text shorter than three characters asks nothing and shows no heading`() = runTest {
        val h = Harness(this)
        h.session.onQuery("an")
        advanceTimeBy(2_000)
        runCurrent()
        assertTrue(h.asked.isEmpty())
        assertEquals(PeopleNetworkView.HIDDEN, h.session.view.value)
        assertEquals(PeopleSearchState.IDLE, h.session.view.value.state)
    }

    @Test
    fun `the field asks 500 ms after the last keystroke and only for the last text`() = runTest {
        val h = Harness(this)
        h.session.onQuery("ann")
        advanceTimeBy(300)
        h.session.onQuery("anna")
        advanceTimeBy(499)
        runCurrent()
        assertTrue("nothing is asked before the pause", h.asked.isEmpty())
        assertTrue(h.session.view.value.searching)
        assertEquals(PeopleSearchState.SEARCHING, h.session.view.value.state)
        advanceTimeBy(2)
        runCurrent()
        assertEquals(listOf("anna"), h.asked)
        assertFalse(h.session.view.value.searching)
        assertEquals(PeopleSearchState.NOT_FOUND, h.session.view.value.state)
    }

    @Test
    fun `an answered text is not asked again in the session`() = runTest {
        val h = Harness(this)
        h.answer = { found("Anna") }
        h.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        h.session.onQuery("annab")
        advanceTimeBy(600)
        runCurrent()
        h.session.onQuery("anna")
        runCurrent()
        assertEquals(listOf("anna", "annab"), h.asked)
        // Shown at once from what the session remembers, no spinner.
        assertFalse(h.session.view.value.searching)
        assertEquals(PeopleSearchState.FOUND, h.session.view.value.state)
        advanceTimeBy(2_000)
        runCurrent()
        assertEquals(listOf("anna", "annab"), h.asked)
    }

    @Test
    fun `the memory is shared by every field of the session`() = runTest {
        val memory = PeopleSearchMemory()
        val chats = Harness(this, memory)
        chats.answer = { found("Anna") }
        chats.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        val contacts = Harness(this, memory)
        contacts.session.onQuery("Anna")
        advanceTimeBy(600)
        runCurrent()
        assertTrue("letter case does not make a second search", contacts.asked.isEmpty())
        assertEquals(PeopleSearchState.FOUND, contacts.session.view.value.state)
    }

    @Test
    fun `a leading at sign is ignored`() = runTest {
        val h = Harness(this)
        h.session.onQuery("@anna")
        advanceTimeBy(600)
        runCurrent()
        assertEquals(listOf("anna"), h.asked)
        h.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        assertEquals(listOf("anna"), h.asked)
        h.session.onQuery("@an")
        assertEquals(PeopleNetworkView.HIDDEN, h.session.view.value)
    }

    @Test
    fun `a network failure is not remembered and typing again asks again`() = runTest {
        val h = Harness(this)
        h.answer = { PeopleSearchOutcome(emptyList(), null, PeopleSearchState.FAILED) }
        h.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        assertEquals(PeopleSearchState.FAILED, h.session.view.value.state)
        assertEquals(SEARCH_FAILED, searchStateMessage(h.session.view.value.state))
        h.session.onQuery("anna ")
        advanceTimeBy(600)
        runCurrent()
        assertEquals(listOf("anna", "anna"), h.asked)
    }

    @Test
    fun `out of searches is its own state, not remembered, never not found`() = runTest {
        val h = Harness(this)
        h.answer = { PeopleSearchOutcome(emptyList(), null, PeopleSearchState.ALLOWANCE_USED) }
        h.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        val state = h.session.view.value.state
        assertEquals(PeopleSearchState.ALLOWANCE_USED, state)
        assertEquals(SEARCH_OUT_OF_SEARCHES, searchStateMessage(state))
        h.session.onQuery("annA")
        advanceTimeBy(600)
        runCurrent()
        assertEquals(2, h.asked.size)
    }

    @Test
    fun `earlier results stay until the new answer arrives`() = runTest {
        val h = Harness(this)
        h.answer = { found("Anna") }
        h.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        val first = h.session.view.value.shown
        h.gate = CompletableDeferred()
        h.answer = { found("Annabel") }
        h.session.onQuery("annab")
        advanceTimeBy(600)
        runCurrent()
        assertEquals(listOf("anna", "annab"), h.asked)
        assertTrue(h.session.view.value.searching)
        assertSame(first, h.session.view.value.shown)
        h.gate?.complete(Unit)
        runCurrent()
        assertFalse(h.session.view.value.searching)
        assertEquals("Annabel", h.session.view.value.shown?.people?.single()?.name)
    }

    @Test
    fun `typing on does not cancel a question already asked, and its answer is remembered`() = runTest {
        val h = Harness(this)
        h.gate = CompletableDeferred()
        h.answer = { q -> found(q) }
        h.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        h.session.onQuery("annabel")
        h.gate?.complete(Unit)
        advanceTimeBy(600)
        runCurrent()
        assertEquals(listOf("anna", "annabel"), h.asked)
        assertEquals("annabel", h.session.view.value.shown?.people?.single()?.name)
        h.session.onQuery("anna")
        runCurrent()
        assertEquals(2, h.asked.size)
        assertEquals("anna", h.session.view.value.shown?.people?.single()?.name)
    }

    @Test
    fun `returning to a text in flight does not ask it twice`() = runTest {
        val h = Harness(this)
        h.gate = CompletableDeferred()
        h.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        h.session.onQuery("annab")
        h.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        assertEquals(listOf("anna"), h.asked)
        h.gate?.complete(Unit)
        runCurrent()
        assertFalse(h.session.view.value.searching)
    }

    @Test
    fun `add contact sends at once and the row reads request sent`() = runTest {
        val h = Harness(this)
        val anna = person("Anna")
        h.sendGate = CompletableDeferred()
        h.session.add(anna)
        runCurrent()
        assertEquals(RowSendState.SENDING, h.session.sendStates.value[anna.keyHex])
        assertEquals(RowAction.ADDING, personRowView(anna, h.session.sendStates.value[anna.keyHex]).action)
        // A second tap while it goes out sends nothing more.
        h.session.add(anna)
        h.sendGate?.complete(Unit)
        runCurrent()
        assertEquals(listOf(anna.keyHex), h.sent)
        val row = personRowView(anna, h.session.sendStates.value[anna.keyHex])
        assertEquals(RowAction.NONE, row.action)
        assertEquals(REQUEST_SENT, row.note)
        h.session.add(anna)
        runCurrent()
        assertEquals(1, h.sent.size)
    }

    @Test
    fun `a refusal leaves no button, a failure brings the button back`() = runTest {
        val h = Harness(this)
        val anna = person("Anna", keyByte = 1)
        val bob = person("Bob", keyByte = 2)
        h.sendOutcome = SendOutcome.Refused
        h.session.add(anna)
        runCurrent()
        val refused = personRowView(anna, h.session.sendStates.value[anna.keyHex])
        assertEquals(RowAction.NONE, refused.action)
        assertEquals(REQUEST_REFUSED, refused.note)

        h.sendOutcome = SendOutcome.Failed(REQUEST_FAILED)
        h.session.add(bob)
        runCurrent()
        val failed = personRowView(bob, h.session.sendStates.value[bob.keyHex])
        assertEquals(RowAction.ADD, failed.action)
        assertEquals(REQUEST_FAILED, failed.note)
        // Pressing again tries again.
        h.sendOutcome = SendOutcome.Sent
        h.session.add(bob)
        runCurrent()
        assertEquals(listOf(anna.keyHex, bob.keyHex, bob.keyHex), h.sent)
        assertEquals(RowSendState.SENT, h.session.sendStates.value[bob.keyHex])
    }

    @Test
    fun `a contact shows open and nobody sends to a contact or a hidden person`() = runTest {
        val h = Harness(this)
        val contact = person("Anna", PersonReach.CONTACT)
        val hidden = person("Bob", PersonReach.NONE, keyByte = 2)
        assertEquals(PersonRowView(RowAction.OPEN, null), personRowView(contact, null))
        assertEquals(PersonRowView(RowAction.NONE, null), personRowView(hidden, null))
        assertEquals(PersonRowView(RowAction.ADD, null), personRowView(person("Cy", PersonReach.GRANT, 3).copy(grantWireB64 = "Zw=="), null))
        h.session.add(contact)
        h.session.add(hidden)
        runCurrent()
        assertTrue(h.sent.isEmpty())
        assertNull(h.session.sendStates.value[contact.keyHex])
    }

    @Test
    fun `a contact who was sent a request shows open`() {
        val anna = person("Anna", PersonReach.CONTACT)
        assertEquals(RowAction.OPEN, personRowView(anna, RowSendState.SENT).action)
    }

    @Test
    fun `the bare text drops a leading at sign and surrounding spaces`() {
        assertEquals("anna", bareSearchText("  @anna "))
        assertEquals("anna weber", bareSearchText("anna weber"))
        assertEquals(searchCacheKey("anna weber"), searchCacheKey("Anna   Weber"))
    }
}
