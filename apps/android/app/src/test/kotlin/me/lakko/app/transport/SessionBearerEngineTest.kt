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
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.SessionPair
import me.lakko.app.auth.SessionRefusal
import me.lakko.app.auth.SessionTokenProvider
import me.lakko.app.auth.SessionTokens
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0073 section 3.3 — the one place every bearer request passes through:
 * a Ktor client over [SessionBearerEngine] over a [MockEngine] standing in
 * for the network, with a provider over a fake refresh route.
 */
class SessionBearerEngineTest {

    private val acct = "AAAAAAAAAAAAAAAAAAAAAA"
    private val now = 1_000_000L
    private val dev = "D".repeat(43)

    private fun pair(n: Int, expiresAtMs: Long = now + 900_000L) = SessionPair(
        accessToken = "v3.$acct.1.$dev.1.$expiresAtMs.access$n",
        accessExpiresAtMs = expiresAtMs,
        refreshToken = "r2.$acct.1.$dev.1.9.refresh$n",
        refreshExpiresAtMs = expiresAtMs + 1_000_000L,
        keyGeneration = 1L,
    )

    private class FakeRefresher : SessionTokenProvider.SessionRefresher {
        var calls = 0
        var answer: suspend () -> SessionPair = { throw IllegalStateException("no answer") }
        override suspend fun refresh(refreshToken: String): SessionPair {
            calls += 1
            return answer()
        }
    }

    private val refresher = FakeRefresher()
    private val refusals = mutableListOf<SessionRefusal>()
    private val provider = SessionTokenProvider(refresher, clock = { now })

    /** The bearer each request carried when it reached the "network". */
    private val seenBearers = mutableListOf<String?>()

    private fun install(pair: SessionPair) {
        provider.install(pair, persist = {}, onRefusal = { refusals += it })
        SessionTokens.provider = provider
    }

    @After
    fun tearDown() {
        SessionTokens.provider = null
    }

    /** A client whose "server" answers with [responses] in order, recording
     *  the bearer of each request. */
    private fun client(vararg responses: Pair<HttpStatusCode, String>): HttpClient {
        var index = 0
        val mock = MockEngine { request ->
            seenBearers += request.headers[HttpHeaders.Authorization]
            val (status, body) = responses[index.coerceAtMost(responses.size - 1)]
            index += 1
            respond(body, status, headersOf(HttpHeaders.ContentType, "application/json"))
        }
        return HttpClient(SessionBearerEngine(mock))
    }

    @Test
    fun `a captured stale token of the account is replaced by the provider's current one`() = runTest {
        install(pair(2))
        client(HttpStatusCode.OK to "{}").use { http ->
            val response = http.get("https://api.test/discovery/identity") {
                header(HttpHeaders.Authorization, "Bearer v3.$acct.1.$dev.1.5.access1")
            }
            assertEquals(HttpStatusCode.OK, response.status)
        }
        assertEquals(listOf("Bearer ${pair(2).accessToken}"), seenBearers)
        assertEquals(0, refresher.calls)
    }

    @Test
    fun `a token about to expire is refreshed before the request goes out`() = runTest {
        install(pair(1, expiresAtMs = now + 60_000L))
        refresher.answer = { pair(2) }
        client(HttpStatusCode.OK to "{}").use { http ->
            http.get("https://api.test/auth/x") { header(HttpHeaders.Authorization, "Bearer ${pair(1).accessToken}") }
        }
        assertEquals(1, refresher.calls)
        assertEquals(listOf("Bearer ${pair(2).accessToken}"), seenBearers)
    }

    @Test
    fun `a request with no bearer, a refresh credential or another account's token goes through untouched`() = runTest {
        install(pair(2))
        client(HttpStatusCode.OK to "{}").use { http ->
            http.get("https://api.test/discovery/lookup")
            http.post("https://api.test/auth/session/refresh") {
                header(HttpHeaders.Authorization, "Bearer r2.$acct.1.$dev.1.9.refresh1")
            }
            http.get("https://api.test/x") { header(HttpHeaders.Authorization, "Bearer v3.BBBBBBBBBBBBBBBBBBBBBB.1.$dev.1.5.t") }
        }
        assertEquals(
            listOf(null, "Bearer r2.$acct.1.$dev.1.9.refresh1", "Bearer v3.BBBBBBBBBBBBBBBBBBBBBB.1.$dev.1.5.t"),
            seenBearers,
        )
        assertEquals(0, refresher.calls)
    }

