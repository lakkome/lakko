// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.test.runTest
import me.lakko.app.ds.DsReachRefusedError
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * ADR-0050 section 2.5a item 7 — a sweep's Welcome into a closed mailbox is
 * never written unsigned and never silently lost: each held grant is tried in
 * order, a `401` moves to the next, and with none left the Welcome stays owed.
 */
class WelcomeGrantFallbackTest {

    private fun refused(): Throwable = DsReachRefusedError("/ds/mailbox/aa/messages", "")

    @Test
    fun `a 401 moves to the next grant`() = runTest {
        val tried = mutableListOf<String?>()
        val id = writeWelcomeWithGrants(
            candidates = listOf({ "stale-member-key" }, { "contact-key" }),
            write = { auth ->
                tried += auth
                if (auth == "stale-member-key") throw refused()
                7L
            },
            isRefusal = { it is DsReachRefusedError },
            requireGrant = true,
            accountKeyHex = "aa",
        )
        assertEquals(7L, id)
        assertEquals(listOf<String?>("stale-member-key", "contact-key"), tried)
    }

    @Test
    fun `no grant writes nothing and keeps the Welcome owed`() = runTest {
        val tried = mutableListOf<String?>()
        try {
            writeWelcomeWithGrants<String, Long>(
                candidates = emptyList(),
                write = { auth -> tried += auth; 1L },
                isRefusal = { it is DsReachRefusedError },
                requireGrant = true,
                accountKeyHex = "aa",
            )
            fail("expected WelcomeUndeliverable")
        } catch (e: WelcomeUndeliverable) {
            assertEquals("aa", e.accountKeyHex)
        }
        assertTrue("nothing was written, unsigned least of all", tried.isEmpty())
    }

    @Test
    fun `every grant refused writes nothing unsigned`() = runTest {
        val tried = mutableListOf<String?>()
        try {
            writeWelcomeWithGrants(
                candidates = listOf({ "a" }, { "b" }),
                write = { auth -> tried += auth; throw refused() },
                isRefusal = { it is DsReachRefusedError },
                requireGrant = true,
                accountKeyHex = "aa",
            )
            fail("expected WelcomeUndeliverable")
        } catch (_: WelcomeUndeliverable) {
            // owed
        }
        assertEquals(listOf<String?>("a", "b"), tried)
    }

    @Test
    fun `a grant that arrives later delivers the owed Welcome`() = runTest {
        val grants = mutableListOf<() -> String?>()
        val attempt: suspend () -> Long = {
            writeWelcomeWithGrants(
                candidates = grants.toList(),
                write = { auth -> if (auth == null) error("unsigned") else 3L },
                isRefusal = { it is DsReachRefusedError },
                requireGrant = true,
                accountKeyHex = "aa",
            )
        }
        try {
            attempt()
            fail("expected WelcomeUndeliverable")
        } catch (_: WelcomeUndeliverable) {
            // owed
        }
        grants += { "member-key" }
        assertEquals(3L, attempt())
    }

    @Test
    fun `outside a sweep a missing grant still writes unsigned and a refusal propagates`() = runTest {
        var unsigned = false
        writeWelcomeWithGrants<String, Long>(
            candidates = emptyList(),
            write = { auth -> unsigned = auth == null; 1L },
            isRefusal = { it is DsReachRefusedError },
            requireGrant = false,
            accountKeyHex = "aa",
        )
        assertTrue(unsigned)
        try {
            writeWelcomeWithGrants(
                candidates = listOf({ "a" }),
                write = { throw refused() },
                isRefusal = { it is DsReachRefusedError },
                requireGrant = false,
                accountKeyHex = "aa",
            )
            fail("expected the refusal")
        } catch (_: DsReachRefusedError) {
            // as before
        }
    }

    @Test
    fun `a failure that is not a 401 propagates at once`() = runTest {
        val tried = mutableListOf<String?>()
        try {
            writeWelcomeWithGrants(
                candidates = listOf({ "a" }, { "b" }),
                write = { auth -> tried += auth; throw IllegalStateException("offline") },
                isRefusal = { it is DsReachRefusedError },
                requireGrant = true,
                accountKeyHex = "aa",
            )
            fail("expected the failure")
        } catch (e: IllegalStateException) {
            assertTrue(e !is WelcomeUndeliverable)
        }
        assertEquals(listOf<String?>("a"), tried)
    }

    // ---- a person found in Everyone (ADR-0060 section 6, 2026-10-29) ------

    @Test
    fun `a held grant that is refused never falls back to an unsigned write and stays owed`() = runTest {
        var asked = false
        val tried = mutableListOf<String?>()
        try {
            writeWelcomeWithGrants(
                candidates = listOf({ "held-grant" }, { null }),
                write = { auth ->
                    tried += auth
                    throw refused()
                },
                isRefusal = { it is DsReachRefusedError },
                requireGrant = true,
                accountKeyHex = "aa",
                openFallback = { asked = true; "open-mailbox" },
            )
            fail("expected WelcomeUndeliverable")
        } catch (e: WelcomeUndeliverable) {
            assertEquals("aa", e.accountKeyHex)
        }
        assertEquals(listOf<String?>("held-grant"), tried)
        assertTrue("the open path was not consulted", !asked)
    }

    @Test
    fun `candidates that yield nothing count as none, and the open mailbox is written`() = runTest {
        val tried = mutableListOf<String?>()
        val id = writeWelcomeWithGrants(
            // The continuity no-op and a delegation lookup that finds none.
            candidates = listOf({ null }, { null }),
            write = { auth -> tried += auth; 9L },
            isRefusal = { it is DsReachRefusedError },
            requireGrant = true,
            accountKeyHex = "aa",
            openFallback = { "open-mailbox" },
        )
        assertEquals(9L, id)
        assertEquals(listOf<String?>("open-mailbox"), tried)
    }

    @Test
    fun `a grant that is accepted never reaches the open mailbox`() = runTest {
        var asked = false
        val tried = mutableListOf<String?>()
        writeWelcomeWithGrants(
            candidates = listOf({ "contact-key" }),
            write = { auth -> tried += auth; 1L },
            isRefusal = { it is DsReachRefusedError },
            requireGrant = true,
            accountKeyHex = "aa",
            openFallback = { asked = true; "open-mailbox" },
        )
        assertEquals(listOf<String?>("contact-key"), tried)
        assertTrue("the open path was not even consulted", !asked)
    }

    @Test
    fun `with no grant the open mailbox named for this person is written`() = runTest {
        val tried = mutableListOf<String?>()
        writeWelcomeWithGrants<String, Long>(
            candidates = emptyList(),
            write = { auth -> tried += auth; 2L },
            isRefusal = { it is DsReachRefusedError },
            requireGrant = true,
            accountKeyHex = "aa",
            openFallback = { "open-mailbox" },
        )
        assertEquals(listOf<String?>("open-mailbox"), tried)
    }

    @Test
    fun `a person with no record and no no-delegation envelope stays owed`() = runTest {
        val tried = mutableListOf<String?>()
        try {
            writeWelcomeWithGrants<String, Long>(
                candidates = emptyList(),
                write = { auth -> tried += auth; 1L },
                isRefusal = { it is DsReachRefusedError },
                requireGrant = true,
                accountKeyHex = "aa",
                // The fallback names nothing for this person.
                openFallback = { null },
            )
            fail("expected WelcomeUndeliverable")
        } catch (e: WelcomeUndeliverable) {
            assertEquals("aa", e.accountKeyHex)
        }
        assertTrue("nothing was written, unsigned least of all", tried.isEmpty())
    }
}
