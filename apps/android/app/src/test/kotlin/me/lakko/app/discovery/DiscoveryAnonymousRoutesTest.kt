// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import me.lakko.app.anon.AnonTokenContext
import me.lakko.app.anon.AnonTokenQuotaExhausted
import me.lakko.app.anon.FakeAnonTokenSource
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0054 S3 - which discovery routes spend an anonymous token, and the
 * privacy rule: a token route (and the lookups that follow an OPRF
 * evaluation) NEVER carries `Authorization`, even when the client holds a
 * session for its bearer writes. Nothing calls `/pow/challenge` any more.
 */
class DiscoveryAnonymousRoutesTest {

    private data class Seen(val path: String, val authorization: String?, val token: String?)

    private fun client(
        seen: MutableList<Seen>,
        tokens: FakeAnonTokenSource,
        answer: (path: String, call: Int) -> Pair<HttpStatusCode, String> = { path, _ -> default(path) },
    ): DiscoveryClient {
        val calls = mutableMapOf<String, Int>()
        val engine = MockEngine { request ->
            val path = request.url.encodedPath
            seen += Seen(
                path = path,
                authorization = request.headers[HttpHeaders.Authorization],
                token = request.headers["x-lakko-anon-token"],
            )
            val n = calls.merge(path, 1, Int::plus)!!
            val (status, body) = answer(path, n)
            respond(body, status, headersOf(HttpHeaders.ContentType, "application/json"))
        }
        return DiscoveryClient(
            baseUrl = "http://test.invalid",
            engine = engine,
            anonTokens = tokens,
            inviteLinkLengths = {
                InviteLinkLengths(blobIdLen = 6, blobIdUrlLen = 8, linkSecretLen = 16, linkSecretUrlLen = 22)
            },
        ).also { it.setSessionToken("session-token") }
    }

    private fun default(path: String): Pair<HttpStatusCode, String> = when {
        path == "/discovery/oprf" -> HttpStatusCode.OK to """{"evaluated":"AA=="}"""
        path == "/discovery/register" -> HttpStatusCode.OK to """{"ok":true}"""
        path == "/discovery/channel/register" -> HttpStatusCode.OK to """{"ok":true}"""
        path.endsWith("/consume") ->
            HttpStatusCode.OK to """{"used_count":1,"max_uses":5,"already_consumed":false}"""
        else -> HttpStatusCode.NotFound to ""
    }

    @Test
    fun tokenRoutesCarryATokenAndNeverTheSession() = runTest {
        val seen = mutableListOf<Seen>()
        val tokens = FakeAnonTokenSource()
        client(seen, tokens).use { c ->
            c.oprfEvaluate(OprfRequest(blinded = "AA=="))
            c.channelRegister(ChannelRegisterRequest(oprf_hash = "AA==", channel_id = "AA=="))
            c.inviteConsume("AAAAAAAA", InviteConsumeRequest(joiner_tag = "AA=="))
            c.callInviteConsume("AAAAAAAA", InviteConsumeRequest(joiner_tag = "AA=="))
        }
        assertEquals(
            listOf(
                AnonTokenContext.DISCOVERY_LOOKUP,
                AnonTokenContext.DISCOVERY_REGISTER,
                AnonTokenContext.INVITE_CONSUME,
                AnonTokenContext.INVITE_CONSUME,
            ),
            tokens.taken,
        )
        assertEquals(4, seen.size)
        for (s in seen) {
            assertNull("${s.path} must not carry Authorization", s.authorization)
            assertTrue("${s.path} must carry a token", s.token != null)
        }
    }

    @Test
    fun lookupsAreAnonymousWithoutAToken() = runTest {
        val seen = mutableListOf<Seen>()
        val tokens = FakeAnonTokenSource()
        client(seen, tokens).use { c ->
            c.lookupRaw(LookupRequest(oprf_hash = "AA=="))
            c.channelLookupRaw(ChannelLookupRequest(oprf_hash = "AA=="))
        }
        assertEquals(listOf("/discovery/lookup", "/discovery/channel/lookup"), seen.map { it.path })
        for (s in seen) {
            assertNull(s.authorization)
            assertNull(s.token)
        }
        assertTrue(tokens.taken.isEmpty())
    }

    @Test
    fun aBearerWriteCarriesTheSessionAndNoToken() = runTest {
        val seen = mutableListOf<Seen>()
        val tokens = FakeAnonTokenSource()
        client(seen, tokens).use { c ->
            c.register(RegisterRequest(account_id = "AA==", oprf_hash = "AA=="))
        }
        assertEquals("Bearer session-token", seen.single().authorization)
        assertNull(seen.single().token)
        assertTrue(tokens.taken.isEmpty())
    }

    @Test
    fun aSpentTokenIsDroppedAndTheRouteRetriedOnceWithAnother() = runTest {
        val seen = mutableListOf<Seen>()
        val tokens = FakeAnonTokenSource()
        client(seen, tokens, answer = { path, call ->
            if (path == "/discovery/oprf" && call == 1) {
                HttpStatusCode.Unauthorized to """{"error":"anon-token-spent"}"""
            } else {
                default(path)
            }
        }).use { c ->
            assertEquals("AA==", c.oprfEvaluate(OprfRequest(blinded = "AA==")).evaluated)
        }
        assertEquals(listOf("tok-1", "tok-2"), seen.map { it.token })
        assertTrue(seen.all { it.authorization == null })
    }

    @Test
    fun aSecondRefusalIsNotRetriedAgain() {
        val seen = mutableListOf<Seen>()
        val err = assertThrows(DiscoveryHttpError::class.java) {
            runTest {
                client(seen, FakeAnonTokenSource(), answer = { _, _ ->
                    HttpStatusCode.Unauthorized to """{"error":"anon-token-expired"}"""
                }).use { c -> c.oprfEvaluate(OprfRequest(blinded = "AA==")) }
            }
        }
        assertEquals(401, err.status)
        assertEquals(2, seen.size)
    }

    @Test
    fun tokenRequiredIsAClientBugAndNotRetried() {
        val seen = mutableListOf<Seen>()
        assertThrows(DiscoveryHttpError::class.java) {
            runTest {
                client(seen, FakeAnonTokenSource(), answer = { _, _ ->
                    HttpStatusCode.Unauthorized to """{"error":"anon-token-required"}"""
                }).use { c -> c.oprfEvaluate(OprfRequest(blinded = "AA==")) }
            }
        }
        assertEquals(1, seen.size)
    }

    @Test
    fun aUsedUpAllowanceNeverReachesTheNetwork() {
        val seen = mutableListOf<Seen>()
        val tokens = FakeAnonTokenSource(
            failWith = AnonTokenQuotaExhausted(AnonTokenContext.INVITE_CONSUME, retryAfterSecs = 3600),
        )
        assertThrows(AnonTokenQuotaExhausted::class.java) {
            runTest {
                client(seen, tokens).use { c ->
                    c.inviteConsume("AAAAAAAA", InviteConsumeRequest(joiner_tag = "AA=="))
                }
            }
        }
        assertTrue(seen.isEmpty())
    }

    @Test
    fun nothingCallsTheOldChallengeRoute() = runTest {
        val seen = mutableListOf<Seen>()
        client(seen, FakeAnonTokenSource()).use { c ->
            c.oprfEvaluate(OprfRequest(blinded = "AA=="))
            c.lookupRaw(LookupRequest(oprf_hash = "AA=="))
            c.register(RegisterRequest(account_id = "AA==", oprf_hash = "AA=="))
        }
        assertFalse(seen.any { it.path.endsWith("/pow/challenge") })
    }
}
