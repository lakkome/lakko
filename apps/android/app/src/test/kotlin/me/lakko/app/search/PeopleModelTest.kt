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
    fun `a channel the card already shows gets no second handle row`() {
        // Base scenario 22 (2026-11-12): "@walk_22_channel" listed the card
        // and, below it, the same channel again from the handle lookup.
        val id = "0123456789abcdef0123456789abcdef"
        assertFalse(handleChannelRowShown(id, listOf(id.uppercase()), foundPending = false))
        assertTrue(handleChannelRowShown(id, listOf("ffffffffffffffffffffffffffffffff"), foundPending = false))
        assertTrue(handleChannelRowShown(id, emptyList(), foundPending = false))
        // Not while the cards are still asked: no row that turns into a copy.
        assertFalse(handleChannelRowShown(id, emptyList(), foundPending = true))
        // Nor when the channel already stands as a row of the list.
        assertFalse(handleChannelRowShown(id, emptyList(), foundPending = false, listedIdsHex = setOf(id)))
    }

    @Test
    fun `a followed channel found on the network is not drawn under its own row`() {
        // Live on the Pixel 7a (2026-11-12): "@walk_22_channel" showed the
        // followed channel's list row and, below it, its network card.
        val id = "0123456789abcdef0123456789abcdef"
        assertFalse(foundConversationShown(id, setOf(id)))
        assertFalse(foundConversationShown(id.uppercase(), setOf(id)))
        assertTrue(foundConversationShown(id, setOf("ffffffffffffffffffffffffffffffff")))
        assertTrue(foundConversationShown(id, emptySet()))
    }

    @Test
    fun `a channel reads its name when known, else its handle, never a prefix`() {
        assertEquals("Trip news", channelResultLabel(" Trip news ", "trip"))
        assertEquals("@trip", channelResultLabel(null, "trip"))
        assertEquals("@trip", channelResultLabel("  ", "trip"))
    }

    // -------------------------------------------------------------------------
    // Base scenario 9 - one person, one row. The same table as web
    // `people.test.ts` and iOS `PeopleSearchRowsTests`.
    // -------------------------------------------------------------------------

    /** Own keys: OWN_OLD was proven superseded by OWN_NEW. Contact Cara is
     *  stored under C_OLD and rotated (proven) to C_NEW. */
    private val links = mapOf("OWN_OLD" to "OWN_NEW", "C_OLD" to "C_NEW")
    private val identity = SearchIdentity(
        ownKeysB64 = setOf("OWN_NEW", "OWN_OLD"),
        ownAccountIdHex = "aa",
        personOf = { links[it] ?: it },
    )
    private val cara = SearchContact("C_OLD", "C_OLD", "cc")
    private val dan = SearchContact("D", "D", "dd")

    private fun hit(key: String, reach: PersonReach, account: String? = null, name: String = "Kaj", grant: String? = null) =
        PersonResult(key, key, account, name, null, reach, grant)

    private fun rows(
        vararg lists: List<PersonResult>,
        listed: Set<String> = emptySet(),
        chatPeers: Set<String> = emptySet(),
    ) = searchPeopleRows(lists.toList(), identity, listOf(cara, dan), listed, chatPeers)

    @Test
    fun `the own account never appears, under any key or by its account id`() {
        assertEquals(
            emptyList<PersonResult>(),
            rows(
                listOf(hit("OWN_OLD", PersonReach.NONE)),
                listOf(hit("OWN_NEW", PersonReach.OPEN), hit("X", PersonReach.OPEN, account = "AA")),
            ),
        )
    }

    @Test
    fun `a contact found under their new key is their one contact row`() {
        val found = rows(
            listOf(hit("C_OLD", PersonReach.CONTACT, name = "Cara")),
            listOf(hit("C_NEW", PersonReach.OPEN, account = "cc", name = "Cara N", grant = "G")),
        ).single()
        assertEquals("C_OLD", found.keyB64)
        assertEquals(PersonReach.CONTACT, found.reach)
        assertEquals("Cara", found.name)
        assertNull(found.grantWireB64)
        assertFalse(canAddContact(found))
    }

    @Test
    fun `a contact the network alone found, by key or by account id, is no Add contact`() {
        val byKey = rows(listOf(hit("C_NEW", PersonReach.OPEN, account = "cc"))).single()
        assertEquals("C_OLD", byKey.keyB64)
        assertEquals(PersonReach.CONTACT, byKey.reach)
        val byAccount = rows(listOf(hit("Y", PersonReach.GRANT, account = "DD", grant = "G"))).single()
        assertEquals("D", byAccount.keyB64)
        assertEquals(PersonReach.CONTACT, byAccount.reach)
        assertFalse(canAddContact(byAccount))
    }

    @Test
    fun `a person whose one-to-one stands above has no second row, contact or not`() {
        assertEquals(
            emptyList<PersonResult>(),
            rows(
                listOf(hit("C_NEW", PersonReach.OPEN, account = "cc"), hit("P", PersonReach.OPEN, account = "pp")),
                chatPeers = setOf("C_OLD", "P"),
            ),
        )
    }

    @Test
    fun `a person listed above leaves, and a stranger keeps Add contact`() {
        val stranger = hit("S", PersonReach.OPEN, account = "ss")
        val found = rows(listOf(hit("Q", PersonReach.OPEN, account = "qq"), stranger), listed = setOf("Q"))
        assertEquals(listOf(stranger), found)
        assertTrue(canAddContact(found.single()))
    }

    @Test
    fun `a contact the list cannot match yet keeps one row`() {
        val found = rows(listOf(hit("D", PersonReach.CONTACT)))
        assertEquals(listOf("D"), found.map { it.keyB64 })
    }

    @Test
    fun `a key nothing proved related is another person`() {
        val other = hit("C_OTHER", PersonReach.OPEN, account = "ee")
        assertEquals(listOf(other), rows(listOf(other)))
    }
}
