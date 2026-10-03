// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * ADR-0054 S5 - a session that lands by any path binds its login handle once
 * per (session, username) per process, best-effort and silent.
 */
class LoginHandleSessionBindTest {

    private val now = 1_800_000_000_000L
    private val accountBytes = ByteArray(16) { (it + 1).toByte() }
    private val accountB64 = base64Encode(accountBytes)

    private fun handleOf(name: String): ByteArray = ByteArray(32) { (name.hashCode() + it).toByte() }

    private val source = LoginHandleSource { name ->
        NamedHandle(username = name.lowercase(), handle = handleOf(name.lowercase()))
    }

    private class FakeBinder(var answer: () -> LoginHandleBinding = { LoginHandleBinding.Bound }) : LoginHandleBinder {
        val binds = mutableListOf<Pair<String, ByteArray>>()

        override suspend fun bind(sessionToken: String, handle: ByteArray): LoginHandleBinding {
            binds += sessionToken to handle.copyOf()
            return answer()
        }

        override suspend fun unbind(sessionToken: String): Boolean = error("never unbinds")
    }

    private val launched = mutableListOf<suspend () -> Unit>()
    private val logs = mutableListOf<String>()

    private fun rebinder(binder: LoginHandleBinder) = LoginHandleSessionRebinder(
        source = source,
        binder = binder,
        launch = { launched += it },
        log = { logs += it },
    )

    private suspend fun runLaunched() {
        val pending = launched.toList()
        launched.clear()
        pending.forEach { it() }
    }

    private fun restoreSource(username: String) = object : SessionRestoreSource {
        override fun loadMarker() = PersistedSession(accountIdB64 = accountB64, username = username)
        override fun hasDeviceKey(accountIdB64: String) = true
        override fun loadSessionPair(accountIdB64: String) = SessionPair(
            accessToken = "v1.restored.tok.sig",
            accessExpiresAtMs = now + 60_000,
            refreshToken = "r1.restored.ref.sig",
            refreshExpiresAtMs = now + 60_000,
            keyGeneration = 1L,
        )
        override fun loadAccountIdentitySeed(accountIdB64: String) = ByteArray(64) { 7 }
        override fun loadExportKey(accountIdB64: String) = ByteArray(32) { 3 }
        override fun discardSessionToken(accountIdB64: String) = Unit
    }

    /** What the repository's observer sees after a cold-start restore. */
    private fun restoredTarget(username: String): LoginHandleBindTarget? {
        val decision = decideSessionRestore(restoreSource(username), now) { _, _ -> }
        if (decision !is SessionRestoreDecision.Restored) fail("expected Restored, got $decision")
        decision as SessionRestoreDecision.Restored
        return loginHandleBindTarget(AuthState.Session(decision.account), decision.secrets)
    }

    private fun secrets(token: String, account: ByteArray = accountBytes) = IdentitySecrets(
        accountId = account,
        exportKey = ByteArray(32),
        accountIdentitySeed = ByteArray(64),
        session = SessionPair(
            accessToken = token,
            accessExpiresAtMs = now + 60_000,
            refreshToken = "r1.$token",
            refreshExpiresAtMs = now + 60_000,
            keyGeneration = 1L,
        ),
    )

    @Test
    fun `a restored session binds its handle once with the restored token`() = runTest {
        val binder = FakeBinder()
        val target = restoredTarget("alice") ?: error("restore has a name to bind")
        rebinder(binder).onSessionAvailable(target)
        runLaunched()
        assertEquals(1, binder.binds.size)
        assertEquals("v1.restored.tok.sig", binder.binds[0].first)
        assertArrayEquals(handleOf("alice"), binder.binds[0].second)
        assertTrue(logs.isEmpty())
    }

    @Test
    fun `an account with no username binds nothing`() {
        assertNull(restoredTarget(""))
        assertNull(restoredTarget("   "))
    }

    @Test
    fun `no session, no secrets or another account's secrets bind nothing`() {
        val account = SessionAccount(accountIdB64 = accountB64, username = "alice")
        assertNull(loginHandleBindTarget(AuthState.Session(account), null))
        assertNull(loginHandleBindTarget(AuthState.NeedsRecoveryPhrase(account), secrets("t")))
        assertNull(loginHandleBindTarget(AuthState.Anon("alice"), secrets("t")))
        assertNull(loginHandleBindTarget(AuthState.Session(account), secrets("t", ByteArray(16) { 9 })))
    }

    @Test
    fun `a 409 changes nothing and is logged by status only`() = runTest {
        val binder = FakeBinder { LoginHandleBinding.Taken }
        val r = rebinder(binder)
        r.onSessionAvailable(LoginHandleBindTarget("tok-1", "alice"))
        runLaunched()
        assertEquals(1, binder.binds.size)
        assertEquals(listOf("login-handle bind: status 409"), logs)
        // Spent: the same session landing again does not retry.
        r.onSessionAvailable(LoginHandleBindTarget("tok-1", "alice"))
        runLaunched()
        assertEquals(1, binder.binds.size)
    }

    @Test
    fun `an error is logged by status and login-by-name-disabled is silent`() = runTest {
        val binder = FakeBinder { throw AuthHttpError(500, "/auth/login-handle", "boom") }
        val r = rebinder(binder)
        r.onSessionAvailable(LoginHandleBindTarget("tok-1", "alice"))
        runLaunched()
        assertEquals(listOf("login-handle bind: status 500"), logs)

        binder.answer = {
            throw AuthHttpError(503, "/auth/login-handle", "", code = LOGIN_BY_NAME_DISABLED)
        }
        r.onSessionAvailable(LoginHandleBindTarget("tok-2", "alice"))
        runLaunched()
        assertEquals(2, binder.binds.size)
        assertEquals(listOf("login-handle bind: status 500"), logs)
    }

    @Test
    fun `the same session installed twice binds once`() = runTest {
        val binder = FakeBinder()
        val r = rebinder(binder)
        val account = SessionAccount(accountIdB64 = accountB64, username = "alice")
        repeat(2) {
            val target = loginHandleBindTarget(AuthState.Session(account), secrets("tok-1"))
                ?: error("a target")
            r.onSessionAvailable(target)
        }
        runLaunched()
        assertEquals(1, binder.binds.size)
        // A new session for the same name is a new pair.
        r.onSessionAvailable(LoginHandleBindTarget("tok-2", "alice"))
        runLaunched()
        assertEquals(listOf("tok-1", "tok-2"), binder.binds.map { it.first })
    }

    @Test
    fun `a sign-in by name and a signup claim spend the pair the landing would bind`() = runTest {
        val binder = FakeBinder()
        val r = rebinder(binder)
        r.bind("tok-1", handleOf("alice"))
        r.onSessionAvailable(LoginHandleBindTarget("tok-1", "alice"))
        r.markBound("tok-2", "bob")
        r.onSessionAvailable(LoginHandleBindTarget("tok-2", "bob"))
        runLaunched()
        assertEquals(listOf("tok-1"), binder.binds.map { it.first })
    }

    @Test
    fun `a name the username rule refuses binds nothing`() = runTest {
        val binder = FakeBinder()
        val r = LoginHandleSessionRebinder(
            source = { throw uniffi.lakko_ffi_uniffi.FfiException.UsernameInvalidCharacters("bad") },
            binder = binder,
            launch = { launched += it },
            log = { logs += it },
        )
        r.onSessionAvailable(LoginHandleBindTarget("tok-1", "a-b"))
        runLaunched()
        assertTrue(binder.binds.isEmpty())
        assertEquals(1, logs.size)
    }
}
