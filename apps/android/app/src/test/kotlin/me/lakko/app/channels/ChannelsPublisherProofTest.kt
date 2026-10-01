// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpMethod
import io.ktor.http.HttpStatusCode
import io.ktor.http.content.OutgoingContent
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import me.lakko.app.anon.FakeAnonTokenSource
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.fail
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0059 section 9 (G-43) — what [ChannelsClient] signs for the publisher
 * routes, under which role, and what it does on a refusal. The proofs are a
 * fake that records its inputs; MockEngine-backed, no native library.
 */
class ChannelsPublisherProofTest {

    private class FakeProofs(override val roleKey: String = "owner-key-hex") : ChannelPublisherProofs {
        data class Call(
            val role: ChannelPublisherRole,
            val method: String,
            val path: String,
            val query: String,
            val body: ByteArray,
        )

        val calls = mutableListOf<Call>()

        override fun proofHeader(
            role: ChannelPublisherRole,
            method: String,
            path: String,
            query: String,
            body: ByteArray,
        ): Pair<String, String> {
            calls += Call(role, method, path, query, body)
            val header = if (role == ChannelPublisherRole.OWNER) OWNER_HEADER else COPUB_HEADER
            return header to "proof-${calls.size}"
        }
    }

    private data class Seen(
        val method: HttpMethod,
        val encodedPath: String,
        val encodedQuery: String,
        val ownerProof: String?,
        val copubProof: String?,
        val body: ByteArray?,
        val contentType: String?,
    )

    private val channelId = "00112233445566778899aabbccddeeff"
    private val refused = """{"code":"owner-proof-required","detail":"refused"}"""

    private fun client(
        replies: List<Pair<HttpStatusCode, String>>,
        seen: MutableList<Seen>,
        roles: ChannelPublisherRoles,
    ): ChannelsClient {
        var index = 0
        val engine = MockEngine { request ->
            val body = when (val outgoing = request.body) {
                is OutgoingContent.ByteArrayContent -> outgoing.bytes()
                is OutgoingContent.NoContent -> null
                else -> error("unexpected body ${outgoing::class}")
            }
            seen += Seen(
                method = request.method,
                encodedPath = request.url.encodedPath,
                encodedQuery = request.url.encodedQuery,
                ownerProof = request.headers[OWNER_HEADER],
                copubProof = request.headers[COPUB_HEADER],
                body = body,
                contentType = request.body.contentType?.toString(),
            )
            val (status, text) = replies[index++]
            respond(text, status, headersOf(HttpHeaders.ContentType, "application/json"))
        }
        // A base URL WITH the prefix nginx strips: the proof must never cover it.
        return ChannelsClient(
            baseUrl = "https://test.invalid/native/channels",
            engine = engine,
            anonTokens = FakeAnonTokenSource(),
            publisherRoles = roles,
        )
    }

    @Test
    fun `a publish signs the service path and exactly the bytes it sends`() = runTest {
        val seen = mutableListOf<Seen>()
        val proofs = FakeProofs()
        val payload = byteArrayOf(1, 2, 3, 0, -1)
        client(
            listOf(HttpStatusCode.OK to """{"cursor":4,"server_ts_ms":5,"payload_b64":"AQ"}"""),
            seen,
            ChannelPublisherRoles(),
        ).use { it.publishPost(channelId, payload, proofs) }

        val call = proofs.calls.single()
        assertEquals("POST", call.method)
        assertEquals("/v1/channels/$channelId/posts", call.path)
        assertEquals("", call.query)
        assertArrayEquals(payload, call.body)
        val wire = seen.single()
        assertEquals("/native/channels/v1/channels/$channelId/posts", wire.encodedPath)
        assertArrayEquals(call.body, wire.body)
        assertEquals("proof-1", wire.ownerProof)
        assertEquals("application/octet-stream", wire.contentType)
    }

    @Test
    fun `a delivery signs the epoch query it sends`() = runTest {
        val seen = mutableListOf<Seen>()
        val proofs = FakeProofs()
        client(listOf(HttpStatusCode.NoContent to ""), seen, ChannelPublisherRoles()).use {
            it.deliverEnvelope(channelId, "ab".repeat(32), byteArrayOf(9, 9), proofs, epoch = 7)
        }
        val call = proofs.calls.single()
        assertEquals("/v1/channels/$channelId/subscribers/${"ab".repeat(32)}/envelope", call.path)
        assertEquals("epoch=7", call.query)
        assertEquals("epoch=7", seen.single().encodedQuery)
        assertArrayEquals(byteArrayOf(9, 9), seen.single().body)
    }

