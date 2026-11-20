// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.test.runTest
import me.lakko.app.l10n.UiText
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException

/**
 * A session that lands with no username (a passkey sign-in on a new device)
 * learns it from the backup when auth confirms it, else asks for it.
 */
class UsernamePromptTest {

    private class Ports(
        var username: String = "",
        var backup: List<String> = emptyList(),
        /** The name auth holds for the account, or `null` when it holds none. */
        var accountName: String? = "alice",
        var failure: Throwable? = null,
        var signedIn: Boolean = true,
    ) : UsernamePromptPorts {
        val checks = mutableListOf<String>()
        val adopted = mutableListOf<String>()

        override fun session(): UsernamePromptSession? =
            if (signedIn) UsernamePromptSession("acct", username) else null

        override fun backupCandidates(accountIdB64: String): List<String> = backup

        override fun handleFor(username: String): NamedHandle {
            val canonical = username.trim().lowercase()
            if (canonical.length < 3 || !canonical.all { it in 'a'..'z' || it in '0'..'9' || it == '_' }) {
                throw AuthClientArgError("bad", UiText("username.error.too_short"))
            }
            return NamedHandle(canonical, canonical.toByteArray().copyOf(32))
        }

        override suspend fun check(handle: ByteArray): LoginHandleState {
            val name = String(handle).trimEnd('\u0000')
            checks += name
            failure?.let { throw it }
            val held = accountName ?: return LoginHandleState.None
            return if (held == name) LoginHandleState.Same else LoginHandleState.Other
        }

        override fun adopt(accountIdB64: String, username: String) {
            adopted += username
            this.username = username
        }
    }

    private fun tooMany() = AuthHttpError(status = 429, endpoint = "/auth/login-handle/check", body = "")

    @Test
    fun `the backup's name is checked and adopted with no prompt`() = runTest {
        val ports = Ports(backup = listOf("Alice"))
        val prompt = UsernamePromptController(ports)
        prompt.evaluate()
        assertEquals(listOf("alice"), ports.checks)
        assertEquals(listOf("alice"), ports.adopted)
        assertNull(prompt.state.value)
    }

    @Test
    fun `a backup name that is not the account's is passed over for the next one`() = runTest {
        val ports = Ports(backup = listOf("mallory", "alice"))
        val prompt = UsernamePromptController(ports)
        prompt.evaluate()
        assertEquals(listOf("mallory", "alice"), ports.checks)
        assertEquals(listOf("alice"), ports.adopted)
        assertNull(prompt.state.value)
    }

    @Test
    fun `with no name in the backup the prompt asks`() = runTest {
        val ports = Ports()
        val prompt = UsernamePromptController(ports)
        prompt.evaluate()
        assertEquals(UsernamePromptState(), prompt.state.value)
        assertTrue(ports.checks.isEmpty())
    }

    @Test
    fun `a backup name auth refuses opens the prompt and is not asked again`() = runTest {
        val ports = Ports(backup = listOf("mallory"))
        val prompt = UsernamePromptController(ports)
        prompt.evaluate()
        assertNotNull(prompt.state.value)
        prompt.evaluate()
        assertEquals(listOf("mallory"), ports.checks)
        assertTrue(ports.adopted.isEmpty())
    }

    @Test
    fun `a backup name that could not be checked opens the prompt and is tried again later`() = runTest {
        val ports = Ports(backup = listOf("alice"), failure = IOException("offline"))
        val prompt = UsernamePromptController(ports)
        prompt.evaluate()
        assertNotNull(prompt.state.value)
        ports.failure = null
        prompt.evaluate()
        assertEquals(listOf("alice"), ports.adopted)
        assertNull(prompt.state.value)
    }

    @Test
    fun `a session that has its username shows nothing and checks nothing`() = runTest {
        val ports = Ports(username = "alice", backup = listOf("alice"))
        val prompt = UsernamePromptController(ports)
        prompt.evaluate()
        assertNull(prompt.state.value)
        assertTrue(ports.checks.isEmpty())
    }

    @Test
    fun `no eligible session, a guest or signed out, shows nothing`() = runTest {
        val ports = Ports(signedIn = false)
        val prompt = UsernamePromptController(ports)
        prompt.evaluate()
        assertNull(prompt.state.value)
    }

