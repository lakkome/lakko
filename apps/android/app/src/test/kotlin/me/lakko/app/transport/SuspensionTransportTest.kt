// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.transport

import io.ktor.client.HttpClient
import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.bodyAsText
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import me.lakko.app.auth.ACCOUNT_SUSPENDED
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.SessionPair
import me.lakko.app.auth.SessionRefusal
import me.lakko.app.auth.SessionTokenProvider
import me.lakko.app.auth.SessionTokens
import me.lakko.app.auth.SuspendedCredentials
import me.lakko.app.auth.SuspendedWriteRefused
import me.lakko.app.auth.SuspensionGate
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * ADR-0099 section 4 - the transport's two parts in a suspension: a bearer
 * route's bare `403 account-suspended` makes the provider refresh once (the
 * refresh answers the restricted session), and while suspended no write to
 * the Delivery Service leaves the device.
 */
class SuspensionTransportTest {

    private val acct = "AAAAAAAAAAAAAAAAAAAAAA"
    private val dev = "D".repeat(43)
    private val pair = SessionPair("v3.$acct.1.$dev.1.9.a", Long.MAX_VALUE, "r2.$acct.1.$dev.1.9.r", Long.MAX_VALUE, 1L)
    private val creds = SuspendedCredentials(
        accountIdB64 = "AAAAAAAAAAAAAAAAAAAAAA==",
        restrictedToken = "s1.$acct.1.$dev.1.9.s",
        restrictedExpiresAtMs = 5L,
        refreshToken = "r2.$acct.1.$dev.1.9.r2",
        refreshExpiresAtMs = 6L,
        keyGeneration = 1L,
    )

    private var refreshCalls = 0
    private val suspended = mutableListOf<SuspendedCredentials>()
    private val refusals = mutableListOf<SessionRefusal>()
    private val seen = mutableListOf<String>()

    private val provider = SessionTokenProvider(
        refresher = object : SessionTokenProvider.SessionRefresher {
            override suspend fun refresh(refreshToken: String): SessionPair {
                refreshCalls += 1
                throw AuthHttpError(403, "/auth/session/refresh", "{}", code = ACCOUNT_SUSPENDED, suspended = creds)
            }
        },
    )

    @After
    fun tearDown() {
        SessionTokens.provider = null
        SuspensionGate.set(false)
    }

    private fun client(status: HttpStatusCode, body: String): HttpClient =
        HttpClient(
            SessionBearerEngine(
                MockEngine { request ->
                    seen += request.url.toString()
                    respond(body, status, headersOf(HttpHeaders.ContentType, "application/json"))
                },
            ),
        )

    @Test
    fun `a bearer route's 403 account-suspended refreshes once - not a sign-out`() = runTest {
        provider.install(pair, persist = {}, onRefusal = { refusals += it }, onSuspended = { suspended += it })
        SessionTokens.provider = provider
        client(HttpStatusCode.Forbidden, """{"error":"account-suspended"}""").use { http ->
            val response = http.post("https://api.test/auth/tokens") {
                header(HttpHeaders.Authorization, "Bearer ${pair.accessToken}")
                setBody("{}")
            }
            assertEquals(HttpStatusCode.Forbidden, response.status)
            assertEquals("""{"error":"account-suspended"}""", response.bodyAsText())
        }
        assertEquals(1, refreshCalls)
        assertEquals(listOf(creds), suspended)
        assertEquals(emptyList<SessionRefusal>(), refusals)
        assertTrue(provider.isSuspended())
    }

    @Test
    fun `while suspended a DS write is refused before it leaves the device, a read goes out`() = runTest {
        SuspensionGate.set(true)
        val ds = ApiEndpoints.baseUrlFor(ApiService.DS)
        client(HttpStatusCode.OK, "{}").use { http ->
            try {
                http.post("$ds/group/aa/messages") { setBody("x") }
                fail("a DS write went out while suspended")
            } catch (_: SuspendedWriteRefused) {
                // expected
            }
            http.get("$ds/group/aa/messages")
        }
        assertEquals(listOf("$ds/group/aa/messages"), seen)
    }
}