    @Test
    fun `a stage chunk signs the JSON bytes it sends`() = runTest {
        val seen = mutableListOf<Seen>()
        val proofs = FakeProofs()
        val entries = listOf(RotateBatchEntry("cd".repeat(32), "ZW52"))
        client(listOf(HttpStatusCode.NoContent to ""), seen, ChannelPublisherRoles()).use {
            it.stageRotation(channelId, 3u, entries, proofs)
        }
        val call = proofs.calls.single()
        assertEquals("/v1/channels/$channelId/rotate/stage", call.path)
        assertEquals(
            """{"target_epoch":3,"envelopes":[{"subscriber_pubkey_hex":"${"cd".repeat(32)}","envelope_b64":"ZW52"}]}""",
            call.body.toString(Charsets.UTF_8),
        )
        assertArrayEquals(call.body, seen.single().body)
        assertTrue(seen.single().contentType!!.startsWith("application/json"))
    }

    @Test
    fun `an unknown role tries the owner first, then the co-publisher, and remembers it`() = runTest {
        val seen = mutableListOf<Seen>()
        val roles = ChannelPublisherRoles()
        val proofs = FakeProofs()
        client(
            listOf(
                HttpStatusCode.Unauthorized to refused,
                HttpStatusCode.OK to """{"epoch":4}""",
                HttpStatusCode.OK to """{"epoch":5}""",
            ),
            seen,
            roles,
        ).use {
            assertEquals(4u, it.commitRotation(channelId, 4u, proofs))
            assertEquals(ChannelPublisherRole.COPUBLISHER, roles["owner-key-hex"])
            // Known now: one request, under the co-publisher key only.
            assertEquals(5u, it.commitRotation(channelId, 5u, proofs))
        }
        assertEquals(
            listOf(ChannelPublisherRole.OWNER, ChannelPublisherRole.COPUBLISHER, ChannelPublisherRole.COPUBLISHER),
            proofs.calls.map { it.role },
        )
        assertEquals(3, seen.size)
        assertNull(seen[2].ownerProof)
        // Every attempt is a fresh proof.
        assertEquals(listOf("proof-2", "proof-3"), seen.drop(1).map { it.copubProof })
    }

    @Test
    fun `a known role is signed once and a refusal forgets it`() = runTest {
        val seen = mutableListOf<Seen>()
        val roles = ChannelPublisherRoles().apply { remember("owner-key-hex", ChannelPublisherRole.OWNER) }
        val proofs = FakeProofs()
        client(listOf(HttpStatusCode.Unauthorized to refused), seen, roles).use {
            val error = expectApiError { it.publishPost(channelId, byteArrayOf(1), proofs) }
            assertEquals(PUBLISHER_REFUSED, error.code)
            assertEquals(PUBLISHER_REFUSED_COPY, channelPublishErrorText(error, "fallback"))
        }
        assertEquals(1, seen.size)
        assertNull(roles["owner-key-hex"])
    }

    @Test
    fun `a non-401 refusal does not try the co-publisher key`() = runTest {
        val seen = mutableListOf<Seen>()
        val roles = ChannelPublisherRoles()
        client(
            listOf(HttpStatusCode.NotFound to """{"code":"unknown-channel","detail":"x"}"""),
            seen,
            roles,
        ).use {
            val error = expectApiError { it.listSubscribersAsPublisher(channelId, FakeProofs()) }
            assertEquals("unknown-channel", error.code)
            assertEquals("fallback", channelPublishErrorText(error, "fallback"))
        }
        assertEquals(1, seen.size)
        assertNull(roles["owner-key-hex"])
    }

    @Test
    fun `the subscriber list uses the cached role`() = runTest {
        val seen = mutableListOf<Seen>()
        val roles = ChannelPublisherRoles().apply { remember("owner-key-hex", ChannelPublisherRole.COPUBLISHER) }
        val proofs = FakeProofs()
        val list = client(listOf(HttpStatusCode.OK to """{"subscribers":["aa"]}"""), seen, roles).use {
            it.listSubscribersAsPublisher(channelId, proofs)
        }
        assertEquals(listOf("aa"), list)
        val call = proofs.calls.single()
        assertEquals(ChannelPublisherRole.COPUBLISHER, call.role)
        assertEquals("GET", call.method)
        assertEquals("/v1/channels/$channelId/subscribers", call.path)
        assertEquals(0, call.body.size)
        assertNull(seen.single().body)
    }

    private suspend fun expectApiError(block: suspend () -> Unit): ChannelsApiException {
        try {
            block()
        } catch (e: ChannelsApiException) {
            return e
        }
        fail("expected a ChannelsApiException")
        throw AssertionError()
    }

    private companion object {
        const val OWNER_HEADER = "x-test-owner-proof"
        const val COPUB_HEADER = "x-test-copublisher-proof"
    }
}
