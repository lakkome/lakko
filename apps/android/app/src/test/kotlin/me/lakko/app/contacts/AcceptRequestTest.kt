// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Accepting a contact request must complete, every time: the order of an
 * accept, what stands after each way it can fail, and the owed Welcome.
 */
class AcceptRequestTest {

    private val founding = LinkClaims.Founded(
        groupIdHex = "aa",
        welcomeB64 = "d2VsY29tZQ==",
        targets = listOf(LinkClaims.FoundedTarget("cGVlcg==", isRecipient = true)),
        delivered = false,
    )

    private class Fake(
        var held: LinkClaims.Founded? = null,
        var answerable: Boolean = true,
        var answerableThrows: Boolean = false,
        var prepareThrows: Boolean = false,
        var recordFails: Boolean = false,
        var deliverThrows: Boolean = false,
        val made: LinkClaims.Founded,
    ) : AcceptDeps {
        val steps = mutableListOf<String>()
        val notes = mutableListOf<String>()

        override suspend fun answerable(): Boolean {
            steps += "probe"
            if (answerableThrows) throw IllegalStateException("offline")
            return answerable
        }

        override fun noteFailure(reason: String) {
            notes += reason
        }

        override fun founded(): LinkClaims.Founded? = held

        override fun note(): LinkClaims.HeldNote? = null

        override suspend fun prepare(): LinkClaims.Founded {
            steps += "prepare"
            if (prepareThrows) throw IllegalStateException("no key package")
            return made
        }

        override fun record(founded: LinkClaims.Founded): Boolean {
            steps += "record"
            if (recordFails) return false
            held = founded
            return true
        }

        override suspend fun postNote(founded: LinkClaims.Founded): Boolean = true

        override suspend fun hold(founded: LinkClaims.Founded): Boolean {
            steps += "hold"
            return true
        }

        override suspend fun deliver(founded: LinkClaims.Founded) {
            steps += "deliver"
            if (deliverThrows) throw IllegalStateException("refused")
        }

        override suspend fun forward(note: LinkClaims.HeldNote): String? = error("an accept never forwards")

        override suspend fun finish(groupIdHex: String?) {
            steps += "ack"
        }
    }

    @Test
    fun `an accept founds, records, files the contact, writes the Welcome, and acks last`() = runTest {
        val fake = Fake(made = founding)
        assertEquals(AcceptOutcome.DONE, runAccept(fake))
        assertEquals(listOf("probe", "prepare", "record", "record", "hold", "deliver", "record", "ack"), fake.steps)
        assertTrue(fake.notes.isEmpty())
    }

    @Test
    fun `no grant that opens the mailbox fails with nothing founded and the request still listed`() = runTest {
        val fake = Fake(made = founding, answerable = false)
        assertEquals(AcceptOutcome.FAILED, runAccept(fake))
        // `hold` is what files the contact: no accepted contact without a chat.
        assertEquals(listOf("probe"), fake.steps)
        assertEquals(listOf("no-grant-opens-the-mailbox"), fake.notes)
    }

    @Test
    fun `a check that could not be made fails the same way`() = runTest {
        val fake = Fake(made = founding, answerableThrows = true)
        assertEquals(AcceptOutcome.FAILED, runAccept(fake))
        assertEquals(listOf("probe"), fake.steps)
        assertEquals(listOf("reach-check-failed"), fake.notes)
    }

    @Test
    fun `a founding that fails leaves nothing and accept works again`() = runTest {
        val fake = Fake(made = founding, prepareThrows = true)
        assertEquals(AcceptOutcome.FAILED, runAccept(fake))
        assertFalse("hold" in fake.steps)
        fake.prepareThrows = false
        assertEquals(AcceptOutcome.DONE, runAccept(fake))
    }

    @Test
    fun `a founding that could not be recorded sends nothing`() = runTest {
        val fake = Fake(made = founding, recordFails = true)
        assertEquals(AcceptOutcome.FAILED, runAccept(fake))
        assertFalse("hold" in fake.steps)
        assertFalse("deliver" in fake.steps)
    }

    @Test
    fun `a failed Welcome stays owed, the chat stands, and no ack goes before it`() = runTest {
        val fake = Fake(made = founding, deliverThrows = true)
        assertEquals(AcceptOutcome.WELCOME_OWED, runAccept(fake))
        assertTrue("the contact row shows the chat", "hold" in fake.steps)
        assertFalse("ack" in fake.steps)
        assertFalse(fake.held!!.delivered)
        assertEquals(listOf("welcome-owed"), fake.notes)
    }

    @Test
    fun `the retry delivers the same group, founds nothing and does not ask again`() = runTest {
        val fake = Fake(made = founding, deliverThrows = true)
        runAccept(fake)
        fake.steps.clear()
        fake.deliverThrows = false
        assertEquals(AcceptOutcome.DONE, runAccept(fake))
        assertEquals(listOf("hold", "deliver", "record", "ack"), fake.steps)
    }

    @Test
    fun `a delivered Welcome is not written again when only the ack is left`() = runTest {
        val fake = Fake(made = founding, held = founding.copy(notePosted = true, welcomeB64 = "", delivered = true))
        assertEquals(AcceptOutcome.DONE, runAccept(fake))
        assertEquals(listOf("hold", "ack"), fake.steps)
    }

    @Test
    fun `the owed record survives by person and goes when answered`() {
        val record = LinkClaimRecord("acct", InMemoryLinkClaimStorage())
        assertNull(record.owedAccept("p1"))
        assertTrue(record.setOwedAccept("p1", founding))
        assertTrue(record.setOwedAccept("p2", founding.copy(groupIdHex = "bb")))
        assertEquals(setOf("p1", "p2"), record.owedAcceptKeys().toSet())
        assertEquals(founding, record.owedAccept("p1"))
        record.forgetOwedAccept("p1")
        assertNull(record.owedAccept("p1"))
        assertEquals("bb", record.owedAccept("p2")?.groupIdHex)
    }
}
