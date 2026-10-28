// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.search

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** People search, the pure half: the rules web `people.ts` and
 *  `search-state.ts` hold, run on the host JVM with the fallback normaliser. */
class PeopleModelTest {

    private val normalize: Normalize = ::fallbackNormalize

    private fun candidate(
        key: String,
        name: String?,
        username: String? = null,
        contact: Boolean = false,
        grant: String? = null,
        accountIdHex: String? = "00",
    ) = LocalCandidate(
        keyB64 = key,
        keyHex = key,
        name = name,
        verifiedUsername = username,
        isContact = contact,
        accountIdHex = accountIdHex,
        keptGrantWireB64 = grant,
    )

    @Test
    fun `local matching finds any part of any word, ignoring case and accents`() {
        assertTrue(matchesLoosely("Anna Weber", "ann", normalize))
        assertTrue(matchesLoosely("Anna Weber", "nna", normalize))
        assertTrue(matchesLoosely("Anna Weber", "weber an", normalize))
        assertTrue(matchesLoosely("Zoë", "zoe", normalize))
        assertFalse(matchesLoosely("Anna Weber", "anna x", normalize))
    }

    @Test
    fun `a person with no name is never listed, and the own account never appears`() {
        val found = localPeople(
            listOf(candidate("a", null, contact = true), candidate("me", "Anna"), candidate("b", "Anna B", contact = true)),
            "anna",
            normalize,
            ownKeyHex = "me",
        )
        assertEquals(listOf("b"), found.map { it.keyHex })
    }

    @Test
    fun `a username is shown only with the permission to use it`() {
        val hidden = localPeople(listOf(candidate("h", "Hidden", username = "hidden")), "hid", normalize, null).single()
        assertEquals(PersonReach.NONE, hidden.reach)
        assertNull(hidden.username)
        val granted = localPeople(
            listOf(candidate("g", "Granted", username = "granted", grant = "R1JBTlQ=")),
            "gra",
            normalize,
            null,
        ).single()
        assertEquals(PersonReach.GRANT, granted.reach)
        assertEquals("granted", granted.username)
        assertEquals("R1JBTlQ=", granted.grantWireB64)
        assertFalse(canAddContact(hidden))
        assertTrue(canAddContact(granted))
    }

    @Test
    fun `a hidden person is not found by a username this device verified`() {
        val found = localPeople(listOf(candidate("h", "Someone", username = "secret")), "secret", normalize, null)
        assertTrue(found.isEmpty())
    }

    @Test
    fun `merge keeps the local name, the strongest reach, and gates the username again`() {
        val local = PersonResult("k", "k", null, "Local name", null, PersonReach.NONE, null)
        val network = PersonResult("k", "k", "aa", "Network name", "anna", PersonReach.OPEN, null)
        val merged = mergePeople(listOf(listOf(local), listOf(network)), ownKeyHex = null).single()
        assertEquals("Local name", merged.name)
        assertEquals(PersonReach.OPEN, merged.reach)
        assertEquals("anna", merged.username)
        assertEquals("aa", merged.accountIdHex)
        assertNull(merged.grantWireB64)
    }

    @Test
    fun `not found is an answer only when every question was answered`() {
        assertEquals(PeopleSearchState.FOUND, foldSearch(1, listOf(LegOutcome.FAILED)))
        assertEquals(PeopleSearchState.NOT_FOUND, foldSearch(0, listOf(LegOutcome.ANSWERED, LegOutcome.ANSWERED)))
        assertEquals(PeopleSearchState.FAILED, foldSearch(0, listOf(LegOutcome.ANSWERED, LegOutcome.FAILED)))
        assertEquals(
            PeopleSearchState.ALLOWANCE_USED,
            foldSearch(0, listOf(LegOutcome.FAILED, LegOutcome.ALLOWANCE_USED)),
        )
        assertEquals(SEARCH_FAILED, searchStateMessage(PeopleSearchState.FAILED))
        assertEquals(SEARCH_NOT_FOUND, searchStateMessage(PeopleSearchState.NOT_FOUND))
        assertNull(searchStateMessage(PeopleSearchState.SEARCHING))
        // Out of tokens is its own line: never "not found", never the network failure.
        assertEquals(SEARCH_OUT_OF_SEARCHES, searchStateMessage(PeopleSearchState.ALLOWANCE_USED))
        assertEquals("search.people.out_of_searches", SEARCH_OUT_OF_SEARCHES.key)
    }
}
