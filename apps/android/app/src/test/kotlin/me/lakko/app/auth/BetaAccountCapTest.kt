// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import java.io.IOException
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Sign-up when the beta is full (`docs/ux/beta-account-cap.md`): the
 * `/signup/open` question reads any failure as open, a first bind refused at
 * the cap is its own outcome, and the screens' decisions.
 */
class BetaAccountCapTest {

    private val handle = ByteArray(32) { 7 }

    private fun client(status: HttpStatusCode, body: String, seen: MutableList<String> = mutableListOf()) =
        AuthClient(
            baseUrl = "http://test.invalid",
            engine = MockEngine { request ->
                seen += "${request.method.value} ${request.url.encodedPath}"
                respond(body, status, headersOf(HttpHeaders.ContentType, "application/json"))
            },
        )

    @Test
    fun `signup open is an anonymous GET and reads the answer`() = runTest {
        val seen = mutableListOf<String>()
        assertTrue(client(HttpStatusCode.OK, """{"open":true}""", seen).use { it.signupOpen() })
        assertFalse(client(HttpStatusCode.OK, """{"open":false}""", seen).use { it.signupOpen() })
        assertEquals(listOf("GET /signup/open", "GET /signup/open"), seen)
    }

    @Test
    fun `a failed question reads as open`() = runTest {
        // An older server without the route, an error, a body that does not
        // parse, a missing field: the form is shown and the bind decides.
        assertTrue(client(HttpStatusCode.NotFound, "").use { it.signupOpen() })
        assertTrue(client(HttpStatusCode.InternalServerError, """{"error":"internal"}""").use { it.signupOpen() })
        assertTrue(client(HttpStatusCode.OK, "not json").use { it.signupOpen() })
        assertTrue(client(HttpStatusCode.OK, """{}""").use { it.signupOpen() })
        val offline = AuthClient(
            baseUrl = "http://test.invalid",
            engine = MockEngine { throw IOException("no route to host") },
        )
        assertTrue(offline.use { it.signupOpen() })
    }

    @Test
    fun `a first bind refused at the cap is Closed`() = runTest {
        val outcome = client(HttpStatusCode.Forbidden, """{"error":"registration-closed"}""")
            .use { it.bindLoginHandle("tok", handle) }
        assertEquals(LoginHandleBinding.Closed, outcome)
    }

    @Test
    fun `another 403 on the bind is not the cap`() = runTest {
        try {
            client(HttpStatusCode.Forbidden, """{"error":"fresh-auth-required"}""")
                .use { it.bindLoginHandle("tok", handle) }
            throw AssertionError("a fresh-auth refusal must propagate")
        } catch (err: AuthHttpError) {
            assertEquals(403, err.status)
            assertFalse(isRegistrationClosed(err))
        }
    }

    @Test
    fun `the error mapping`() {
        val closed = AuthHttpError(403, "/auth/login-handle", "", code = REGISTRATION_CLOSED_CODE)
        assertTrue(isRegistrationClosed(closed))
        assertTrue(isFullState(closed))
        assertTrue(isFullState(RegistrationClosed()))
        assertFalse(isRegistrationClosed(AuthHttpError(409, "/auth/login-handle", "", code = REGISTRATION_CLOSED_CODE)))
        assertFalse(isFullState(AuthHttpError(403, "/auth/login-handle", "", code = "fresh-auth-required")))
        assertFalse(isFullState(AuthClientArgError("taken")))
    }

    @Test
    fun `the screen decisions`() {
        assertEquals(SignupGate.FORM, signupGate(open = true))
        assertEquals(SignupGate.CLOSED, signupGate(open = false))
        assertEquals(GuestOfferMode.ASK, guestOfferMode(open = true, refusedAtBind = false))
        assertEquals(GuestOfferMode.CLOSED, guestOfferMode(open = false, refusedAtBind = false))
        // The bind is final; a later open answer does not undo it.
        assertEquals(GuestOfferMode.CLOSED, guestOfferMode(open = true, refusedAtBind = true))
    }
}
