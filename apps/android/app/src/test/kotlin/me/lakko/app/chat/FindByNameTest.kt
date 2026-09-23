// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * docs/ux/navigation.md section 4 — the ONE network lookup.
 *
 * The two rules it is gated by (`bare_name`, `looks_like_a_name`) live in
 * `crates/lakko-ui-rules` and are tested there; they arrive here as parameters,
 * so what is tested on this side is the decision built on top of them: ask both
 * namespaces, and never report "no such person" when what actually happened was
 * "no answer".
 */
class FindByNameTest {

    /** The two namespaces, scripted. A `null` script entry means "not here"; a
     *  `Throwable` means the network was the problem. */
    private class Lookups(
        val channelResult: Any? = null,
        val personResult: Any? = null,
    ) : NameLookups {
        var channelAsked: String? = null
        var personAsked: String? = null

        override suspend fun channel(handle: String): String? {
            channelAsked = handle
            if (channelResult is Throwable) throw channelResult
            return channelResult as String?
        }

        override suspend fun person(username: String): String? {
            personAsked = username
            if (personResult is Throwable) throw personResult
            return personResult as String?
        }
    }

    private val bare: (String) -> String = { it.trim().removePrefix("@").trim() }
    private val canonical: (String) -> Boolean = { it.length in 3..32 }

    private suspend fun find(typed: String, lookups: NameLookups) =
        findByName(typed, lookups, bare, canonical)

    @Test
    fun `a channel handle opens the channel`() = runTest {
        val lookups = Lookups(channelResult = "deadbeef")
        val found = find("@news", lookups)
        assertEquals(FoundByName.Channel("deadbeef", "news"), found)
        // The channel answered, so the person namespace was never asked.
        assertNull(lookups.personAsked)
    }

    @Test
    fun `a username is handed on as the normalised form the server returned`() = runTest {
        val found = find("@Anna", Lookups(personResult = "anna"))
        assertEquals(FoundByName.Person("anna"), found)
    }

    @Test
    fun `both namespaces are asked, because the person does not know which holds the name`() =
        runTest {
            val lookups = Lookups()
            assertEquals(FoundByName.NothingFound, find("@anna", lookups))
            assertEquals("anna", lookups.channelAsked)
            assertEquals("anna", lookups.personAsked)
        }

    @Test
    fun `a name the blinder would refuse never costs a lookup`() = runTest {
        val lookups = Lookups(personResult = "ab")
        assertEquals(FoundByName.NothingFound, find("ab", lookups))
        assertNull(lookups.channelAsked)
        assertNull(lookups.personAsked)
    }

    @Test
    fun `an unreachable network is not the same fact as an absent name`() = runTest {
        // The worst thing this can do is tell someone their friend does not
        // exist because the connection dropped.
        val down = java.io.IOException("offline")
        val found = find("@anna", Lookups(channelResult = down, personResult = down))
        assertEquals(FoundByName.Unreachable, found)
    }

    @Test
    fun `one namespace failing does not stop the other answering`() = runTest {
        val found = find(
            "@anna",
            Lookups(channelResult = java.io.IOException("offline"), personResult = "anna"),
        )
        assertEquals(FoundByName.Person("anna"), found)
    }

    @Test
    fun `one namespace failing after the other answered nothing is still nothing`() = runTest {
        // The person namespace was reached and said no; only the channel call
        // failed, so there IS an answer about this name.
        val found = find(
            "@anna",
            Lookups(channelResult = java.io.IOException("offline"), personResult = null),
        )
        assertEquals(FoundByName.NothingFound, found)
    }

    @Test
    fun `a cancellation is the caller going away, never a lookup result`() = runTest {
        val lookups = Lookups(channelResult = CancellationException("gone"))
        try {
            find("@anna", lookups)
            fail("the cancellation should have propagated")
        } catch (_: CancellationException) {
            // Expected. Turning it into "nothing found" would report a
            // conclusion nobody reached.
        }
    }

    @Test
    fun `the copy names what happened and what to do, and never a protocol word`() {
        val nothing = findByNameMessage(FoundByName.NothingFound, "@anna", bare)
        assertEquals("Nothing found for @anna.", nothing)
        val unreachable = findByNameMessage(FoundByName.Unreachable, "@anna", bare)
        assertEquals("Could not look that up. Check your connection and try again.", unreachable)
        for (copy in listOfNotNull(nothing, unreachable)) {
            assertFalse(copy.contains("OPRF"))
            assertFalse(copy.contains("blind"))
            assertFalse(copy.contains("discovery"))
            assertFalse(copy.contains("!"))
        }
    }

    @Test
    fun `a result that navigates has nothing to say`() {
        assertNull(findByNameMessage(FoundByName.Person("anna"), "@anna", bare))
        assertNull(findByNameMessage(FoundByName.Channel("id", "news"), "@news", bare))
    }

    @Test
    fun `the row offers the whole name, never a prefix`() {
        assertEquals("Look up @anna", findByNameRowLabel("  @Anna  ") { it.trim().removePrefix("@").lowercase() })
        assertTrue(findByNameRowLabel("anna", bare).startsWith("Look up @"))
    }
}