    @Test
    fun `same adopts the typed name and closes the prompt`() = runTest {
        val ports = Ports()
        val prompt = UsernamePromptController(ports)
        prompt.evaluate()
        prompt.submit("  Alice ")
        assertEquals(listOf("alice"), ports.adopted)
        assertNull(prompt.state.value)
    }

    @Test
    fun `other keeps the prompt with the not-this-account line`() = runTest {
        val ports = Ports()
        val prompt = UsernamePromptController(ports)
        prompt.evaluate()
        prompt.submit("bob")
        assertEquals(UiText("auth.username_prompt.error.not_this"), prompt.state.value?.error)
        assertFalse(prompt.state.value!!.busy)
        assertTrue(ports.adopted.isEmpty())
    }

    @Test
    fun `none closes the prompt for good, nothing adopted`() = runTest {
        val ports = Ports(accountName = null)
        val prompt = UsernamePromptController(ports)
        prompt.evaluate()
        prompt.submit("alice")
        assertNull(prompt.state.value)
        assertTrue(ports.adopted.isEmpty())
        prompt.evaluate()
        assertNull("the account has no username: not asked again", prompt.state.value)
    }

    @Test
    fun `a 429 says to try later`() = runTest {
        val ports = Ports(failure = tooMany())
        val prompt = UsernamePromptController(ports)
        prompt.evaluate()
        prompt.submit("alice")
        assertEquals(UiText("auth.username_prompt.error.too_many"), prompt.state.value?.error)
    }

    @Test
    fun `a network failure or another refusal is a connection line`() = runTest {
        val ports = Ports(failure = IOException("offline"))
        val prompt = UsernamePromptController(ports)
        prompt.evaluate()
        prompt.submit("alice")
        assertEquals(UiText("auth.username_prompt.error.network"), prompt.state.value?.error)
        ports.failure = AuthHttpError(status = 500, endpoint = "/auth/login-handle/check", body = "")
        prompt.submit("alice")
        assertEquals(UiText("auth.username_prompt.error.network"), prompt.state.value?.error)
        ports.failure = null
        prompt.submit("alice")
        assertEquals(listOf("alice"), ports.adopted)
        assertNull(prompt.state.value)
    }

    @Test
    fun `a name that fails the username rule shows the rule's line and makes no request`() = runTest {
        val ports = Ports()
        val prompt = UsernamePromptController(ports)
        prompt.evaluate()
        prompt.submit("a!")
        assertEquals(UiText("username.error.too_short"), prompt.state.value?.error)
        assertTrue(ports.checks.isEmpty())
    }

    @Test
    fun `I don't remember shows the note once, and the next start asks again`() = runTest {
        val ports = Ports()
        val prompt = UsernamePromptController(ports)
        prompt.evaluate()
        prompt.forgot()
        assertEquals(UsernamePromptState(forgotNote = true), prompt.state.value)
        prompt.closeNote()
        assertNull(prompt.state.value)
        prompt.evaluate()
        assertNull("not asked again in this run of the app", prompt.state.value)

        // The next start of the app: a new controller, the name still empty.
        val nextStart = UsernamePromptController(ports)
        nextStart.evaluate()
        assertEquals(UsernamePromptState(), nextStart.state.value)
    }

    @Test
    fun `after I don't remember a backup name auth confirms is still adopted`() = runTest {
        val ports = Ports()
        val prompt = UsernamePromptController(ports)
        prompt.evaluate()
        prompt.forgot()
        prompt.closeNote()
        ports.backup = listOf("alice")
        prompt.evaluate()
        assertEquals(listOf("alice"), ports.adopted)
    }

    @Test
    fun `a 429 is told apart from other failures`() {
        assertEquals(UsernameCheckResult.TOO_MANY, usernameCheckFailure(tooMany()))
        assertEquals(UsernameCheckResult.NETWORK, usernameCheckFailure(IOException("x")))
        assertEquals(
            UsernameCheckResult.NETWORK,
            usernameCheckFailure(AuthHttpError(status = 401, endpoint = "/x", body = "")),
        )
    }
}
