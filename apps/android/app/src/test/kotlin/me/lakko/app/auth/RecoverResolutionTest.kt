// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * Locks the cross-device recover-resolution behaviour (G-29) — the
 * Android mirror of `apps/web/src/auth/recover-resolution.test.ts` +
 * the iOS twin. Six cases pin the cache-hit / discovery-lookup /
 * fresh-device branch logic + the two collapsed error messages.
 */
class RecoverResolutionTest {

    /** Deterministic fake encoder so the test can assert the looked-up
     *  bytes were passed through it. */
    private val fakeEncode: (ByteArray) -> String = { bytes ->
        "enc:" + bytes.joinToString(",")
    }

    /** A fake lookup lambda recording whether it was invoked, returning
     *  a fixed [RecoverAccountLookup]. */
    private class FakeLookup(private val result: RecoverAccountLookup) {
        var wasCalled: Boolean = false
            private set
        var lastUsername: String? = null
            private set

        val fn: suspend (String) -> RecoverAccountLookup = { name ->
            wasCalled = true
            lastUsername = name
            result
        }
    }

    @Test
    fun `uses the cached account when the typed username is empty (no lookup)`() = runTest {
        val lookup = FakeLookup(RecoverAccountLookup(account = byteArrayOf(1, 2), "alice"))
        val resolved = resolveRecoverAccount(
            cached = CachedAccount(accountIdB64 = "CACHED", username = "alice"),
            username = null,
            lookup = lookup.fn,
            encodeAccountId = fakeEncode,
        )
        assertEquals("CACHED", resolved.accountIdB64)
        assertEquals("alice", resolved.resolvedUsername)
        assertFalse(lookup.wasCalled)
    }

    @Test
    fun `uses the cached account when the typed username matches it (no lookup)`() = runTest {
        val lookup = FakeLookup(RecoverAccountLookup(account = byteArrayOf(1, 2), "alice"))
        val resolved = resolveRecoverAccount(
            cached = CachedAccount(accountIdB64 = "CACHED", username = "alice"),
            username = "  alice  ", // trimmed before compare
            lookup = lookup.fn,
            encodeAccountId = fakeEncode,
        )
        assertEquals("CACHED", resolved.accountIdB64)
        assertEquals("alice", resolved.resolvedUsername)
        assertFalse(lookup.wasCalled)
    }

    @Test
    fun `looks up via discovery on a FRESH device (no cache) given the username`() = runTest {
        val lookup = FakeLookup(
            RecoverAccountLookup(account = byteArrayOf(1, 2, 3, 4), normalisedUsername = "alice"),
        )
        val resolved = resolveRecoverAccount(
            cached = null,
            username = "alice",
            lookup = lookup.fn,
            encodeAccountId = fakeEncode,
        )
        assertTrue(lookup.wasCalled)
        assertEquals("alice", lookup.lastUsername)
        assertEquals("enc:1,2,3,4", resolved.accountIdB64)
        assertEquals("alice", resolved.resolvedUsername)
    }

    @Test
    fun `looks up (not the cache) when the typed username differs from the cached one`() = runTest {
        val lookup = FakeLookup(
            RecoverAccountLookup(account = byteArrayOf(9, 9), normalisedUsername = "bob"),
        )
        val resolved = resolveRecoverAccount(
            cached = CachedAccount(accountIdB64 = "CACHED", username = "alice"),
            username = "bob",
            lookup = lookup.fn,
            encodeAccountId = fakeEncode,
        )
        assertTrue(lookup.wasCalled)
        assertEquals("bob", lookup.lastUsername)
        assertEquals("enc:9,9", resolved.accountIdB64)
        assertEquals("bob", resolved.resolvedUsername)
    }

    @Test
    fun `throws the bad-phrase-collapsed error when the username is unregistered`() = runTest {
        val lookup = FakeLookup(
            RecoverAccountLookup(account = null, normalisedUsername = "ghost"),
        )
        val error = assertFailsWith {
            resolveRecoverAccount(
                cached = null,
                username = "ghost",
                lookup = lookup.fn,
                encodeAccountId = fakeEncode,
            )
        }
        assertTrue(
            "expected message to contain \"invalid recovery\", was: ${error.message}",
            error.message?.contains("invalid recovery") == true,
        )
    }

    @Test
    fun `throws the fresh-device prompt when neither a cache nor a username is given`() = runTest {
        val lookup = FakeLookup(RecoverAccountLookup(account = byteArrayOf(1), "x"))
        val error = assertFailsWith {
            resolveRecoverAccount(
                cached = null,
                username = "   ", // whitespace-only -> empty after trim
                lookup = lookup.fn,
                encodeAccountId = fakeEncode,
            )
        }
        assertTrue(
            "expected message to contain \"no account on this device\", was: ${error.message}",
            error.message?.contains("no account on this device") == true,
        )
        assertFalse(lookup.wasCalled)
    }

    /**
     * Runs [block] and asserts it throws an [AuthClientArgError],
     * returning the caught instance for message assertions. JUnit's
     * `assertThrows` takes a non-suspend `ThrowingRunnable`, so we
     * capture the throw manually to keep the `suspend` call inside the
     * coroutine body.
     */
    private suspend fun assertFailsWith(
        block: suspend () -> Unit,
    ): AuthClientArgError {
        try {
            block()
        } catch (e: AuthClientArgError) {
            return e
        }
        fail("expected AuthClientArgError, but no exception was thrown")
        error("unreachable")
    }
}
