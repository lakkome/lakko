// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0074 section 3.5 - who is the same person across a PROVEN rotation,
 * and that an unproven key change never gets predecessor treatment.
 */
class ProvenPredecessorsTest {
    private val alice = "acct-alice"
    private val bob = "acct-bob"

    private fun proven(account: String, old: String, new: String, at: Long = 1L): ProvenLink =
        provenLinkFor(KeyChangeEvidence.ProofVerified, account, old, new, at)!!

    @Test
    fun `only a verified proof makes the old key a predecessor`() {
        assertEquals(ProvenLink(alice, "K1", "K2", 5L), provenLinkFor(KeyChangeEvidence.ProofVerified, alice, "K1", "K2", 5L))
        // A reset, a stripped proof, a refused proof: never.
        assertNull(provenLinkFor(KeyChangeEvidence.Unproven, alice, "K1", "K2", 5L))
        assertNull(provenLinkFor(KeyChangeEvidence.ProofRefused, alice, "K1", "K2", 5L))
        // And never a key over itself or an empty key.
        assertNull(provenLinkFor(KeyChangeEvidence.ProofVerified, alice, "K1", "K1", 5L))
        assertNull(provenLinkFor(KeyChangeEvidence.ProofVerified, alice, "", "K2", 5L))
    }

    @Test
    fun `a proven rotation is the same person in both directions`() {
        val links = listOf(proven(alice, "K1", "K2"))
        assertTrue(samePerson("K1", "K2", links))
        assertTrue(samePerson("K2", "K1", links))
        assertTrue(samePerson("K1", "K1", links))
        // Nothing relates a stranger's key to either.
        assertFalse(samePerson("K1", "X", links))
        assertFalse(samePerson("K2", "X", links))
    }

    @Test
    fun `an unproven change is two people even under one account id`() {
        // The account id was the same; nothing proved K1 -> K9.
        val links = emptyList<ProvenLink>()
        assertFalse(samePerson("K1", "K9", links))
        assertEquals("K1", currentKeyOf("K1", links))
        assertEquals("K9", earliestKeyOf("K9", links))
        assertFalse(bindingIsOfAccount(boundKeyB64 = "K1", accountKeyB64 = "K9", links = links))
    }

    @Test
    fun `the predecessor resolves to the current key through every proven step`() {
        val links = listOf(proven(alice, "K1", "K2"), proven(alice, "K2", "K3"))
        assertEquals("K3", currentKeyOf("K1", links))
        assertEquals("K3", currentKeyOf("K2", links))
        assertEquals("K3", currentKeyOf("K3", links))
        assertEquals("K1", earliestKeyOf("K3", links))
        assertEquals(listOf("K1", "K2", "K3"), keysOfPerson("K2", links))
        assertTrue(samePerson("K1", "K3", links))
    }

    @Test
    fun `a leaf bound to a proven predecessor is the account's, a later key's is not an earlier account's`() {
        val links = listOf(proven(alice, "K1", "K2"))
        // The pinned key is K2; an existing binding under K1 stays valid.
        assertTrue(bindingIsOfAccount("K1", "K2", links))
        assertTrue(bindingIsOfAccount("K2", "K2", links))
        // The reverse is not a predecessor relation.
        assertFalse(bindingIsOfAccount("K2", "K1", links))
        assertFalse(bindingIsOfAccount("X", "K2", links))
    }

    @Test
    fun `names stored under the first key answer for every later key`() {
        val links = listOf(proven(alice, "K1", "K2"), proven(alice, "K2", "K3"), proven(bob, "B1", "B2"))
        assertEquals(mapOf("K2" to "K1", "K3" to "K1", "B2" to "B1"), provenAliases(links))
    }

    @Test
    fun `a key history is a chain per account, never a graph`() {
        val one = listOf(proven(alice, "K1", "K2"))
        // The same step again changes nothing.
        assertEquals(one, withProvenLink(one, proven(alice, "K1", "K2", at = 99L)))
        // A second successor of the same key is refused.
        assertEquals(one, withProvenLink(one, proven(alice, "K1", "K7")))
        // A cycle is refused.
        assertEquals(one, withProvenLink(one, proven(alice, "K2", "K1")))
        // Another account cannot be linked into this one's keys.
        assertEquals(one, withProvenLink(one, proven(bob, "K2", "B9")))
        assertEquals(one, withProvenLink(one, proven(bob, "B0", "K1")))
        // The next step of the chain is taken.
        assertEquals(2, withProvenLink(one, proven(alice, "K2", "K3")).size)
    }

    @Test
    fun `a reset ends every predecessor the account had`() {
        val links = listOf(proven(alice, "K1", "K2"), proven(alice, "K2", "K3"), proven(bob, "B1", "B2"))
        val after = withoutLinksOf(links, alice)
        assertEquals(listOf(proven(bob, "B1", "B2")), after)
        assertFalse(samePerson("K1", "K3", after))
        assertFalse(bindingIsOfAccount("K1", "K3", after))
        // Bob is untouched.
        assertTrue(samePerson("B1", "B2", after))
    }

