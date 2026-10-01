// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.content.TextContent
import io.ktor.http.headersOf
import java.util.Base64
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import me.lakko.app.anon.FakeAnonTokenSource
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * ADR-0060 section 5 — the two lookups search makes. The request carries the
 * LOOKUP KEY only, and the response is read in the shape
 * `services/discovery` sends: `results[{account_id_b64, public_record_b64}]`
 * for a name, `{account_id, public_record_b64}` for a username.
 */
class DiscoveryNameLookupTest {

    private val b64 = Base64.getEncoder()

    private fun client(response: String, bodies: MutableList<String>, status: HttpStatusCode = HttpStatusCode.OK) =
        DiscoveryClient(
            baseUrl = "http://test.invalid",
            engine = MockEngine { request ->
                bodies += (request.body as TextContent).text
                respond(
                    content = response,
                    status = status,
                    headers = headersOf(HttpHeaders.ContentType, "application/json"),
                )
            },
            inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS },
            anonTokens = FakeAnonTokenSource(),
        )

    @Test
    fun `a name lookup sends the lookup key and reads the results with their records`() = runTest {
        val account = ByteArray(16) { 4 }
        val record = byteArrayOf(1, 2, 3)
        val lookupKey = ByteArray(64) { 9 }
        val bodies = mutableListOf<String>()
        val response = """{"results":[{"account_id_b64":"${b64.encodeToString(account)}",""" +
            """"public_record_b64":"${b64.encodeToString(record)}"},""" +
            """{"account_id_b64":"${b64.encodeToString(ByteArray(3))}","public_record_b64":"AA=="}]}"""
        val hits = client(response, bodies).use { it.nameLookup(lookupKey) }
        assertEquals(1, hits.size)
        assertArrayEquals(account, hits.single().accountId)
        assertArrayEquals(record, hits.single().publicRecord)
        val sent = Json.parseToJsonElement(bodies.single()).jsonObject
        assertEquals(b64.encodeToString(lookupKey), sent["oprf_hash_b64"]!!.jsonPrimitive.content)
    }

    @Test
    fun `a username lookup by key returns the account and its record`() = runTest {
        val account = ByteArray(16) { 5 }
        val lookupKey = ByteArray(64) { 8 }
        val bodies = mutableListOf<String>()
        val response = """{"account_id":"${b64.encodeToString(account)}","public_record_b64":"AQI="}"""
        val hit = client(response, bodies).use { it.lookupPublic(lookupKey) }!!
        assertArrayEquals(account, hit.accountId)
        assertArrayEquals(byteArrayOf(1, 2), hit.publicRecord)
        val sent = Json.parseToJsonElement(bodies.single()).jsonObject
        assertEquals(b64.encodeToString(lookupKey), sent["oprf_hash"]!!.jsonPrimitive.content)
    }

    @Test
    fun `an unregistered or hidden username is null`() = runTest {
        val hit = client("", mutableListOf(), HttpStatusCode.NotFound).use { it.lookupPublic(ByteArray(64)) }
        assertNull(hit)
    }
}
