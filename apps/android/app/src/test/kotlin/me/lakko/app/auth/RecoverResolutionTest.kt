// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * ADR-0054 S5 - sign in and recover by name: which name a flow targets, that
 * the request carries its login handle (never an account id, never a lookup),
 * and that a successful sign-in re-binds the handle.
 */
class RecoverResolutionTest {

    private val asked = mutableListOf<String>()

    private val source = LoginHandleSource { name ->
        asked += name
        NamedHandle(username = name.trim().removePrefix("@").lowercase(), handle = ByteArray(32) { name.length.toByte() })
    }

    @Test
    fun `the typed name wins over the cached account`() {
        val named = resolveNamedSignIn(CachedAccount("CACHED", "alice"), " Bob ", source)
        assertEquals(listOf("Bob"), asked)
        assertEquals("bob", named.username)
    }

    @Test
    fun `a blank field falls back to the name cached on this device`() {
        val named = resolveNamedSignIn(CachedAccount("CACHED", "alice"), "  ", source)
        assertEquals(listOf("alice"), asked)
        assertEquals("alice", named.username)
        assertEquals(32, named.handle.size)
    }

    @Test
    fun `a fresh device with a blank field is asked for the name`() {
        for (cached in listOf(null, CachedAccount("GUEST", ""))) {
            try {
                resolveNamedSignIn(cached, null, source)
                fail("nothing to name the account by")
            } catch (err: AuthClientArgError) {
                assertEquals(NO_NAME_COPY, err.message)
            }
        }
        assertTrue(asked.isEmpty())
    }

    @Test
    fun `a name failing the rule reads as the rule's copy`() {
        val refusing = LoginHandleSource {
            throw uniffi.lakko_ffi_uniffi.FfiException.UsernameInvalidCharacters("bad")
        }
        try {
            resolveNamedSignIn(null, "a-b", refusing)
            fail("refused")
        } catch (err: AuthClientArgError) {
            assertEquals(me.lakko.app.profile.UsernameProblem.INVALID_CHARACTERS.copy, err.message)
        }
    }

    @Test
    fun `a sign-in by name calls with the handle and then re-binds it with the new session`() = runTest {
        val sent = mutableListOf<ByteArray>()
        val rebinds = mutableListOf<Pair<String, ByteArray>>()
        val (named, result) = signInByName(
            cached = null,
            typed = "alice",
            source = source,
            call = { sent += it.handle; "session-1" },
            sessionTokenOf = { it },
            rebind = { token, handle -> rebinds += token to handle },
        )
        assertEquals("session-1", result)
        assertArrayEquals(named.handle, sent.single())
        assertEquals("session-1", rebinds.single().first)
        assertArrayEquals(named.handle, rebinds.single().second)
    }

    @Test
    fun `a failed sign-in binds nothing`() = runTest {
        val rebinds = mutableListOf<String>()
        try {
            signInByName(
                cached = null,
                typed = "alice",
                source = source,
                call = { throw AuthHttpError(401, "/login/finish", "invalid credentials") },
                sessionTokenOf = { _: String -> "never" },
                rebind = { token, _ -> rebinds += token },
            )
            fail("wrong password")
        } catch (err: AuthHttpError) {
            assertEquals(401, err.status)
        }
        assertTrue(rebinds.isEmpty())
    }
}