    @Test
    fun `a plain 401 refreshes once and retries once with the new token`() = runTest {
        install(pair(1))
        refresher.answer = { pair(2) }
        client(HttpStatusCode.Unauthorized to """{"error":"unauthorized"}""", HttpStatusCode.OK to """{"ok":true}""")
            .use { http ->
                val response = http.post("https://api.test/discovery/identity") {
                    header(HttpHeaders.Authorization, "Bearer ${pair(1).accessToken}")
                    setBody("{}")
                }
                assertEquals(HttpStatusCode.OK, response.status)
                assertEquals("""{"ok":true}""", response.bodyAsText())
            }
        assertEquals(1, refresher.calls)
        assertEquals(listOf("Bearer ${pair(1).accessToken}", "Bearer ${pair(2).accessToken}"), seenBearers)
        assertTrue(refusals.isEmpty())
    }

    @Test
    fun `a second 401 after the retry is handed back, not retried again`() = runTest {
        install(pair(1))
        refresher.answer = { pair(2) }
        client(HttpStatusCode.Unauthorized to """{"error":"x"}""").use { http ->
            val response = http.get("https://api.test/auth/x") {
                header(HttpHeaders.Authorization, "Bearer ${pair(1).accessToken}")
            }
            assertEquals(HttpStatusCode.Unauthorized, response.status)
            // The body still reads after the engine classified it.
            assertEquals("""{"error":"x"}""", response.bodyAsText())
        }
        assertEquals(1, refresher.calls)
        assertEquals(2, seenBearers.size)
    }

    @Test
    fun `a 401 whose refresh cannot be made is handed back without a retry`() = runTest {
        install(pair(1))
        refresher.answer = { throw AuthHttpError(503, "/auth/session/refresh", "") }
        client(HttpStatusCode.Unauthorized to "{}").use { http ->
            val response = http.get("https://api.test/auth/x") {
                header(HttpHeaders.Authorization, "Bearer ${pair(1).accessToken}")
            }
            assertEquals(HttpStatusCode.Unauthorized, response.status)
        }
        assertEquals(1, seenBearers.size)
    }

    @Test
    fun `key-generation-superseded is reported to the provider and never retried`() = runTest {
        install(pair(1))
        client(HttpStatusCode.Unauthorized to """{"error":"key-generation-superseded"}""").use { http ->
            val response = http.post("https://api.test/discovery/identity") {
                header(HttpHeaders.Authorization, "Bearer ${pair(1).accessToken}")
                setBody("{}")
            }
            assertEquals(HttpStatusCode.Unauthorized, response.status)
            assertEquals("""{"error":"key-generation-superseded"}""", response.bodyAsText())
        }
        assertEquals(0, refresher.calls)
        assertEquals(1, seenBearers.size)
        assertEquals(listOf(SessionRefusal.KeysReset), refusals)
    }

    @Test
    fun `a late superseded answer to a replaced token goes out again and wipes nothing`() = runTest {
        // This device's own key rotation: the request left with the old token,
        // the new pair was installed, and the refusal arrived after that.
        install(pair(1))
        var sends = 0
        val mock = MockEngine { request ->
            seenBearers += request.headers[HttpHeaders.Authorization]
            sends += 1
            if (sends == 1) {
                provider.update(pair(2))
                respond(
                    """{"error":"key-generation-superseded"}""",
                    HttpStatusCode.Unauthorized,
                    headersOf(HttpHeaders.ContentType, "application/json"),
                )
            } else {
                respond("{}", HttpStatusCode.OK, headersOf(HttpHeaders.ContentType, "application/json"))
            }
        }
        HttpClient(SessionBearerEngine(mock)).use { http ->
            val response = http.get("https://api.test/auth/device/requests") {
                header(HttpHeaders.Authorization, "Bearer ${pair(1).accessToken}")
            }
            assertEquals(HttpStatusCode.OK, response.status)
        }
        assertEquals(listOf("Bearer ${pair(1).accessToken}", "Bearer ${pair(2).accessToken}"), seenBearers)
        assertTrue(refusals.isEmpty())
        assertEquals(0, refresher.calls)
    }

    @Test
    fun `a superseded answer to the retry is reported too`() = runTest {
        install(pair(1))
        refresher.answer = { pair(2) }
        client(
            HttpStatusCode.Unauthorized to """{"error":"expired"}""",
            HttpStatusCode.Unauthorized to """{"error":"key-generation-superseded"}""",
        ).use { http ->
            val response = http.get("https://api.test/auth/x") {
                header(HttpHeaders.Authorization, "Bearer ${pair(1).accessToken}")
            }
            assertEquals(HttpStatusCode.Unauthorized, response.status)
        }
        assertEquals(listOf(SessionRefusal.KeysReset), refusals)
    }

