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
        grants: List<String> = emptyList(),
        check: KeptGrantCheck? = null,
        accountIdHex: String? = "00",
    ) = LocalCandidate(
        keyB64 = key,
        keyHex = key,
        name = name,
        verifiedUsername = username,
        isContact = contact,
        accountIdHex = accountIdHex,
        keptGrantWiresB64 = grants,
        keptGrantCheck = check,
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
            listOf(candidate("g", "Granted", username = "granted", check = KeptGrantCheck.Live("R1JBTlQ="))),
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
    fun `before a verdict the row is the name alone, and a username does not find it`() {
        val waiting = candidate("w", "Waiting", username = "waiting", grants = listOf("G1", "G2"))
        val row = localPeople(listOf(waiting), "wai", normalize, null).single()
        assertEquals(PersonReach.NONE, row.reach)
        assertNull(row.username)
        assertNull(row.grantWireB64)
        assertFalse(canAddContact(row))
        val byUsername = candidate("w", "Someone", username = "waiting", grants = listOf("G1"))
        assertTrue(localPeople(listOf(byUsername), "waiting", normalize, null).isEmpty())
    }

    @Test
    fun `a live verdict offers the grant that passed, a verdict of dead offers nothing`() {
        val held = listOf(candidate("a", "Anna", username = "anna_w", grants = listOf("G1", "G2")))

        val live = localPeople(withKeptGrantChecks(held, mapOf("a" to KeptGrantCheck.Live("G2"))), "ann", normalize, null)
            .single()
        assertEquals(PersonReach.GRANT, live.reach)
        assertEquals("G2", live.grantWireB64)
        assertEquals("anna_w", live.username)
        assertTrue(live.grantChecked)
        assertTrue(canAddContact(live))

        val dead = withKeptGrantChecks(held, mapOf("a" to KeptGrantCheck.Dead))
        val row = localPeople(dead, "ann", normalize, null).single()
        assertEquals(PersonReach.NONE, row.reach)
        assertNull(row.username)
        assertNull(row.grantWireB64)
        assertFalse(canAddContact(row))
        // Matched by name only: the username is not this person's handle here.
        assertTrue(localPeople(dead, "anna_w", normalize, null).isEmpty())
    }

    @Test
    fun `a check that could not be made leaves the row as it was without one`() {
        val held = listOf(candidate("a", "Anna", username = "anna_w", grants = listOf("G1", "G2")))
        val row = localPeople(withKeptGrantChecks(held, mapOf("a" to KeptGrantCheck.Unreached("G2"))), "ann", normalize, null)
            .single()
        assertEquals(PersonReach.GRANT, row.reach)
        assertEquals("G2", row.grantWireB64)
        assertEquals("anna_w", row.username)
        assertFalse("nothing passed", row.grantChecked)
        assertTrue(canAddContact(row))
    }

    @Test
    fun `a contact is never checked and never waits`() {
        val contact = candidate("c", "Cara", contact = true, grants = listOf("G1"))
        assertEquals(PersonReach.CONTACT, localPeople(listOf(contact), "car", normalize, null).single().reach)
        assertTrue(keptGrantsToCheck(listOf(contact), "car", normalize, null).isEmpty())
    }

    @Test
    fun `who is checked for a text`() {
        val byName = candidate("n", "Anna", grants = listOf("G1"))
        val byUsername = candidate("u", "Someone", username = "annabel", grants = listOf("G2"))
        val noGrant = candidate("x", "Anna X")
        val otherText = candidate("o", "Bob", grants = listOf("G3"))
        val own = candidate("me", "Anna Me", grants = listOf("G4"))
        val unnamed = candidate("q", null, grants = listOf("G5"))
        val all = listOf(byName, byUsername, noGrant, otherText, own, unnamed)
        // From the first letter, by name or by username.
        assertEquals(listOf("n", "u"), keptGrantsToCheck(all, "a", normalize, "me").map { it.keyHex })
    }

    @Test
    fun `a verdict held for a candidate does not stop the next text asking`() {
        val dead = candidate("d", "Anna", grants = listOf("G1")).copy(keptGrantCheck = KeptGrantCheck.Dead)
        val live = candidate("l", "Annabel", grants = listOf("G2")).copy(keptGrantCheck = KeptGrantCheck.Live("G2"))
        assertEquals(listOf("d", "l"), keptGrantsToCheck(listOf(dead, live), "ann", normalize, null).map { it.keyHex })
    }

    @Test
    fun `merge prefers the grant that passed a check over one merely kept`() {
        val kept = PersonResult("k", "k", null, "Local", null, PersonReach.GRANT, "STALE")
        val checked = PersonResult("k", "k", "aa", "Network", "anna", PersonReach.GRANT, "LIVE", grantChecked = true)

        val merged = mergePeople(listOf(listOf(kept), listOf(checked)), ownKeyHex = null).single()
        assertEquals("LIVE", merged.grantWireB64)
        assertTrue(merged.grantChecked)
        assertEquals("Local", merged.name)
        // Whichever list it stands in.
        assertEquals(
            "LIVE",
            mergePeople(listOf(listOf(checked), listOf(kept)), ownKeyHex = null).single().grantWireB64,
        )
        // A name-only local row takes the network's checked grant.
        val waiting = PersonResult("k", "k", null, "Local", null, PersonReach.NONE, null)
        val overWaiting = mergePeople(listOf(listOf(waiting), listOf(checked)), ownKeyHex = null).single()
        assertEquals(PersonReach.GRANT, overWaiting.reach)
        assertEquals("LIVE", overWaiting.grantWireB64)
        assertEquals("anna", overWaiting.username)
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

    private fun row(key: String, reach: PersonReach) = PersonResult(key, key, null, "Kaj", null, reach, null)

    @Test
    fun `a contact listed above leaves People, and a person who is no contact stays`() {
        val listed = row("k", PersonReach.CONTACT)
        val chat = row("c", PersonReach.CONTACT)
        val noChat = row("n", PersonReach.CONTACT)
        val removed = row("r", PersonReach.GRANT)
        assertEquals(
            listOf(noChat, removed),
            peopleRows(listOf(listed, chat, noChat, removed), setOf("k"), setOf("c", "r")),
        )
    }

    @Test
    fun `a pending contact listed above leaves People whatever their reach`() {
        // Added through their link and not answered yet: the row above is
        // theirs, and People offers them no second row reading Request sent.
        val pending = row("p", PersonReach.OPEN)
        val granted = row("g", PersonReach.GRANT)
        val elsewhere = row("e", PersonReach.OPEN)
        assertEquals(listOf(elsewhere), peopleRows(listOf(pending, granted, elsewhere), setOf("p", "g"), emptySet()))
    }

    @Test
    fun `a channel reads its name when known, else its handle, never a prefix`() {
        assertEquals("Trip news", channelResultLabel(" Trip news ", "trip"))
        assertEquals("@trip", channelResultLabel(null, "trip"))
        assertEquals("@trip", channelResultLabel("  ", "trip"))
    }

    @Test
    fun `a held answer never offers a second request to a person who accepted`() {
        val held = PersonResult("k", "k", "aa", "Eivor", null, PersonReach.OPEN, null)
        val row = withCurrentContacts(listOf(held), setOf("k")).single()
        assertEquals(PersonReach.CONTACT, row.reach)
        assertFalse(canAddContact(row))
    }

    @Test
    fun `a person who is no contact keeps their row and their grant`() {
        val held = PersonResult("k", "k", "aa", "Eivor", null, PersonReach.GRANT, "G", grantChecked = true)
        assertEquals(listOf(held), withCurrentContacts(listOf(held), setOf("other")))
    }
}
