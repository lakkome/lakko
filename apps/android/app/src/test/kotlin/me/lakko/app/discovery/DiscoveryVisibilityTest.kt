// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpMethod
import io.ktor.http.HttpStatusCode
import io.ktor.http.content.TextContent
import java.util.Base64
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import me.lakko.app.anon.FakeAnonTokenSource
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0060 section 5 — the `PUT /discovery/visibility` body. The server
 * requires `public_record_b64` in Everyone and refuses it (and any name term)
 * outside it, so the body must carry exactly that.
 */
class DiscoveryVisibilityTest {

    private class Captured(val method: HttpMethod, val url: String, val auth: String?, val body: JsonObject)

    private fun client(captured: MutableList<Captured>): DiscoveryClient {
        val engine = MockEngine { request ->
            val text = (request.body as TextContent).text
            captured += Captured(
                request.method,
                request.url.toString(),
                request.headers["Authorization"],
                Json.parseToJsonElement(text).jsonObject,
            )
            respond(content = "", status = HttpStatusCode.NoContent)
        }
        return DiscoveryClient(
            baseUrl = "http://test.invalid",
            engine = engine,
            inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS },
            anonTokens = FakeAnonTokenSource(),
        ).also { it.setSessionToken("TOKEN") }
    }

    @Test
    fun `entering Everyone sends the lookup keys and the public record`() = runTest {
        val captured = mutableListOf<Captured>()
        val key1 = ByteArray(64) { 1 }
        val key2 = ByteArray(64) { 2 }
        val record = byteArrayOf(10, 20, 30)
        client(captured).use { it.putVisibility(true, listOf(key1, key2), record) }

        val c = captured.single()
        assertEquals(HttpMethod.Put, c.method)
        assertEquals("http://test.invalid/discovery/visibility", c.url)
        assertEquals("Bearer TOKEN", c.auth)
        assertEquals(true, c.body["public"]!!.jsonPrimitive.content.toBoolean())
        assertEquals(
            listOf(Base64.getEncoder().encodeToString(key1), Base64.getEncoder().encodeToString(key2)),
            c.body["name_terms_b64"]!!.jsonArray.map { it.jsonPrimitive.content },
        )
        assertEquals(
            Base64.getEncoder().encodeToString(record),
            c.body["public_record_b64"]!!.jsonPrimitive.content,
        )
    }

    @Test
    fun `leaving Everyone omits the terms and the record`() = runTest {
        val captured = mutableListOf<Captured>()
        // Even if a caller passes them, outside Everyone they are not sent.
        client(captured).use { it.putVisibility(false, listOf(ByteArray(64)), byteArrayOf(1)) }

        val body = captured.single().body
        assertEquals(false, body["public"]!!.jsonPrimitive.content.toBoolean())
        assertFalse(body.containsKey("public_record_b64"))
        val terms = body["name_terms_b64"]
        assertTrue(terms == null || terms.jsonArray.isEmpty())
    }
}
