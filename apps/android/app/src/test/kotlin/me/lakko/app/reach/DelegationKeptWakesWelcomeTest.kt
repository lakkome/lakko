// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import me.lakko.app.contacts.ProvenLink
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * ADR-0060 section 6 — a fresh committer envelope wakes a Welcome already
 * owed: a committer that merged the commit may hold only an expired
 * delegation, and the chat repository files an owed Welcome under the added
 * person's CURRENT key, so that is the key this must name.
 */
class DelegationKeptWakesWelcomeTest {

    private class MemDocs : ReachDocStore {
        val docs = mutableMapOf<String, ReachDoc>()
        override fun load(accountIdB64: String) = docs[accountIdB64] ?: ReachDoc()
        override fun save(accountIdB64: String, doc: ReachDoc) {
            docs[accountIdB64] = doc
        }

        override fun delete(accountIdB64: String) {
            docs.remove(accountIdB64)
        }
    }

    private val account = "acct"
    private val group = "ab".repeat(32)
    private val own = ByteArray(32) { 1 }
    private val sender = ByteArray(32) { 2 }
    private val carol = ByteArray(32) { 3 }

    private fun manager(store: ReachDocStore, added: ByteArray, links: List<ProvenLink> = emptyList()) =
        ReachManager(
            store = store,
            provenLinks = { links },
            delegationAddedKey = { added },
        ).also { it.bindForTest(account, own) }

    private fun wakeOn(reach: ReachManager): CompletableDeferred<ByteArray> {
        val woken = CompletableDeferred<ByteArray>()
        reach.onGrantHeld = { woken.complete(it) }
        return woken
    }

    @Test
    fun `an arriving envelope wakes the owed Welcome of the person it names`() = runBlocking {
        val store = MemDocs()
        val reach = manager(store, added = carol)
        val woken = wakeOn(reach)

        reach.keepDelegation(group, sender, byteArrayOf(9, 9))

        assertArrayEquals(carol, withTimeout(5_000) { woken.await() })
        val kept = store.docs.getValue(account).delegations.single()
        assertEquals(reachHex(byteArrayOf(9, 9)), kept.envelopeHex)
    }

    @Test
    fun `an envelope naming a predecessor wakes the key the Welcome is owed under`() = runBlocking {
        val carolNow = ByteArray(32) { 4 }
        val b64 = java.util.Base64.getEncoder()
        val link = ProvenLink("carol", b64.encodeToString(carol), b64.encodeToString(carolNow), provenAtMs = 1L)
        val reach = manager(MemDocs(), added = carol, links = listOf(link))
        val woken = wakeOn(reach)

        reach.keepDelegation(group, sender, byteArrayOf(9))

        assertArrayEquals(carolNow, withTimeout(5_000) { woken.await() })
    }

    @Test
    fun `a re-sent envelope replaces the last one from that sender and wakes again`() = runBlocking {
        val store = MemDocs()
        val reach = manager(store, added = carol)
        val first = wakeOn(reach)
        reach.keepDelegation(group, sender, byteArrayOf(1))
        withTimeout(5_000) { first.await() }

        val second = wakeOn(reach)
        reach.keepDelegation(group, sender, byteArrayOf(2))
        withTimeout(5_000) { second.await() }

        assertEquals(reachHex(byteArrayOf(2)), store.docs.getValue(account).delegations.single().envelopeHex)
    }

    @Test
    fun `an envelope that does not decode wakes nothing and is not kept`() {
        val store = MemDocs()
        val reach = ReachManager(store = store, delegationAddedKey = { error("not an envelope") })
        reach.bindForTest(account, own)
        var woken = false
        reach.onGrantHeld = { woken = true }

        reach.keepDelegation(group, sender, byteArrayOf(1))

        assertEquals(false, woken)
        assertEquals(null, store.docs[account])
    }

    @Test
    fun `an envelope from another device of this account is kept like any other`() = runBlocking {
        // Device B proposed the Add and holds the grant; this device sweeps.
        // Its only way to the person is what B sent, signed by the same
        // account key this device holds.
        val store = MemDocs()
        val reach = manager(store, added = carol)
        val woken = wakeOn(reach)

        reach.keepDelegation(group, own, byteArrayOf(9))

        assertArrayEquals(carol, withTimeout(5_000) { woken.await() })
        assertEquals(reachHex(own), store.docs.getValue(account).delegations.single().senderKeyHex)
    }

    @Test
    fun `the receive gate asks only that the sender is a person of the group`() {
        val self = "own-person-key"
        val members = listOf(self, "someone")
        assertEquals("this account's other device", true, keepsCommitterDelegation(self, members))
        assertEquals(true, keepsCommitterDelegation("someone", members))
        assertEquals("not a member", false, keepsCommitterDelegation("stranger", members))
        assertEquals("sender not resolved", false, keepsCommitterDelegation(null, members))
    }

    @Test
    fun `the same open envelope in two groups is kept once for each`() = runBlocking {
        // A no-delegation envelope carries the same bytes in every group it
        // goes to: nothing may treat the second group's as a repeat.
        val store = MemDocs()
        val reach = manager(store, added = carol)
        val other = "cd".repeat(32)
        val bytes = byteArrayOf(9, 9)

        val first = wakeOn(reach)
        reach.keepDelegation(group, sender, bytes)
        withTimeout(5_000) { first.await() }
        val second = wakeOn(reach)
        reach.keepDelegation(other, sender, bytes)
        withTimeout(5_000) { second.await() }

        assertEquals(setOf(group, other), store.docs.getValue(account).delegations.map { it.groupHex }.toSet())
    }

    @Test
    fun `an envelope from another own device touches no record of what this device sent`() = runBlocking {
        // This device's own record of an Add is its open binding, beside the
        // kept envelopes, and it is what its own sweep reads.
        val ownRecord = StoredOpenBinding(group, reachHex(carol), "aa")
        val store = MemDocs()
        store.docs[account] = ReachDoc(openBindings = listOf(ownRecord))
        val reach = manager(store, added = carol)
        val woken = wakeOn(reach)

        reach.keepDelegation(group, own, byteArrayOf(9))
        withTimeout(5_000) { woken.await() }

        val doc = store.docs.getValue(account)
        assertEquals(listOf(ownRecord), doc.openBindings)
        assertEquals(1, doc.delegations.size)
    }
}
