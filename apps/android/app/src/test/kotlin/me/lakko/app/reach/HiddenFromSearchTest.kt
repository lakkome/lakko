// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import me.lakko.app.discovery.DiscoveryHttpError
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0098 step 5 — discovery refuses the Everyone record with
 * `403 hidden-from-search` while moderators keep the account out of search:
 * Who can find me says so and Everyone cannot be chosen; the state is kept
 * for the account, is not a failure to retry, and ends when a later
 * Everyone publication lands.
 */
class HiddenFromSearchTest {

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

    private fun manager(store: ReachDocStore) =
        ReachManager(store = store, delegationAddedKey = { error("not an envelope") })
            .also { it.bindForTest(account, own) }

    private fun awaitDoc(store: MemDocs, done: (ReachDoc) -> Boolean) = runBlocking {
        repeat(200) {
            if (store.docs[account]?.let(done) == true) return@runBlocking
            delay(10)
        }
    }

    private fun awaitSaved(store: MemDocs, wanted: Boolean) = awaitDoc(store) { it.hiddenFromSearch == wanted }

    @Test
    fun `only a 403 hidden-from-search is the moderators' hiding`() {
        val body = """{"error":"hidden-from-search"}"""
        assertTrue(isHiddenFromSearch(DiscoveryHttpError(403, "/discovery/visibility", body)))
        assertTrue(isHiddenFromSearch(RuntimeException("wrapped", DiscoveryHttpError(403, "/x", body))))
        assertFalse(isHiddenFromSearch(DiscoveryHttpError(403, "/discovery/visibility", """{"error":"forbidden"}""")))
        assertFalse(isHiddenFromSearch(DiscoveryHttpError(400, "/discovery/visibility", body)))
        assertFalse(isHiddenFromSearch(IllegalStateException("network")))
    }

    @Test
    fun `Everyone cannot be chosen while hidden`() {
        assertFalse(everyoneSelectable(hiddenFromSearch = true))
        assertTrue(everyoneSelectable(hiddenFromSearch = false))
    }

    @Test
    fun `the refusal is shown, is not a failure, and survives a new session`() {
        val store = MemDocs()
        val reach = manager(store)
        assertFalse(reach.hiddenFromSearch.value)
        reach.noteEveryoneStuck()
        assertEquals(ReachApplyStatus.FAILED, reach.applyStatus.value)

        reach.noteHiddenFromSearch()
        assertTrue(reach.hiddenFromSearch.value)
        assertFalse(reach.everyonePublicationStuck())
        assertEquals(ReachApplyStatus.APPLIED, reach.applyStatus.value)
        awaitSaved(store, true)

        val again = manager(store)
        assertTrue(again.hiddenFromSearch.value)
        // The setting is not touched: the published note stays as it was.
        assertEquals(null to null, again.publishedEveryone())
    }

    @Test
    fun `a later Everyone publication that lands ends the state`() {
        val store = MemDocs()
        val reach = manager(store)
        reach.noteHiddenFromSearch()
        awaitSaved(store, true)

        // Withdrawing says nothing about the hiding.
        reach.notePublishedEveryone(false, null)
        assertTrue(reach.hiddenFromSearch.value)
        awaitDoc(store) { it.publishedPublic == false }
        assertTrue(store.docs[account]!!.hiddenFromSearch)

        reach.notePublishedEveryone(true, "key")
        assertFalse(reach.hiddenFromSearch.value)
        awaitSaved(store, false)
        assertFalse(manager(store).hiddenFromSearch.value)
    }

    @Test
    fun `sign-out forgets it`() {
        val reach = manager(MemDocs())
        reach.noteHiddenFromSearch()
        reach.clear()
        assertFalse(reach.hiddenFromSearch.value)
    }
}
