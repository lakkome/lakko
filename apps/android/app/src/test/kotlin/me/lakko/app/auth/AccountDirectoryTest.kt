// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.test.runTest
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.discovery.DiscoveryNetworkError
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * Pins the discovery half of signup and login on Android — the rules that
 * were missing entirely: signup never claimed the username, and login refused
 * on any device that did not already hold the account.
 *
 * `AuthRepository` itself cannot be constructed on the JVM (Keystore-backed
 * stores, the native core), so these drive the two helpers it now calls.
 */
class AccountDirectoryTest {

    private val accountId = ByteArray(16) { (it + 1).toByte() }

    private val fakeEncode: (ByteArray) -> String = { bytes -> "enc:" + bytes.joinToString(",") }

    /** Records every call; answers from the configured behaviour. */
    private class FakeDirectory(
        private val onRegister: (String) -> String = { it.lowercase() },
        private val lookupResult: RecoverAccountLookup? = null,
    ) : AccountDirectory {
        val registrations = mutableListOf<Triple<String, ByteArray, String>>()
        val lookups = mutableListOf<String>()

        override suspend fun registerUsername(
            sessionToken: String,
            accountId: ByteArray,
            username: String,
        ): String {
            registrations += Triple(sessionToken, accountId, username)
            return onRegister(username)
        }

        override suspend fun lookup(username: String): RecoverAccountLookup {
            lookups += username
            return lookupResult ?: error("lookup not expected")
        }
    }

    // --- signup ---------------------------------------------------------

    @Test
    fun `signup registers the username for the new account under its session token`() = runTest {
        val directory = FakeDirectory(onRegister = { "alice" })

        val normalised = claimUsernameAtSignup(
            directory = directory,
            sessionToken = "session-token-1",
            accountId = accountId,
            username = "  Alice ",
        )

        assertEquals(1, directory.registrations.size)
        val (token, boundAccount, sentName) = directory.registrations.single()
        assertEquals("session-token-1", token)
        assertArrayEquals(accountId, boundAccount)
        // Trimmed only: the canonical form is the native blinder's job, and it
        // comes back as the return value rather than being recomputed here.
        assertEquals("Alice", sentName)
        assertEquals("alice", normalised)
    }

    @Test
    fun `signup surfaces a taken username as a form error`() = runTest {
        val directory = FakeDirectory(onRegister = {
            throw DiscoveryHttpError(409, "/discovery/register", "username already registered")
        })

        try {
            claimUsernameAtSignup(directory, "token", accountId, "alice")
            fail("a taken username must fail signup")
        } catch (err: AuthClientArgError) {
            assertEquals("username @alice is already taken — pick another", err.message)
        }
    }

    @Test
    fun `signup propagates every other registration failure unchanged`() = runTest {
        val forbidden = DiscoveryHttpError(403, "/discovery/register", "account_id mismatch")
        val network = DiscoveryNetworkError("/discovery/register", RuntimeException("offline"))
        for (failure in listOf(forbidden, network)) {
            val directory = FakeDirectory(onRegister = { throw failure })
            try {
                claimUsernameAtSignup(directory, "token", accountId, "alice")
                fail("a registration failure must fail signup")
            } catch (err: Throwable) {
                assertSame(failure, err)
            }
        }
    }

    // --- login ----------------------------------------------------------

    @Test
    fun `login on a device with no cached account resolves the username through discovery`() = runTest {
        val directory = FakeDirectory(
            lookupResult = RecoverAccountLookup(account = accountId, normalisedUsername = "alice"),
        )

        val resolved = resolveLoginAccount(
            cached = null,
            username = " Alice ",
            directory = directory,
            encodeAccountId = fakeEncode,
        )

        assertEquals(listOf("Alice"), directory.lookups)
        assertEquals(fakeEncode(accountId), resolved.accountIdB64)
        assertEquals("alice", resolved.username)
        assertFalse(resolved.fromCache)
    }

    @Test
    fun `login with the cached account does not look it up`() = runTest {
        val directory = FakeDirectory()

        val resolved = resolveLoginAccount(
            cached = CachedAccount(accountIdB64 = "CACHED", username = "alice"),
            username = "alice",
            directory = directory,
            encodeAccountId = fakeEncode,
        )

        assertTrue(directory.lookups.isEmpty())
        assertEquals("CACHED", resolved.accountIdB64)
        assertEquals("alice", resolved.username)
        assertTrue(resolved.fromCache)
    }

    @Test
    fun `login as a different account than the cached one looks the typed username up`() = runTest {
        val other = ByteArray(16) { 9 }
        val directory = FakeDirectory(
            lookupResult = RecoverAccountLookup(account = other, normalisedUsername = "bob"),
        )

        val resolved = resolveLoginAccount(
            cached = CachedAccount(accountIdB64 = "CACHED", username = "alice"),
            username = "bob",
            directory = directory,
            encodeAccountId = fakeEncode,
        )

        assertEquals(listOf("bob"), directory.lookups)
        assertEquals(fakeEncode(other), resolved.accountIdB64)
        assertEquals("bob", resolved.username)
    }

    @Test
    fun `login with an unknown username on a fresh device fails as invalid credentials`() = runTest {
        val directory = FakeDirectory(
            lookupResult = RecoverAccountLookup(account = null, normalisedUsername = "nobody"),
        )

        try {
            resolveLoginAccount(null, "nobody", directory, fakeEncode)
            fail("an unregistered username must not resolve")
        } catch (err: AuthHttpError) {
            // 401 is what LoginViewModel renders as "Invalid credentials." — the
            // same copy a wrong password gets, so the form does not reveal
            // which usernames exist.
            assertEquals(401, err.status)
            assertEquals("invalid credentials", err.body)
        }
    }
}
