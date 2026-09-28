// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.test.runTest
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.discovery.DiscoveryNetworkError
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * The username claim at signup (ADR-0054 S5 claim order): the login handle is
 * bound at auth FIRST, the name registered at discovery SECOND; an auth `409`
 * stops the claim before discovery, and a discovery failure releases the
 * handle again.
 *
 * `AuthRepository` itself cannot be constructed on the JVM (Keystore-backed
 * stores, the native core), so these drive the helper it calls.
 */
class AccountDirectoryTest {

    private val accountId = ByteArray(16) { (it + 1).toByte() }
    private val handle = ByteArray(32) { 5 }

    /** Every call, in order, across the two registries. */
    private val calls = mutableListOf<String>()

    private val handles = LoginHandleSource { name ->
        calls += "handle:$name"
        NamedHandle(username = name.trim().lowercase(), handle = handle)
    }

    private inner class FakeBinder(private val onBind: () -> LoginHandleBinding = { LoginHandleBinding.Bound }) :
        LoginHandleBinder {
        val bound = mutableListOf<Pair<String, ByteArray>>()
        var unbinds = 0

        override suspend fun bind(sessionToken: String, handle: ByteArray): LoginHandleBinding {
            calls += "bind"
            bound += sessionToken to handle
            return onBind()
        }

        override suspend fun unbind(sessionToken: String): Boolean {
            calls += "unbind"
            unbinds += 1
            return true
        }
    }

    private inner class FakeDirectory(private val onRegister: (String) -> String = { it.lowercase() }) :
        AccountDirectory {
        val registrations = mutableListOf<Triple<String, ByteArray, String>>()

        override suspend fun registerUsername(
            sessionToken: String,
            accountId: ByteArray,
            username: String,
        ): String {
            calls += "register"
            registrations += Triple(sessionToken, accountId, username)
            return onRegister(username)
        }
    }

    @Test
    fun `the handle is bound at auth before the name is registered at discovery`() = runTest {
        val binder = FakeBinder()
        val directory = FakeDirectory(onRegister = { "alice" })

        val normalised = claimUsernameAtSignup(
            directory = directory,
            binder = binder,
            handles = handles,
            sessionToken = "session-token-1",
            accountId = accountId,
            username = "  Alice ",
        )

        assertEquals(listOf("handle:Alice", "bind", "register"), calls)
        val (bindToken, boundHandle) = binder.bound.single()
        assertEquals("session-token-1", bindToken)
        assertArrayEquals(handle, boundHandle)
        val (token, boundAccount, sentName) = directory.registrations.single()
        assertEquals("session-token-1", token)
        assertArrayEquals(accountId, boundAccount)
        assertEquals("Alice", sentName)
        assertEquals("alice", normalised)
        assertEquals(0, binder.unbinds)
    }

    @Test
    fun `an auth 409 stops the claim before discovery`() = runTest {
        val binder = FakeBinder(onBind = { LoginHandleBinding.Taken })
        val directory = FakeDirectory()
        try {
            claimUsernameAtSignup(directory, binder, handles, "token", accountId, "alice")
            fail("a taken handle must fail signup")
        } catch (err: AuthClientArgError) {
            assertEquals("username @alice is already taken — pick another", err.message)
        }
        assertEquals(listOf("handle:alice", "bind"), calls)
        assertTrue(directory.registrations.isEmpty())
    }

    @Test
    fun `a name taken at discovery releases the handle and reads as taken`() = runTest {
        val binder = FakeBinder()
        val directory = FakeDirectory(onRegister = {
            throw DiscoveryHttpError(409, "/discovery/register", "username already registered")
        })
        try {
            claimUsernameAtSignup(directory, binder, handles, "token", accountId, "alice")
            fail("a taken username must fail signup")
        } catch (err: AuthClientArgError) {
            assertEquals("username @alice is already taken — pick another", err.message)
        }
        assertEquals(listOf("handle:alice", "bind", "register", "unbind"), calls)
    }

    @Test
    fun `every other registration failure releases the handle and propagates unchanged`() = runTest {
        val forbidden = DiscoveryHttpError(403, "/discovery/register", "account_id mismatch")
        val network = DiscoveryNetworkError("/discovery/register", RuntimeException("offline"))
        for (failure in listOf(forbidden, network)) {
            calls.clear()
            val binder = FakeBinder()
            try {
                claimUsernameAtSignup(FakeDirectory(onRegister = { throw failure }), binder, handles, "token", accountId, "alice")
                fail("a registration failure must fail signup")
            } catch (err: Throwable) {
                assertSame(failure, err)
            }
            assertEquals(1, binder.unbinds)
        }
    }

    @Test
    fun `a name failing the username rule never reaches either registry`() = runTest {
        val binder = FakeBinder()
        val directory = FakeDirectory()
        val refusing = LoginHandleSource { throw uniffi.lakko_ffi_uniffi.FfiException.UsernameTooShort("too short") }
        try {
            claimUsernameAtSignup(directory, binder, refusing, "token", accountId, "al")
            fail("a name failing the rule must fail signup")
        } catch (err: AuthClientArgError) {
            assertEquals(me.lakko.app.profile.UsernameProblem.TOO_SHORT.copy, err.message)
        }
        assertTrue(binder.bound.isEmpty())
        assertTrue(directory.registrations.isEmpty())
    }

    @Test
    fun `the directory offers no lookup, so no sign-in can resolve a name before a session`() {
        val methods = AccountDirectory::class.java.declaredMethods.map { it.name }.toSet()
        assertEquals(setOf("registerUsername"), methods)
    }
}
