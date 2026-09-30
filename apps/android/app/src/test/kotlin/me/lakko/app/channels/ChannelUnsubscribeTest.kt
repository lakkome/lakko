// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpMethod
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Test

/**
 * ADR-0060 section 3 — a subscriber leaves a channel with a DELETE signed by
 * its subscriber key. The signature itself is the core's; a fake stands in on
 * the host JVM and the test checks what it was asked to sign and where the
 * answer goes.
 */
class ChannelUnsubscribeTest {

    private class Seen(val method: HttpMethod, val url: String, val header: String?)

    private fun client(status: HttpStatusCode, seen: MutableList<Seen>): ChannelsClient =
        ChannelsClient(
            baseUrl = "http://test.invalid",
            engine = MockEngine { request ->
                seen += Seen(request.method, request.url.toString(), request.headers[HEADER])
                respond(
                    content = if (status.value == 401) """{"code":"bad-unsubscribe-signature","detail":"x"}""" else "",
                    status = status,
                    headers = headersOf("Content-Type", "application/json"),
                )
            },
        )

    private val channelHex = "0123456789abcdef0123456789abcdef"
    private val keys = ChannelSubscriberKeys(secret = ByteArray(32) { 5 }, publicKey = ByteArray(32) { 0x0a })

    @Test
    fun `the DELETE names the channel and the subscriber key and carries the signature`() = runTest {
        val seen = mutableListOf<Seen>()
        var signedSecret: ByteArray? = null
        var signedChannel: ByteArray? = null
        var signedAt: ULong? = null
        client(HttpStatusCode.NoContent, seen).use { c ->
            ChannelUnsubscribe.tellServer(
                client = c,
                channelIdHex = channelHex,
                keys = keys,
                nowMs = 1_700_000_000_000,
                headerName = HEADER,
                sign = { secret, channel, at ->
                    signedSecret = secret
                    signedChannel = channel
                    signedAt = at
                    "1700000000000.SIG"
                },
            )
        }
        val request = seen.single()
        assertEquals(HttpMethod.Delete, request.method)
        assertEquals(
            "http://test.invalid/v1/channels/$channelHex/subscribers/${"0a".repeat(32)}",
            request.url,
        )
        assertEquals("1700000000000.SIG", request.header)
        assertArrayEquals(keys.secret, signedSecret)
        assertArrayEquals(ChannelsComposer.channelIdFromHex(channelHex), signedChannel)
        assertEquals(1_700_000_000_000uL, signedAt)
    }

    @Test
    fun `a 404 counts as done`() = runTest {
        client(HttpStatusCode.NotFound, mutableListOf()).use { c ->
            c.unsubscribe(channelHex, "0a".repeat(32), HEADER, "v")
        }
    }

    @Test
    fun `a refused signature throws`() = runTest {
        client(HttpStatusCode.Unauthorized, mutableListOf()).use { c ->
            val error = try {
                c.unsubscribe(channelHex, "0a".repeat(32), HEADER, "v")
                null
            } catch (e: ChannelsApiException) {
                e
            }
            assertNotNull(error)
            assertEquals(401, error!!.status)
            assertEquals("bad-unsubscribe-signature", error.code)
        }
    }

    private companion object {
        const val HEADER = "x-lakko-unsubscribe-sig"
    }
}
