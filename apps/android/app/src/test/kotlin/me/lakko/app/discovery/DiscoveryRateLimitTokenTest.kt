// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import me.lakko.app.auth.PowProofPayload
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Discovery rate-limits `/discovery/oprf`, `/discovery/lookup` and
 * `/discovery/channel/lookup` per account when the request carries a session
 * token and per IP otherwise. A client built without a token must still carry
 * the signed-in session's token on those three reads, and on nothing else.
 */
class DiscoveryRateLimitTokenTest {

    private val pow = PowProofPayload(seed = "AA==", nonce = "AA==", solution = "AA==")

    /** Path -> Authorization header value (null when absent). */
    private fun client(
        sessionToken: String?,
        ambient: String?,
        seen: MutableMap<String, String?>,
    ): DiscoveryClient {
        val engine = MockEngine { request ->
            val path = request.url.encodedPath
            seen[path] = request.headers[HttpHeaders.Authorization]
            val (status, body) = when (path) {
                "/discovery/oprf" -> HttpStatusCode.OK to """{"evaluated":"AA=="}"""
                "/discovery/register" -> HttpStatusCode.OK to """{"ok":true}"""
                else -> HttpStatusCode.NotFound to ""
            }
            respond(body, status, headersOf(HttpHeaders.ContentType, "application/json"))
        }
        return DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, rateLimitToken = { ambient })
            .also { it.setSessionToken(sessionToken) }
    }

    @Test
    fun rateLimitedReadsCarryTheSignedInTokenWhenTheClientHasNone() = runTest {
        val seen = mutableMapOf<String, String?>()
        client(sessionToken = null, ambient = "signed-in", seen = seen).use { c ->
            c.oprfEvaluate(OprfRequest(pow = pow, blinded = "AA=="))
            c.lookupRaw(LookupRequest(pow = pow, oprf_hash = "AA=="))
            c.channelLookupRaw(ChannelLookupRequest(pow = pow, oprf_hash = "AA=="))
            c.register(RegisterRequest(pow = pow, account_id = "AA==", oprf_hash = "AA=="))
        }
        assertEquals("Bearer signed-in", seen["/discovery/oprf"])
        assertEquals("Bearer signed-in", seen["/discovery/lookup"])
        assertEquals("Bearer signed-in", seen["/discovery/channel/lookup"])
        // A protected write never borrows the signed-in session's token.
        assertTrue(seen.containsKey("/discovery/register"))
        assertNull(seen["/discovery/register"])
    }

    @Test
    fun theClientsOwnTokenWinsAndSignedOutStaysAnonymous() = runTest {
        val seen = mutableMapOf<String, String?>()
        client(sessionToken = "own", ambient = "signed-in", seen = seen).use { c ->
            c.lookupRaw(LookupRequest(pow = pow, oprf_hash = "AA=="))
        }
        assertEquals("Bearer own", seen["/discovery/lookup"])

        seen.clear()
        client(sessionToken = null, ambient = null, seen = seen).use { c ->
            c.lookupRaw(LookupRequest(pow = pow, oprf_hash = "AA=="))
        }
        assertTrue(seen.containsKey("/discovery/lookup"))
        assertNull(seen["/discovery/lookup"])
    }

    @Test
    fun ambientTokenTreatsEmptyAsSignedOut() {
        try {
            AmbientSessionToken.install { "" }
            assertNull(AmbientSessionToken.current)
            AmbientSessionToken.install { "t" }
            assertEquals("t", AmbientSessionToken.current)
        } finally {
            AmbientSessionToken.install { null }
        }
    }
}
