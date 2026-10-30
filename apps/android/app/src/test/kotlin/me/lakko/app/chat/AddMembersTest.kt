// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.runBlocking
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.en
import me.lakko.app.search.PersonReach
import me.lakko.app.search.PersonResult
import me.lakko.app.search.fallbackNormalize
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/conversation-info.md` sections 2.4, 3 and 3.1 — the pure half of
 * the group info screen and its Add members screen: which screen is on top
 * and where back returns, who is listed and who can be picked, the Add
 * button's label, and the add of several picks one after another.
 */
class AddMembersTest {

    private val normalize: (String) -> String? = ::fallbackNormalize

    private fun contact(key: String, name: String, accountId: String? = "acct-$key", username: String? = null) =
        AddCandidateContact(keyB64 = key, accountIdB64 = accountId, name = name, username = username)

    private fun person(
        key: String,
        name: String,
        accountIdHex: String? = "00112233445566778899aabbccddeeff",
        reach: PersonReach = PersonReach.OPEN,
        grant: String? = null,
        binding: String? = null,
    ) = PersonResult(
        keyB64 = key,
        keyHex = key,
        accountIdHex = accountIdHex,
        name = name,
        username = null,
        reach = reach,
        grantWireB64 = grant,
        mailboxBindingB64 = binding,
    )

    // ---- which screen, and where back goes ------------------------------

    @Test
    fun `the info and what is pushed over it show only for an open group`() {
        assertEquals(GroupInfoLayer.CONVERSATION, groupInfoLayer(false, true, true, true))
        assertEquals(GroupInfoLayer.CONVERSATION, groupInfoLayer(true, false, false, false))
        assertEquals(GroupInfoLayer.INFO, groupInfoLayer(true, true, false, false))
        assertEquals(GroupInfoLayer.ADD_MEMBERS, groupInfoLayer(true, true, true, false))
        assertEquals(GroupInfoLayer.TRANSFER, groupInfoLayer(true, true, false, true))
    }

    @Test
    fun `back returns to the screen beneath`() {
        assertNull(backDestination(GroupInfoLayer.CONVERSATION))
        assertEquals(GroupInfoLayer.CONVERSATION, backDestination(GroupInfoLayer.INFO))
        assertEquals(GroupInfoLayer.INFO, backDestination(GroupInfoLayer.ADD_MEMBERS))
        assertEquals(GroupInfoLayer.INFO, backDestination(GroupInfoLayer.TRANSFER))
    }

    @Test
    fun `a member row opens the contact screen for a contact and never for this account`() {
        val contacts = setOf("k1")
        assertTrue(memberOpensContact("k1", ownRow = false, contactKeysB64 = contacts))
        assertFalse(memberOpensContact("k1", ownRow = true, contactKeysB64 = contacts))
        assertFalse(memberOpensContact("k2", ownRow = false, contactKeysB64 = contacts))
    }

    // ---- who is listed ---------------------------------------------------

    @Test
    fun `members and people already asked for are not listed`() {
        val rows = addContactRows(
            contacts = listOf(contact("a", "Anna"), contact("b", "Boris"), contact("c", "Clara")),
            memberKeysB64 = setOf("a"),
            waitingKeysB64 = setOf("b"),
            ownKeyB64 = null,
            query = "",
            normalize = normalize,
        )
        assertEquals(listOf("c"), rows.map { it.contact.keyB64 })
    }

    @Test
    fun `a contact without an account id is listed and cannot be picked`() {
        val rows = addContactRows(
            contacts = listOf(contact("a", "Anna", accountId = null), contact("b", "Boris")),
            memberKeysB64 = emptySet(),
            waitingKeysB64 = emptySet(),
            ownKeyB64 = null,
            query = "",
            normalize = normalize,
        )
        assertEquals(listOf(false, true), rows.map { it.pickable })
        assertNull(pickOf(rows[0].contact))
    }

    @Test
    fun `typing filters contacts loosely from the first letter, by name or username`() {
        val contacts = listOf(
            contact("a", "Anna Weber"),
            contact("b", "Boris", username = "bwann"),
            contact("c", "Clara"),
        )
        fun keys(q: String) = addContactRows(contacts, emptySet(), emptySet(), null, q, normalize).map { it.contact.keyB64 }
        assertEquals(listOf("a", "b", "c"), keys(""))
        assertEquals(listOf("a", "b", "c"), keys("a"))
        assertEquals(listOf("a"), keys("weber an"))
        assertEquals(listOf("a", "b"), keys("@ann"))
        assertEquals(emptyList<String>(), keys("zz"))
    }

    @Test
    fun `people already members, asked for, or listed above are not shown twice`() {
        val rows = addPersonRows(
            results = listOf(person("m", "Member"), person("w", "Waiting"), person("l", "Listed"), person("n", "New")),
            memberKeysB64 = setOf("m"),
            waitingKeysB64 = setOf("w"),
            listedKeysB64 = setOf("l"),
            ownKeyB64 = null,
        )
        assertEquals(listOf("n"), rows.map { it.person.keyB64 })
    }

    @Test
    fun `a found person without an account id or a way to reach them cannot be picked`() {
        val rows = addPersonRows(
            results = listOf(
                person("a", "A", accountIdHex = null),
                person("b", "B", reach = PersonReach.NONE),
                person("c", "C", reach = PersonReach.GRANT, grant = "g"),
            ),
            memberKeysB64 = emptySet(),
            waitingKeysB64 = emptySet(),
            listedKeysB64 = emptySet(),
            ownKeyB64 = null,
        )
        assertEquals(listOf(false, false, true), rows.map { it.pickable })
        assertNull(pickOf(rows[0].person))
        assertNull(pickOf(rows[1].person))
        val pick = pickOf(rows[2].person)!!
        // The account id travels as base64 of its 16 bytes, and the grant
        // the search found them through comes with the pick.
        assertEquals(16, java.util.Base64.getDecoder().decode(pick.accountIdB64).size)
        assertEquals("g", pick.grantWireB64)
    }

    @Test
    fun `a pick carries what the search result carries for reaching the person`() {
        val open = pickOf(person("e", "Eve", reach = PersonReach.OPEN, binding = "bind-e"))!!
        assertEquals("e", open.keyB64)
        assertEquals(16, java.util.Base64.getDecoder().decode(open.accountIdB64).size)
        assertNull(open.grantWireB64)
        assertEquals("bind-e", open.mailboxBindingB64)

        val granted = pickOf(person("g", "Gus", reach = PersonReach.GRANT, grant = "grant-g", binding = "bind-g"))!!
        assertEquals("grant-g", granted.grantWireB64)
        assertEquals("bind-g", granted.mailboxBindingB64)

        // A grant on a result that is not reached through it is not carried.
        val openWithGrant = pickOf(person("h", "Hal", reach = PersonReach.OPEN, grant = "stray", binding = "bind-h"))!!
        assertNull(openWithGrant.grantWireB64)
    }

    @Test
    fun `a found grant is kept first, a found binding only without a grant, a contact files nothing`() {
        assertEquals(
            FoundReach.Grant("grant-g"),
            foundReachOf(AddMemberPick("g", "x", "Gus", grantWireB64 = "grant-g", mailboxBindingB64 = "bind-g")),
        )
        assertEquals(
            FoundReach.OpenBinding("bind-e"),
            foundReachOf(AddMemberPick("e", "x", "Eve", mailboxBindingB64 = "bind-e")),
        )
        assertNull(foundReachOf(pickOf(contact("c", "Cleo"))!!))
    }

    @Test
    fun `picking twice un-picks, by person`() {
        val a = AddMemberPick("a", "x", "Anna")
        val b = AddMemberPick("b", "y", "Boris")
        val picked = togglePick(togglePick(emptyList(), a), b)
        assertEquals(listOf("a", "b"), picked.map { it.keyB64 })
        assertEquals(listOf("b"), togglePick(picked, a.copy(name = "renamed")).map { it.keyB64 })
    }

    @Test
    fun `the Add button counts from two picks on`() {
        EnglishForTests.install()
        assertEquals("Add", addSubmitLabel(0).en)
        assertEquals("Add", addSubmitLabel(1).en)
        assertEquals("Add 3", addSubmitLabel(3).en)
    }

    // ---- adding the picks -------------------------------------------------

    @Test
    fun `every pick is tried in turn and the failures are named once`() = runBlocking {
        val strings = EnglishForTests.install()
        val tried = mutableListOf<String>()
        val picks = listOf(AddMemberPick("a", "x", "Anna"), AddMemberPick("b", "y", "Boris"), AddMemberPick("c", "z", "Clara"))
        val failed = addPicksInTurn(picks) { pick ->
            tried += pick.keyB64
            if (pick.keyB64 != "b") error("refused")
        }
        assertEquals(listOf("a", "b", "c"), tried)
        assertEquals(listOf("a", "c"), failed.map { it.keyB64 })
        assertEquals("Could not add Anna, Clara. Try again.", someFailedText(failed, strings)!!.en)
        assertNull(someFailedText(emptyList(), strings))
    }

    @Test
    fun `a cancellation stops the run and is passed on`() {
        val tried = mutableListOf<String>()
        val picks = listOf(AddMemberPick("a", "x", "Anna"), AddMemberPick("b", "y", "Boris"))
        var thrown: Throwable? = null
        try {
            runBlocking {
                addPicksInTurn(picks) { pick ->
                    tried += pick.keyB64
                    throw CancellationException("left")
                }
            }
        } catch (e: CancellationException) {
            thrown = e
        }
        assertTrue(thrown is CancellationException)
        assertEquals(listOf("a"), tried)
    }
}