    @Test
    fun `the gate's record and a reset's drop reach the store and the listener`() {
        val store = InMemoryProvenLinkStore()
        var told = 0
        ProvenPredecessors.onChanged = { told += 1 }
        try {
            ProvenPredecessors.recordProven(store, "me", alice, "K1", "K2", nowMs = 3L)
            assertEquals(listOf(ProvenLink(alice, "K1", "K2", 3L)), store.load("me"))
            assertEquals(1, told)
            // A re-observation of the same step is not a change.
            ProvenPredecessors.recordProven(store, "me", alice, "K1", "K2", nowMs = 9L)
            assertEquals(1, told)
            // Another signed-in account on this phone sees none of it.
            assertTrue(store.load("someone-else").isEmpty())
            ProvenPredecessors.dropAccount(store, "me", alice)
            assertTrue(store.load("me").isEmpty())
            assertEquals(2, told)
            // Dropping what is not there tells nobody.
            ProvenPredecessors.dropAccount(store, "me", alice)
            assertEquals(2, told)
        } finally {
            ProvenPredecessors.onChanged = null
        }
    }

    // --- ADR-0074 section 3.5a rule 1, for what is sealed to a key ----------

    @Test
    fun `a contact request is never sent for a key that resolves to another`() {
        val old = ByteArray(32) { 1 }
        val current = ByteArray(32) { 2 }
        // The key in hand is the current one (or nothing is known): send.
        assertFalse(staleRecipientKey(current, current.copyOf()))
        // The key in hand is a proven predecessor: the request would be
        // sealed for a mailbox nobody is addressed at any more.
        assertTrue(staleRecipientKey(old, current))
    }

    // --- ADR-0074 section 3.5a rule 5 / people search: which key came first --

    @Test
    fun `a key precedes only the keys its own proven steps lead to`() {
        val links = withProvenLink(listOf(proven(alice, "K1", "K2")), proven(alice, "K2", "K3"))
        assertTrue(keyPrecedes("K1", "K2", links))
        assertTrue(keyPrecedes("K1", "K3", links))
        assertTrue(keyPrecedes("K2", "K3", links))
        // Never backwards, never itself.
        assertFalse(keyPrecedes("K3", "K1", links))
        assertFalse(keyPrecedes("K2", "K2", links))
        // Never across what nothing proved, and never to another account.
        assertFalse(keyPrecedes("K1", "K9", links))
        assertFalse(keyPrecedes("K1", "K2", emptyList()))
        val withBob = withProvenLink(links, proven(bob, "B1", "B2"))
        assertFalse(keyPrecedes("K1", "B2", withBob))
    }

    @Test
    fun `a refused mailbox write is recognised bare or wrapped, and nothing else is`() {
        val refusal = me.lakko.app.ds.DsReachRefusedError("/ds/mailbox", "")
        assertTrue(isMailboxRefusal(refusal))
        assertTrue(isMailboxRefusal(ContactsError.wrap(refusal)))
        assertTrue(isMailboxRefusal(ContactsError.reachRefused(refusal)))
        assertFalse(isMailboxRefusal(ContactsError.wrap(java.io.IOException("down"))))
        assertFalse(isMailboxRefusal(ContactsError.usernameUnknown("anna")))
        assertFalse(isMailboxRefusal(java.io.IOException("down")))
    }

    @Test
    fun `a request that cannot be sent again is dropped only when the person is gone`() {
        val refusal = me.lakko.app.ds.DsReachRefusedError("/ds/mailbox", "")
        val gone = me.lakko.app.reach.RequestResend.GONE
        val failed = me.lakko.app.reach.RequestResend.FAILED
        assertEquals(gone, requestResendOutcome(ContactsError.wrap(refusal)))
        assertEquals(gone, requestResendOutcome(ContactsError.reachRefused(refusal)))
        assertEquals(gone, requestResendOutcome(ContactsError.noIdentityBundle(ByteArray(16))))
        assertEquals(
            gone,
            requestResendOutcome(SupersessionRefused(SupersessionRefusedReason.ProofAbsent, "acct", "refused")),
        )
        // The network: the entry stays and the rotation's step stays owed.
        assertEquals(failed, requestResendOutcome(ContactsError.wrap(java.io.IOException("down"))))
        assertEquals(failed, requestResendOutcome(IllegalStateException("no session")))
    }

    @Test
    fun `the ledger rows to revoke are the recorded ones, each once`() {
        // ADR-0078 section 10: no owner list adds the other devices' links.
        assertEquals(listOf("a", "b", "c"), spentLinkSlugs(listOf("a", "b", "b", "c")))
        assertTrue(spentLinkSlugs(emptyList()).isEmpty())
    }
}
