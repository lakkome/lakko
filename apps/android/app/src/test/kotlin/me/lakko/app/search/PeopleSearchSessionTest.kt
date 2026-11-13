// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.search

import kotlinx.coroutines.CompletableDeferred
import java.util.Base64
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import me.lakko.app.contacts.PendingContact
import me.lakko.app.contacts.SentContactRequest
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

    private class Harness(
        scope: TestScope,
        memory: PeopleSearchMemory = PeopleSearchMemory(),
        asks: (String) -> Boolean = { it.length >= 3 },
    ) {
        val asked = mutableListOf<String>()
        var answer: (String) -> PeopleSearchOutcome = { _ ->
            PeopleSearchOutcome(emptyList(), null, PeopleSearchState.NOT_FOUND)
        }
        var gate: CompletableDeferred<Unit>? = null
        val sent = mutableListOf<String>()
        var sendOutcome: SendOutcome = SendOutcome.Sent
        var sendGate: CompletableDeferred<Unit>? = null
        val checked = mutableListOf<String>()
        var reach: (String) -> Boolean = { false }
        var reachGate: CompletableDeferred<Unit>? = null
        val dismissed = mutableListOf<String>()
        val session = PeopleSearchSession(
            scope = scope,
            memory = memory,
            asks = asks,
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
            reachCheck = { _, wire ->
                checked += wire
                reachGate?.await()
                reach(wire)
            },
            dismissSent = { p -> dismissed += p.keyHex },
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
    fun `two letters are not asked even when the rules allow them`() = runTest {
        // The core's minimum is 2: its rule alone would ask, and show the heading.
        val h = Harness(this, asks = { true })
        h.session.onQuery("ka")
        advanceTimeBy(2_000)
        runCurrent()
        assertTrue(h.asked.isEmpty())
        assertEquals(PeopleNetworkView.HIDDEN, h.session.view.value)
        h.session.onQuery("kaj")
        advanceTimeBy(600)
        runCurrent()
        assertEquals(listOf("kaj"), h.asked)
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
    fun `a contact removed with the text standing is asked again at once`() = runTest {
        val h = Harness(this)
        h.answer = { PeopleSearchOutcome(listOf(person("Anna", PersonReach.CONTACT)), null, PeopleSearchState.FOUND) }
        h.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        assertEquals(listOf("anna"), h.asked)

        h.answer = { found("Anna") }
        h.session.onContacts(emptySet())
        runCurrent()

        assertEquals(listOf("anna", "anna"), h.asked)
        assertEquals(PersonReach.OPEN, h.session.view.value.shown?.people?.single()?.reach)
    }

    @Test
    fun `a contact change that leaves the answer true asks nothing`() = runTest {
        val h = Harness(this)
        h.answer = { PeopleSearchOutcome(listOf(person("Anna", PersonReach.CONTACT)), null, PeopleSearchState.FOUND) }
        h.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        h.session.onContacts(setOf("k1"))
        runCurrent()
        assertEquals(listOf("anna"), h.asked)
    }

    @Test
    fun `a remembered answer about a removed contact is not reused`() = runTest {
        val h = Harness(this)
        h.answer = { q ->
            if (q == "anna") {
                PeopleSearchOutcome(listOf(person("Anna", PersonReach.CONTACT)), null, PeopleSearchState.FOUND)
            } else {
                found("Bert")
            }
        }
        h.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        h.session.onQuery("bert")
        advanceTimeBy(600)
        runCurrent()
        h.session.onContacts(emptySet())
        runCurrent()
        h.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        assertEquals(listOf("anna", "bert", "anna"), h.asked)
    }

    @Test
    fun `a person added through their link reads request sent`() {
        val anna = person("Anna")
        val invited = PendingContact(
            accountIdHex = "aa", signaturePubkeyB64 = Base64.getEncoder().encodeToString(ByteArray(32) { 1 }),
            x25519PubkeyB64 = "", displayName = "", username = "", addedAtMs = 0, linkIdHex = "",
        )
        assertTrue(OutgoingRequests.of(emptyList(), listOf(invited)).holds(anna))
        assertTrue(OutgoingRequests.of(emptyList(), listOf(invited)).holds(anna.copy(keyHex = "ff", accountIdHex = "aa")))
        assertFalse(OutgoingRequests.of(emptyList(), emptyList()).holds(anna))
    }

    private fun TestScope.agingMemory() = PeopleSearchMemory(nowMs = { testScheduler.currentTime })

    @Test
    fun `an answer is reused for its minute, then asked again`() = runTest {
        val h = Harness(this, agingMemory())
        h.answer = { found("Anna") }
        h.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        h.session.onQuery("")
        // Answered at 500, when the debounce ended.
        advanceTimeBy(500 + SEARCH_ANSWER_KEPT_MS - 1 - testScheduler.currentTime)
        h.session.onQuery("anna")
        runCurrent()
        assertEquals("still young", listOf("anna"), h.asked)
        assertFalse(h.session.view.value.searching)
        h.session.onQuery("")
        advanceTimeBy(1)
        h.session.onQuery("anna")
        assertTrue("old enough: on its way again", h.session.view.value.searching)
        advanceTimeBy(600)
        runCurrent()
        assertEquals(listOf("anna", "anna"), h.asked)
    }

    @Test
    fun `a person who widened their setting is found once a not-found ages`() = runTest {
        val h = Harness(this, agingMemory())
        h.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        assertEquals(PeopleSearchState.NOT_FOUND, h.session.view.value.state)
        h.answer = { found("Anna") }
        h.session.onQuery("")
        advanceTimeBy(SEARCH_ANSWER_KEPT_MS)
        h.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        assertEquals(listOf("anna", "anna"), h.asked)
        assertEquals(PeopleSearchState.FOUND, h.session.view.value.state)
    }

    @Test
    fun `an aged text still in flight is not asked twice`() = runTest {
        val h = Harness(this, agingMemory())
        h.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        h.session.onQuery("")
        advanceTimeBy(SEARCH_ANSWER_KEPT_MS)
        h.gate = CompletableDeferred()
        h.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        h.session.onQuery("annab")
        h.session.onQuery("anna")
        advanceTimeBy(600)
        runCurrent()
        assertEquals(listOf("anna", "anna"), h.asked)
        h.gate?.complete(Unit)
        runCurrent()
        assertFalse(h.session.view.value.searching)
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

    private fun held(key: String, vararg wires: String) = LocalCandidate(
        keyB64 = key,
        keyHex = key,
        name = "Anna",
        verifiedUsername = "anna_w",
        isContact = false,
        accountIdHex = "01".repeat(16),
        keptGrantWiresB64 = wires.toList(),
    )

    @Test
    fun `a stale grant then a live one gives the live one`() = runTest {
        val h = Harness(this)
        h.reach = { it == "LIVE" }
        h.session.checkKeptGrants(listOf(held("a", "STALE", "LIVE", "NEVER")))
        runCurrent()
        assertEquals("stops at the first that passes", listOf("STALE", "LIVE"), h.checked)
        assertEquals(KeptGrantCheck.Live("LIVE"), h.session.keptGrantChecks.value["a"])
    }

    @Test
    fun `every grant refused is dead`() = runTest {
        val h = Harness(this)
        h.session.checkKeptGrants(listOf(held("a", "G1", "G2")))
        runCurrent()
        assertEquals(listOf("G1", "G2"), h.checked)
        assertEquals(KeptGrantCheck.Dead, h.session.keptGrantChecks.value["a"])
    }

    @Test
    fun `a check that throws records no verdict and is asked again`() = runTest {
        val h = Harness(this)
        h.reach = { throw IllegalStateException("offline") }
        val anna = held("a", "G1", "G2")
        h.session.checkKeptGrants(listOf(anna))
        runCurrent()
        val checks = h.session.keptGrantChecks.value
        assertEquals(KeptGrantCheck.Unreached("G1"), checks["a"])
        h.reach = { true }
        h.session.checkKeptGrants(listOf(anna))
        runCurrent()
        assertEquals(KeptGrantCheck.Live("G1"), h.session.keptGrantChecks.value["a"])
    }

    @Test
    fun `every text asks again, live and dead alike`() = runTest {
        val h = Harness(this)
        val live = held("l", "G1")
        val dead = held("d", "G2")
        h.reach = { it == "G1" }
        h.session.checkKeptGrants(listOf(live, dead))
        runCurrent()
        assertEquals(KeptGrantCheck.Live("G1"), h.session.keptGrantChecks.value["l"])
        assertEquals(KeptGrantCheck.Dead, h.session.keptGrantChecks.value["d"])
        // The next text: the person left, the other came back.
        h.reach = { it == "G2" }
        h.session.checkKeptGrants(
            keptGrantsToCheck(withKeptGrantChecks(listOf(live, dead), h.session.keptGrantChecks.value), "ann", ::fallbackNormalize, null),
        )
        runCurrent()
        assertEquals(KeptGrantCheck.Dead, h.session.keptGrantChecks.value["l"])
        assertEquals(KeptGrantCheck.Live("G2"), h.session.keptGrantChecks.value["d"])
    }

    @Test
    fun `the last answer stays until the new one arrives`() = runTest {
        val h = Harness(this)
        val anna = held("a", "G1")
        h.reach = { true }
        h.session.checkKeptGrants(listOf(anna))
        runCurrent()
        h.reachGate = CompletableDeferred()
        h.reach = { false }
        h.session.checkKeptGrants(listOf(anna))
        runCurrent()
        assertEquals(KeptGrantCheck.Live("G1"), h.session.keptGrantChecks.value["a"])
        h.reachGate?.complete(Unit)
        runCurrent()
        assertEquals(KeptGrantCheck.Dead, h.session.keptGrantChecks.value["a"])
    }

    @Test
    fun `a check that could not be made changes no verdict`() = runTest {
        val h = Harness(this)
        val anna = held("a", "G1")
        h.session.checkKeptGrants(listOf(anna))
        runCurrent()
        assertEquals(KeptGrantCheck.Dead, h.session.keptGrantChecks.value["a"])
        h.reach = { throw IllegalStateException("offline") }
        h.session.checkKeptGrants(listOf(anna))
        runCurrent()
        assertEquals(KeptGrantCheck.Dead, h.session.keptGrantChecks.value["a"])
    }

    @Test
    fun `a refusal before a throw offers the grant that was not refused`() = runTest {
        assertEquals(
            KeptGrantCheck.Unreached("G2"),
            checkGrantsInOrder(listOf("G1", "G2", "G3")) { wire ->
                if (wire == "G1") false else throw IllegalStateException("offline")
            },
        )
    }

    @Test
    fun `one person is never checked twice at once`() = runTest {
        val h = Harness(this)
        h.reachGate = CompletableDeferred()
        h.reach = { true }
        val anna = held("a", "G1")
        h.session.checkKeptGrants(listOf(anna))
        runCurrent()
        h.session.checkKeptGrants(listOf(anna))
        runCurrent()
        assertEquals(listOf("G1"), h.checked)
        h.reachGate?.complete(Unit)
        runCurrent()
        assertEquals(KeptGrantCheck.Live("G1"), h.session.keptGrantChecks.value["a"])
    }

    @Test
    fun `a check waits for no debounce and no minimum length, and typing on does not cancel it`() = runTest {
        val h = Harness(this)
        h.reachGate = CompletableDeferred()
        h.reach = { true }
        h.session.onQuery("a")
        h.session.checkKeptGrants(listOf(held("a", "G1")))
        runCurrent()
        assertEquals(listOf("G1"), h.checked)
        h.session.onQuery("an")
        h.reachGate?.complete(Unit)
        runCurrent()
        assertEquals(KeptGrantCheck.Live("G1"), h.session.keptGrantChecks.value["a"])
    }

    @Test
    fun `only so many people are checked at one time`() = runTest {
        val h = Harness(this)
        h.reachGate = CompletableDeferred()
        h.session.checkKeptGrants((0 until 20).map { held("p$it", "G$it") })
        runCurrent()
        assertEquals(KEPT_GRANT_CHECKS_AT_ONCE, h.checked.size)
        h.reachGate?.complete(Unit)
        runCurrent()
        assertEquals(20, h.checked.size)
        assertEquals(20, h.session.keptGrantChecks.value.size)
    }

    @Test
    fun `an emptied field forgets the verdicts`() = runTest {
        val h = Harness(this)
        h.session.onQuery("ann")
        h.session.checkKeptGrants(listOf(held("a", "G1")))
        runCurrent()
        assertEquals(KeptGrantCheck.Dead, h.session.keptGrantChecks.value["a"])
        h.session.onQuery("an")
        assertEquals("a shorter text keeps them", KeptGrantCheck.Dead, h.session.keptGrantChecks.value["a"])
        h.session.onQuery("")
        assertTrue(h.session.keptGrantChecks.value.isEmpty())
    }

    @Test
    fun `a send the reach check refuses records dead, and the row keeps only the refusal text`() = runTest {
        val h = Harness(this)
        val anna = person("Anna", PersonReach.GRANT, keyByte = 1).copy(grantWireB64 = "Zw==", username = "anna_w")
        h.sendOutcome = SendOutcome.ReachRefused
        h.session.add(anna)
        runCurrent()
        assertEquals(KeptGrantCheck.Dead, h.session.keptGrantChecks.value[anna.keyHex])
        val row = personRowView(anna, h.session.sendStates.value[anna.keyHex])
        assertEquals(RowAction.NONE, row.action)
        assertEquals(REQUEST_REFUSED, row.note)
    }

    @Test
    fun `a refusal for any other reason records no verdict`() = runTest {
        val h = Harness(this)
        val anna = person("Anna", PersonReach.GRANT, keyByte = 1).copy(grantWireB64 = "Zw==")
        h.sendOutcome = SendOutcome.Refused
        h.session.add(anna)
        runCurrent()
        assertTrue(h.session.keptGrantChecks.value.isEmpty())
        assertEquals(RowSendState.REFUSED, h.session.sendStates.value[anna.keyHex])
    }

    // --- what a row says stays true ------------------------------------------

    private fun granted(name: String, keyByte: Int) =
        person(name, PersonReach.GRANT, keyByte).copy(grantWireB64 = "Zw==")

    @Test
    fun `a refused or failed line lasts until the text changes, then the person can be asked again`() = runTest {
        val h = Harness(this)
        val anna = granted("Anna", 1)
        val bob = granted("Bob", 2)
        h.session.onQuery("ann")
        h.sendOutcome = SendOutcome.ReachRefused
        h.session.add(anna)
        runCurrent()
        h.sendOutcome = SendOutcome.Failed(REQUEST_FAILED)
        h.session.add(bob)
        runCurrent()
        h.session.onQuery("ann")
        assertEquals("the same text changes nothing", RowSendState.REFUSED, h.session.sendStates.value[anna.keyHex])
        assertEquals(RowSendState.FAILED, h.session.sendStates.value[bob.keyHex])
        h.session.onQuery("anna")
        assertTrue(h.session.sendStates.value.isEmpty())
        h.sendOutcome = SendOutcome.Sent
        h.session.add(anna)
        runCurrent()
        assertEquals(listOf(anna.keyHex, bob.keyHex, anna.keyHex), h.sent)
    }

    @Test
    fun `a new field on the same text keeps the row's line`() = runTest {
        val memory = PeopleSearchMemory()
        val first = Harness(this, memory)
        val anna = granted("Anna", 1)
        first.session.onQuery("ann")
        first.sendOutcome = SendOutcome.Refused
        first.session.add(anna)
        runCurrent()
        // A rotation builds the session anew: the text did not change.
        Harness(this, memory).session.onQuery("ann")
        assertEquals(RowSendState.REFUSED, memory.sendStates.value[anna.keyHex])
    }

    @Test
    fun `a change of text leaves a request on its way and a sent one`() = runTest {
        val h = Harness(this)
        val anna = granted("Anna", 1)
        val bob = granted("Bob", 2)
        h.session.add(bob)
        runCurrent()
        h.sendGate = CompletableDeferred()
        h.session.add(anna)
        runCurrent()
        h.session.onQuery("a")
        h.session.onQuery("an")
        assertEquals(RowSendState.SENDING, h.session.sendStates.value[anna.keyHex])
        assertEquals(RowSendState.SENT, h.session.sendStates.value[bob.keyHex])
        h.sendGate?.complete(Unit)
        runCurrent()
    }

    private fun sentTo(person: PersonResult, key: Boolean = true) = SentContactRequest(
        recipientUsername = "anna",
        recipientAccountIdB64 = Base64.getEncoder().encodeToString(ByteArray(16) { 1 }),
        messageId = 1L,
        createdAt = "2026-10-30T09:00:00Z",
        recipientKeyB64 = person.takeIf { key }?.let {
            Base64.getEncoder().encodeToString(ByteArray(32) { 1 })
        },
    )

    @Test
    fun `request sent reads the outgoing list, by key or by account id`() {
        val anna = granted("Anna", 1)
        val rotated = anna.copy(keyHex = "%02x".format(9).repeat(32))
        val stranger = rotated.copy(accountIdHex = "02".repeat(16))
        val byKey = OutgoingRequests.of(listOf(sentTo(anna)))
        assertEquals(RowSendState.SENT, rowSendState(anna, null, byKey))
        assertEquals(RowSendState.SENT, rowSendState(rotated, null, byKey))
        assertEquals(RowSendState.SENT, rowSendState(anna, RowSendState.REFUSED, byKey))
        assertNull(rowSendState(stranger, null, byKey))
        val noKey = OutgoingRequests.of(listOf(sentTo(anna, key = false)))
        assertEquals(RowSendState.SENT, rowSendState(anna, null, noKey))
    }

    @Test
    fun `out of the list the row goes back to what the check says`() {
        val anna = granted("Anna", 1)
        val gone = OutgoingRequests.NONE
        assertNull(rowSendState(anna, RowSendState.SENT, gone))
        assertEquals(RowAction.ADD, personRowView(anna, rowSendState(anna, RowSendState.SENT, gone)).action)
        assertEquals(RowSendState.SENDING, rowSendState(anna, RowSendState.SENDING, gone))
        assertEquals(RowSendState.FAILED, rowSendState(anna, RowSendState.FAILED, gone))
    }

    @Test
    fun `a value that is not base64 names nobody in the list`() {
        val broken = sentTo(granted("Anna", 1)).copy(recipientAccountIdB64 = "not base64!", recipientKeyB64 = "%%")
        assertEquals(OutgoingRequests.NONE, OutgoingRequests.of(listOf(broken)))
    }

    @Test
    fun `a person dismissed from the list can be asked again`() = runTest {
        val h = Harness(this)
        val anna = granted("Anna", 1)
        h.session.add(anna)
        runCurrent()
        h.session.onOutgoing(OutgoingRequests.of(listOf(sentTo(anna))))
        assertEquals(RowSendState.SENT, h.session.sendStates.value[anna.keyHex])
        h.session.add(anna)
        runCurrent()
        assertEquals("while in the list nothing goes out twice", listOf(anna.keyHex), h.sent)
        h.session.onOutgoing(OutgoingRequests.NONE)
        assertNull(h.session.sendStates.value[anna.keyHex])
        h.session.add(anna)
        runCurrent()
        assertEquals(listOf(anna.keyHex, anna.keyHex), h.sent)
    }

    @Test
    fun `the bare text drops a leading at sign and surrounding spaces`() {
        assertEquals("anna", bareSearchText("  @anna "))
        assertEquals("anna weber", bareSearchText("anna weber"))
        assertEquals(searchCacheKey("anna weber"), searchCacheKey("Anna   Weber"))
    }

    @Test
    fun `right after add contact the row reads request sent with no ask again`() {
        // docs/ux/who-can-find-me.md section 6.1: the row reads Request sent
        // and the button goes away; Ask again comes after a day unanswered.
        val anna = granted("Anna", 1)
        val now = 1_794_312_000_000L
        val list = OutgoingRequests.of(listOf(sentTo(anna).copy(sentAtMs = now)))
        assertEquals(RowSendState.SENT, rowSendState(anna, null, list))
        assertFalse(list.canAskAgain(anna, now))
        assertFalse(list.canAskAgain(anna, now + me.lakko.app.chat.ASK_AGAIN_AFTER_MS - 1))
        assertTrue(list.canAskAgain(anna, now + me.lakko.app.chat.ASK_AGAIN_AFTER_MS))
        // The newest request decides.
        val twice = OutgoingRequests.of(
            listOf(sentTo(anna).copy(sentAtMs = now - 2 * me.lakko.app.chat.ASK_AGAIN_AFTER_MS), sentTo(anna).copy(messageId = 2L, sentAtMs = now)),
        )
        assertFalse(twice.canAskAgain(anna, now))
    }

    @Test
    fun `a row with a standing request reads sent and offers ask again`() {
        val anna = person("Anna", PersonReach.OPEN, 1).copy(username = "anna")
        val sent = OutgoingRequests(setOf(anna.keyHex), emptySet(), sentAtMsByIdHex = mapOf(anna.keyHex to 0L))
        assertTrue(sent.canAskAgain(anna, me.lakko.app.chat.ASK_AGAIN_AFTER_MS))
        assertEquals(
            PersonRowView(RowAction.ASK_AGAIN, REQUEST_SENT),
            personRowView(anna, RowSendState.SENT, canAskAgain = true),
        )
        assertEquals(PersonRowView(RowAction.NONE, REQUEST_SENT), personRowView(anna, RowSendState.SENT))
    }

    @Test
    fun `someone added through an invite link is not asked again`() {
        val anna = person("Anna", PersonReach.OPEN, 1).copy(username = "anna")
        val id = anna.accountIdHex!!
        val invited = OutgoingRequests(setOf(anna.keyHex), setOf(id), invitedAccountIdsHex = setOf(id))
        assertTrue(invited.holds(anna))
        assertFalse(invited.canAskAgain(anna, Long.MAX_VALUE))
    }

    @Test
    fun `ask again dismisses the request and sends it in one tap`() = runTest {
        val h = Harness(this)
        val anna = person("Anna", PersonReach.OPEN, 1).copy(username = "anna")
        h.session.add(anna)
        runCurrent()
        assertEquals(RowSendState.SENT, h.session.sendStates.value[anna.keyHex])
        h.session.askAgain(anna)
        runCurrent()
        assertEquals(listOf(anna.keyHex), h.dismissed)
        assertEquals(listOf(anna.keyHex, anna.keyHex), h.sent)
        assertEquals(RowSendState.SENT, h.session.sendStates.value[anna.keyHex])
    }
}
