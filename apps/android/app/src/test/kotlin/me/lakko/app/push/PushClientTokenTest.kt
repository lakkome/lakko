// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.content.OutgoingContent
import io.ktor.http.content.TextContent
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import me.lakko.app.anon.AnonSession
import me.lakko.app.anon.AnonTokenContext
import me.lakko.app.anon.DetachedAnonTokenSource
import me.lakko.app.anon.FakeAnonTokenSource
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0054 S3 with ADR-0078 - push-gw's two subscription routes each spend
 * one `push-register` token, carry no session, no proof of work and no device
 * pseudonym; the wake key is fetched with no token at all.
 */
class PushClientTokenTest {

    private data class Seen(val path: String, val authorization: String?, val token: String?, val body: String?)

    private fun client(seen: MutableList<Seen>, tokens: me.lakko.app.anon.AnonTokenSource): PushClient {
        val engine = MockEngine { request ->
            val body = when (val b = request.body) {
                is TextContent -> b.text
                is OutgoingContent.ByteArrayContent -> b.bytes().toString(Charsets.UTF_8)
                else -> null
            }
            seen += Seen(
                path = request.url.encodedPath,
                authorization = request.headers[HttpHeaders.Authorization],
                token = request.headers["x-lakko-anon-token"],
                body = body,
            )
            val reply = when {
                request.url.encodedPath.endsWith("/subscribe") -> """{"ok":true}"""
                request.url.encodedPath.endsWith("/wake-key") -> """{"key":"${"A".repeat(43)}"}"""
                else -> """{"ok":true,"removed":true}"""
            }
            respond(reply, HttpStatusCode.OK, headersOf(HttpHeaders.ContentType, "application/json"))
        }
        return PushClient(baseUrl = "http://test.invalid", engine = engine, anonTokens = tokens)
    }

    @Test
    fun theTwoRoutesSpendAPushRegisterTokenWithoutSessionOrPow() = runTest {
        val seen = mutableListOf<Seen>()
        val tokens = FakeAnonTokenSource()
        val subId = ByteArray(16) { 1 }
        val group = ByteArray(32) { 2 }
        client(seen, tokens).use { c ->
            c.subscribe(subId, group, group, byteArrayOf(9))
            assertTrue(c.unsubscribe(subId))
        }
        assertEquals(listOf("/push/subscribe", "/push/unsubscribe"), seen.map { it.path })
        assertEquals(List(2) { AnonTokenContext.PUSH_REGISTER }, tokens.taken)
        for (s in seen) {
            assertNull(s.authorization)
            assertTrue(s.token != null)
            assertFalse(s.body!!.contains("\"pow\""))
            assertFalse(s.body.contains("device_pseudonym"))
        }
        assertFalse(seen.any { it.path.endsWith("/pow/challenge") })
    }

    @Test
    fun theWakeKeyIsFetchedWithoutAToken() = runTest {
        val seen = mutableListOf<Seen>()
        val tokens = FakeAnonTokenSource()
        val key = client(seen, tokens).use { it.wakeKey() }
        assertEquals(32, key.size)
        assertEquals("/push/wake-key", seen.single().path)
        assertNull(seen.single().token)
        assertTrue(tokens.taken.isEmpty())
    }

    @Test
    fun theLogoutUnsubscribeDrawsItsTokenWithTheDepartingSession() = runTest {
        val seen = mutableListOf<Seen>()
        val base = FakeAnonTokenSource()
        val departing = AnonSession("acct", "bearer")
        client(seen, DetachedAnonTokenSource(base, departing)).use { c ->
            c.unsubscribe(ByteArray(16))
        }
        assertEquals(listOf(departing to AnonTokenContext.PUSH_REGISTER), base.detached)
        assertTrue(base.taken.isEmpty())
        assertNull(seen.single().authorization)
    }
}
