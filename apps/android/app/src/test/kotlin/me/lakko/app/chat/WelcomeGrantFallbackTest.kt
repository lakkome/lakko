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

    /** The core's `reachOpenMailboxWriteAllowed`, restated for the host JVM
     *  (no native library here) and recording what it was asked: the rule
     *  itself is tested in `lakko-contacts::reach_plan`. */
    private class RecordingRule : me.lakko.app.reach.OpenWriteRule {
        val asked = mutableListOf<Triple<Boolean, Int, Int>>()
        override fun allowed(recordOpen: Boolean, candidates: Int, refused: Int): Boolean {
            asked += Triple(recordOpen, candidates, refused)
            return recordOpen && refused >= candidates
        }
    }

    @Test
    fun `every held grant refused falls back to the open mailbox when the core allows it`() = runTest {
        // G-53 — a stale grant no longer blocks the open mailbox.
        val rule = RecordingRule()
        val tried = mutableListOf<String?>()
        val id = writeWelcomeWithGrants(
            candidates = listOf({ "stale-group-key" }, { null }, { "stale-member-key" }),
            write = { auth ->
                tried += auth
                if (auth != "open-mailbox") throw refused()
                5L
            },
            isRefusal = { it is DsReachRefusedError },
            requireGrant = true,
            accountKeyHex = "aa",
            openFallback = { "open-mailbox" },
            openRule = rule,
        )
        assertEquals(5L, id)
        assertEquals(listOf<String?>("stale-group-key", "stale-member-key", "open-mailbox"), tried)
        // Two candidates yielded an authorisation, both refused with a 401.
        assertEquals(listOf(Triple(true, 2, 2)), rule.asked)
    }

    @Test
    fun `a refusal the core does not accept keeps the Welcome owed`() = runTest {
        val tried = mutableListOf<String?>()
        try {
            writeWelcomeWithGrants(
                candidates = listOf({ "held-grant" }),
                write = { auth -> tried += auth; throw refused() },
                isRefusal = { it is DsReachRefusedError },
                requireGrant = true,
                accountKeyHex = "aa",
                openFallback = { "open-mailbox" },
                openRule = { _, _, _ -> false },
            )
            fail("expected WelcomeUndeliverable")
        } catch (e: WelcomeUndeliverable) {
            assertEquals("aa", e.accountKeyHex)
        }
        assertEquals(listOf<String?>("held-grant"), tried)
    }

    @Test
    fun `a refused grant with no open record stays owed and writes nothing unsigned`() = runTest {
        val rule = RecordingRule()
        val tried = mutableListOf<String?>()
        try {
            writeWelcomeWithGrants<String, Long>(
                candidates = listOf({ "held-grant" }),
                write = { auth -> tried += auth; throw refused() },
                isRefusal = { it is DsReachRefusedError },
                requireGrant = true,
                accountKeyHex = "aa",
                openFallback = { null },
                openRule = rule,
            )
            fail("expected WelcomeUndeliverable")
        } catch (_: WelcomeUndeliverable) {
            // owed
        }
        assertEquals(listOf<String?>("held-grant"), tried)
        assertTrue("no open record: the rule is not even asked", rule.asked.isEmpty())
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
            openRule = RecordingRule(),
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
            openRule = RecordingRule(),
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
