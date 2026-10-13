// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import me.lakko.app.StaleBindingException
import me.lakko.app.contacts.AcceptedContactsStore
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * A sign-in whose round trip started under a binding that has since ended
 * does not land. Driven through the TOTP recovery, the one sign-in whose
 * client half is plain HTTP (no OPAQUE, so no native library on the host);
 * password and passkey login, recover and the contact recovery ask the same
 * question at the same point.
 */
@RunWith(RobolectricTestRunner::class)
class OvertakenSignInTest {

    private lateinit var context: Context
    private val caller = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val accountId = ByteArray(16) { 0x0A }
    private val accountIdB64Url =
        java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(accountId)

    /** The server's answer waits here, so the test decides what happens
     *  while the round trip is out. */
    private val roundTripStarted = CompletableDeferred<Unit>()
    private val answer = CompletableDeferred<Unit>()

    private fun repository(): AuthRepository {
        val engine = MockEngine {
            roundTripStarted.complete(Unit)
            answer.await()
            respond(
                """{"account_id_b64":"$accountIdB64Url","session_token":"token","session_expires_at_ms":4102444800000}""",
                HttpStatusCode.OK,
                headersOf(HttpHeaders.ContentType, "application/json"),
            )
        }
        return AuthRepository(
            applicationContext = context,
            authClientFactory = { AuthClient(baseUrl = "https://test.invalid", engine = engine) },
            loginHandleSource = LoginHandleSource { name -> NamedHandle(username = name, handle = ByteArray(32) { 1 }) },
        )
    }

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        AcceptedContactsStore.unbind()
    }

    @After
    fun tearDown() {
        caller.cancel()
        AcceptedContactsStore.unbind()
    }

    @Test
    fun `a sign-in that is not overtaken lands`() = runBlocking {
        val auth = repository()
        val signIn = caller.async { auth.recoverWithTotp("alice", "123456") }
        withTimeout(5_000) { roundTripStarted.await() }
        answer.complete(Unit)
        withTimeout(5_000) { signIn.await() }

        // No cached seed on this device: parked on the restore screen.
        val landed = auth.state.value
        assertTrue("landed in $landed", landed is AuthState.NeedsRecoveryPhrase)
    }

    @Test
    fun `a sign-in overtaken by a sign-out does not land`() = runBlocking {
        val auth = repository()
        val signIn = caller.async { auth.recoverWithTotp("alice", "123456") }
        withTimeout(5_000) { roundTripStarted.await() }

        auth.logout()
        val afterSignOut = auth.state.value
        answer.complete(Unit)

        try {
            withTimeout(5_000) { signIn.await() }
            fail("a sign-in that outlived the sign-out landed")
        } catch (_: StaleBindingException) {
            // expected: a cancellation, nothing to show
        }
        assertEquals(afterSignOut, auth.state.value)
        assertTrue(auth.state.value is AuthState.Anon)
    }

    @Test
    fun `of two sign-ins racing, the one that lands second is dropped`() = runBlocking {
        val auth = repository()
        // Both round trips are out before either answer arrives.
        val first = caller.async { auth.recoverWithTotp("alice", "123456") }
        val second = caller.async { auth.recoverWithTotp("alice", "123456") }
        withTimeout(5_000) { roundTripStarted.await() }
        answer.complete(Unit)

        val outcomes = listOf(first, second).map { signIn ->
            try {
                withTimeout(5_000) { signIn.await() }
                "landed"
            } catch (_: StaleBindingException) {
                "dropped"
            }
        }
        assertEquals(listOf("dropped", "landed"), outcomes.sorted())
        assertTrue(auth.state.value is AuthState.NeedsRecoveryPhrase)
    }
}
