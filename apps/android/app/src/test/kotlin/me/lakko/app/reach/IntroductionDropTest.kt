// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import kotlinx.coroutines.runBlocking
import me.lakko.app.contacts.ProvenLink
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0060 section 4 — removing a contact forgets what they offered and
 * relayed, under every key they have had, so this device stops relaying their
 * record (the iOS `dropIntroductions` rule).
 */
class IntroductionDropTest {

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
    private val own = ByteArray(32) { 1 }
    private val carol = ByteArray(32) { 3 }
    private val carolNow = ByteArray(32) { 4 }
    private val dave = ByteArray(32) { 5 }

    private fun manager(store: ReachDocStore, links: List<ProvenLink> = emptyList()) =
        ReachManager(store = store, provenLinks = { links }).also { it.bindForTest(account, own) }

    @Test
    fun `a removed contact's offer and bundle are dropped, everyone else's kept`() = runBlocking {
        val store = MemDocs()
        val reach = manager(store)
        reach.keepIntroductionOffer(carol, byteArrayOf(1))
        reach.keepIntroductionsBundle(carol, byteArrayOf(2), 1uL)
        reach.keepIntroductionOffer(dave, byteArrayOf(3))

        assertTrue(reach.dropIntroductions(carol))

        val doc = store.docs.getValue(account)
        assertEquals(setOf(reachHex(dave)), doc.introOffers.keys)
        assertTrue(doc.introBundles.isEmpty())
        assertEquals(listOf(reachHex(byteArrayOf(3))), reach.offeredIntroductionRecordsHex())
        // Nothing left of theirs: nothing more to drop.
        assertFalse(reach.dropIntroductions(carol))
    }

    @Test
    fun `what a contact sent under a key from before a rotation is dropped too`() = runBlocking {
        val b64 = java.util.Base64.getEncoder()
        val link = ProvenLink("carol", b64.encodeToString(carol), b64.encodeToString(carolNow), provenAtMs = 1L)
        val store = MemDocs()
        val reach = manager(store, listOf(link))
        // Kept under the key they had then.
        reach.keepIntroductionOffer(carol, byteArrayOf(1))

        assertTrue(reach.dropIntroductions(carolNow))
        assertTrue(store.docs.getValue(account).introOffers.isEmpty())
    }

    @Test
    fun `the pure rule returns nothing when the document holds nothing of theirs`() {
        val doc = ReachDoc(introOffers = mapOf("aa" to "01"))
        assertNull(withoutIntroductionsFrom(doc, setOf("bb")))
        assertEquals(emptyMap<String, String>(), withoutIntroductionsFrom(doc, setOf("aa"))!!.introOffers)
    }
}
