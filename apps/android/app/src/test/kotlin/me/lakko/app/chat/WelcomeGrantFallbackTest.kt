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
}