    @Test
    fun `with no provider installed every request goes through as it is`() = runTest {
        SessionTokens.provider = null
        client(HttpStatusCode.Unauthorized to "{}").use { http ->
            http.get("https://api.test/x") { header(HttpHeaders.Authorization, "Bearer v3.$acct.1.$dev.1.5.old") }
        }
        assertEquals(listOf("Bearer v3.$acct.1.$dev.1.5.old"), seenBearers)
    }

    // ---- ADR-0074: this device was removed ---------------------------------

    @Test
    fun `auth's 401 device-revoked is reported as a removal and never retried`() = runTest {
        install(pair(1))
        client(HttpStatusCode.Unauthorized to """{"error":"device-revoked"}""").use { http ->
            val response = http.get("https://api.test/auth/x") {
                header(HttpHeaders.Authorization, "Bearer ${pair(1).accessToken}")
            }
            assertEquals(HttpStatusCode.Unauthorized, response.status)
            assertEquals("""{"error":"device-revoked"}""", response.bodyAsText())
        }
        assertEquals(0, refresher.calls)
        assertEquals(1, seenBearers.size)
        assertEquals(listOf(SessionRefusal.DeviceRevoked), refusals)
    }

    @Test
    fun `a device-revoked answer to the retry is reported too`() = runTest {
        install(pair(1))
        refresher.answer = { pair(2) }
        client(
            HttpStatusCode.Unauthorized to """{"error":"expired"}""",
            HttpStatusCode.Unauthorized to """{"error":"device-revoked"}""",
        ).use { http ->
            http.get("https://api.test/auth/x") { header(HttpHeaders.Authorization, "Bearer ${pair(1).accessToken}") }
        }
        assertEquals(listOf(SessionRefusal.DeviceRevoked), refusals)
    }

    @Test
    fun `discovery's 403 device-revoked proves nothing - auth is asked, and its yes keeps the device`() = runTest {
        install(pair(1))
        refresher.answer = { pair(2) }
        client(HttpStatusCode.Forbidden to """{"error":"device-revoked"}""").use { http ->
            val response = http.post("https://api.test/discovery/keypackage") {
                header(HttpHeaders.Authorization, "Bearer ${pair(1).accessToken}")
                setBody("{}")
            }
            // The response goes back as it came, body included.
            assertEquals(HttpStatusCode.Forbidden, response.status)
            assertEquals("""{"error":"device-revoked"}""", response.bodyAsText())
        }
        // Auth was asked once (the refresh) and answered with a pair: no
        // removal, nothing wiped, and the request is not retried.
        assertEquals(1, refresher.calls)
        assertEquals(1, seenBearers.size)
        assertEquals(emptyList<SessionRefusal>(), refusals)
        assertEquals(pair(2), provider.peek())
    }

    @Test
    fun `discovery's 403 device-revoked followed by auth's device-revoked is the removal`() = runTest {
        install(pair(1))
        refresher.answer = {
            throw me.lakko.app.auth.AuthHttpError(401, "/auth/session/refresh", "{}", code = "device-revoked")
        }
        client(HttpStatusCode.Forbidden to """{"error":"device-revoked"}""").use { http ->
            http.post("https://api.test/discovery/keypackage") {
                header(HttpHeaders.Authorization, "Bearer ${pair(1).accessToken}")
                setBody("{}")
            }
        }
        assertEquals(1, refresher.calls)
        assertEquals(listOf(SessionRefusal.DeviceRevoked), refusals)
    }

    @Test
    fun `any other 403 is handed back untouched, with no refresh`() = runTest {
        install(pair(1))
        for (body in listOf("""{"error":"device-mismatch"}""", """{"error":"account-key-required"}""", "", "not json")) {
            client(HttpStatusCode.Forbidden to body).use { http ->
                val response = http.get("https://api.test/discovery/x") {
                    header(HttpHeaders.Authorization, "Bearer ${pair(1).accessToken}")
                }
                assertEquals(HttpStatusCode.Forbidden, response.status)
                assertEquals(body, response.bodyAsText())
            }
        }
        assertEquals(0, refresher.calls)
        assertEquals(emptyList<SessionRefusal>(), refusals)
    }
}
