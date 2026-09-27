// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpMethod
import io.ktor.http.HttpStatusCode
import io.ktor.http.content.OutgoingContent
import io.ktor.http.headersOf
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import kotlinx.coroutines.test.runTest
import me.lakko.app.auth.PowProofPayload
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0053 — what [DsClient] signs, with what, and what it does on a refusal.
 *
 * The signer is a fake that records its inputs, so every test can assert the
 * exact `(method, path, query, body)` the proof covers against what went on
 * the wire. MockEngine-backed; no native library.
 */
class DsClientProofTest {

    private class FakeSigner(override val epoch: ULong, val tag: String) : DsJournalSigner {
        data class Call(val method: String, val path: String, val query: String, val body: ByteArray)

        val calls = mutableListOf<Call>()

        override fun sign(method: String, path: String, query: String, body: ByteArray): String {
            calls += Call(method, path, query, body)
            return "proof-$tag-${calls.size}"
        }

        override fun groupClaimBody(): String = """{"epoch":$epoch,"access_pk_b64":"$tag"}"""

        override fun mailboxClaimBody(): String = """{"identity_pk_b64":"$tag"}"""
    }

    private data class Seen(
        val method: HttpMethod,
        val encodedPath: String,
        val encodedQuery: String,
        val proof: String?,
        val body: ByteArray?,
        val contentType: String?,
    )

    private data class Reply(
        val status: HttpStatusCode,
        val body: String = "",
        val date: String? = null,
    )

    private val groupId = ByteArray(32) { (it + 1).toByte() }
    private val groupHex = DsClient.pseudonymHex(groupId)
    private val mailbox = ByteArray(32) { (0xF0 + it % 16).toByte() }

    private fun client(
        replies: List<Reply>,
        seen: MutableList<Seen>,
        registry: DsSignerRegistry = DsSignerRegistry(),
        nowMs: Long = 1_800_000_000_000L,
    ): DsClient {
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
                proof = request.headers[DS_PROOF_HEADER],
                body = body,
                contentType = request.body.contentType?.toString(),
            )
            val reply = replies[index++]
            val headers = if (reply.date != null) {
                headersOf(HttpHeaders.ContentType to listOf("application/json"), HttpHeaders.Date to listOf(reply.date))
            } else {
                headersOf(HttpHeaders.ContentType, "application/json")
            }
            respond(content = reply.body, status = reply.status, headers = headers)
        }
        // A base URL WITH an nginx-style prefix: the proof must never cover it.
        return DsClient(baseUrl = "https://test.invalid/native/ds", engine = engine, signers = registry, nowMs = { nowMs })
    }

    private val emptyFetch = """{"messages":[],"has_more":false}"""
    private val published = """{"id":7,"created_at":"t"}"""

    // ------------------------------------------------------------ signing

    @Test
    fun `a group read signs the service path and the exact query it sends`() = runTest {
        val seen = mutableListOf<Seen>()
        val signer = FakeSigner(3uL, "n")
        client(listOf(Reply(HttpStatusCode.OK, emptyFetch)), seen).use {
            it.fetchGroup(groupId, after = 5, limit = 10, waitSecs = 0, signer = signer)
        }
        val call = signer.calls.single()
        assertEquals("GET", call.method)
        assertEquals("/ds/group/$groupHex/messages", call.path)
        assertEquals("after=5&limit=10&wait=0", call.query)
        assertEquals(0, call.body.size)
        // What was signed is what was sent, byte for byte.
        assertEquals("/native/ds/ds/group/$groupHex/messages", seen.single().encodedPath)
        assertEquals(call.query, seen.single().encodedQuery)
        assertEquals("proof-n-1", seen.single().proof)
    }

    @Test
    fun `a read with no cursor signs an empty query`() = runTest {
        val seen = mutableListOf<Seen>()
        val signer = FakeSigner(1uL, "n")
        client(listOf(Reply(HttpStatusCode.OK, emptyFetch)), seen).use {
            it.fetchGroup(groupId, signer = signer)
        }
        assertEquals("", signer.calls.single().query)
        assertEquals("", seen.single().encodedQuery)
    }

    @Test
    fun `a group publish signs the exact body bytes`() = runTest {
        val seen = mutableListOf<Seen>()
        val signer = FakeSigner(2uL, "n")
        val payload = byteArrayOf(9, 8, 7, 6)
        client(listOf(Reply(HttpStatusCode.OK, published)), seen).use {
            it.publishGroup(groupId, payload, signer = signer)
        }
        val call = signer.calls.single()
        assertEquals("POST", call.method)
        assertEquals("", call.query)
        assertArrayEquals(payload, call.body)
        assertArrayEquals(payload, seen.single().body)
        assertEquals("proof-n-1", seen.single().proof)
    }

    @Test
    fun `the registry signs when the call site passes no signer`() = runTest {
        val seen = mutableListOf<Seen>()
        val registry = DsSignerRegistry()
        val signer = FakeSigner(4uL, "r")
        registry.installStatic(dsGroupKey(groupId), signer)
        client(listOf(Reply(HttpStatusCode.OK, emptyFetch)), seen, registry).use {
            it.fetchGroup(groupId, after = 1)
        }
        assertEquals("proof-r-1", seen.single().proof)
    }

    @Test
    fun `an unknown journal goes out unsigned`() = runTest {
        val seen = mutableListOf<Seen>()
        client(listOf(Reply(HttpStatusCode.OK, emptyFetch)), seen).use {
            it.fetchGroup(groupId, after = 1)
        }
        assertNull(seen.single().proof)
    }

    @Test
    fun `a mailbox read is signed and a mailbox write never is`() = runTest {
        val seen = mutableListOf<Seen>()
        val registry = DsSignerRegistry()
        val signer = FakeSigner(0uL, "m")
        registry.installStatic(dsMailboxKey(mailbox), signer)
        client(
            listOf(Reply(HttpStatusCode.OK, emptyFetch), Reply(HttpStatusCode.OK, published)),
            seen,
            registry,
        ).use {
            it.fetchMailbox(mailbox, after = 2, waitSecs = 25)
            it.publishMailbox(mailbox, byteArrayOf(1))
        }
        assertEquals("/ds/mailbox/${DsClient.pseudonymHex(mailbox)}/messages", signer.calls.single().path)
        assertEquals("after=2&wait=25", signer.calls.single().query)
        assertEquals("proof-m-1", seen[0].proof)
        assertNull("a mailbox write stays open and unsigned", seen[1].proof)
    }

    // ------------------------------------------------------------ retry on 401

    @Test
    fun `a refused read is retried once with the previous epoch's signer`() = runTest {
        val seen = mutableListOf<Seen>()
        val registry = DsSignerRegistry()
        var epoch = 4uL
        val signers = mapOf(4uL to FakeSigner(4uL, "e4"), 5uL to FakeSigner(5uL, "e5"))
        registry.install(
            dsGroupKey(groupId),
            object : DsSignerSource {
                override fun epoch(): ULong = epoch
                override fun derive(): DsJournalSigner = signers.getValue(epoch)
            },
        )
        registry.current(dsGroupKey(groupId)) // caches epoch 4
        epoch = 5uL // the group applied a commit the server has not registered yet
        client(
            listOf(Reply(HttpStatusCode.Unauthorized, "proof required"), Reply(HttpStatusCode.OK, emptyFetch)),
            seen,
            registry,
        ).use { it.fetchGroup(groupId, after = 0) }

        assertEquals(listOf("proof-e5-1", "proof-e4-1"), seen.map { it.proof })
    }

    @Test
    fun `a second refusal claims the journal and retries with the current signer`() = runTest {
        val seen = mutableListOf<Seen>()
        val signer = FakeSigner(6uL, "cur")
        client(
            listOf(
                Reply(HttpStatusCode.Unauthorized),
                Reply(HttpStatusCode.NoContent),
                Reply(HttpStatusCode.OK, emptyFetch),
            ),
            seen,
        ).use { it.fetchGroup(groupId, signer = signer) }

        assertEquals(listOf(HttpMethod.Get, HttpMethod.Put, HttpMethod.Get), seen.map { it.method })
        assertEquals("/native/ds/ds/group/$groupHex/access", seen[1].encodedPath)
        assertEquals(signer.groupClaimBody(), String(seen[1].body!!, Charsets.UTF_8))
        assertNull("no previous signer held: the claim goes unsigned", seen[1].proof)
        assertTrue(seen[1].contentType!!.startsWith("application/json"))
    }

    @Test
    fun `a background read never claims on a refusal`() = runTest {
        val seen = mutableListOf<Seen>()
        val signer = FakeSigner(6uL, "bg")
        val error = assertThrows(DsProofRefusedError::class.java) {
            kotlinx.coroutines.runBlocking {
                client(listOf(Reply(HttpStatusCode.Unauthorized)), seen).use {
                    it.fetchGroup(groupId, signer = signer, reclaimOnRefusal = false)
                }
            }
        }
        assertEquals(1, seen.size)
        assertEquals(401, error.status)
    }

    @Test
    fun `the final refusal blames the clock when the server date is far off`() = runTest {
        val now = 1_800_000_000_000L
        val serverDate = DateTimeFormatter.RFC_1123_DATE_TIME
            .format(Instant.ofEpochMilli(now + 5 * 60_000L).atZone(ZoneOffset.UTC))
        val seen = mutableListOf<Seen>()
        val error = assertThrows(DsProofRefusedError::class.java) {
            kotlinx.coroutines.runBlocking {
                client(
                    listOf(
                        Reply(HttpStatusCode.Unauthorized, date = serverDate),
                        Reply(HttpStatusCode.NoContent),
                        Reply(HttpStatusCode.Unauthorized, date = serverDate),
                    ),
                    seen,
                    nowMs = now,
                ).use { it.publishGroup(groupId, byteArrayOf(1), signer = FakeSigner(1uL, "x")) }
            }
        }
        assertTrue(error.clockSkewed)
        assertEquals(DS_CLOCK_SKEW_COPY, error.userMessage)
        assertEquals(DS_CLOCK_SKEW_COPY, error.message)
    }

    @Test
    fun `the final refusal with a sane clock is the ordinary delivery error`() = runTest {
        val now = 1_800_000_000_000L
        val serverDate = DateTimeFormatter.RFC_1123_DATE_TIME
            .format(Instant.ofEpochMilli(now + 10_000L).atZone(ZoneOffset.UTC))
        val seen = mutableListOf<Seen>()
        val error = assertThrows(DsProofRefusedError::class.java) {
            kotlinx.coroutines.runBlocking {
                client(
                    listOf(
                        Reply(HttpStatusCode.Unauthorized, date = serverDate),
                        Reply(HttpStatusCode.NoContent),
                        Reply(HttpStatusCode.Unauthorized, date = serverDate),
                    ),
                    seen,
                    nowMs = now,
                ).use { it.fetchGroup(groupId, signer = FakeSigner(1uL, "x")) }
            }
        }
        assertFalse(error.clockSkewed)
        assertNull(error.userMessage)
    }

    // ------------------------------------------------------------ claims

    @Test
    fun `registering the next epoch is proven under the current one`() = runTest {
        val seen = mutableListOf<Seen>()
        val current = FakeSigner(8uL, "n")
        val next = FakeSigner(9uL, "n1")
        client(listOf(Reply(HttpStatusCode.NoContent)), seen).use {
            it.registerNextEpoch(groupId, next, current)
        }
        val put = seen.single()
        assertEquals(HttpMethod.Put, put.method)
        assertEquals(next.groupClaimBody(), String(put.body!!, Charsets.UTF_8))
        assertEquals("proof-n-1", put.proof)
        val call = current.calls.single()
        assertEquals("PUT", call.method)
        assertEquals("/ds/group/$groupHex/access", call.path)
        assertArrayEquals(put.body, call.body)
        assertTrue("the next signer proves nothing", next.calls.isEmpty())
    }

    @Test
    fun `a refused registration surfaces after one re-claim`() = runTest {
        val seen = mutableListOf<Seen>()
        val error = assertThrows(DsHttpError::class.java) {
            kotlinx.coroutines.runBlocking {
                client(
                    listOf(
                        Reply(HttpStatusCode.Unauthorized),
                        Reply(HttpStatusCode.Unauthorized),
                        Reply(HttpStatusCode.Unauthorized),
                    ),
                    seen,
                ).use { it.registerNextEpoch(groupId, FakeSigner(2uL, "b"), FakeSigner(1uL, "a")) }
            }
        }
        assertEquals(401, error.status)
        assertEquals(3, seen.size)
    }

    @Test
    fun `a mailbox claim sends the identity key unsigned`() = runTest {
        val seen = mutableListOf<Seen>()
        val signer = FakeSigner(0uL, "id")
        client(listOf(Reply(HttpStatusCode.NoContent)), seen).use { it.claimMailbox(mailbox, signer) }
        assertEquals("/native/ds/ds/mailbox/${DsClient.pseudonymHex(mailbox)}/access", seen.single().encodedPath)
        assertEquals(signer.mailboxClaimBody(), String(seen.single().body!!, Charsets.UTF_8))
        assertNull(seen.single().proof)
        assertTrue(signer.calls.isEmpty())
    }

    // ------------------------------------------------------------ knock

    @Test
    fun `a knock carries the solved pow and standard base64 payloads, unsigned`() = runTest {
        val seen = mutableListOf<Seen>()
        val result = client(
            listOf(
                Reply(HttpStatusCode.OK, """{"seed":"c2VlZA==","effort_bits":8,"expires_in":60}"""),
                Reply(HttpStatusCode.Created, """{"ids":[11,12],"epoch":42}"""),
            ),
            seen,
        ).use { client ->
            client.knockWithPow(groupId, listOf(byteArrayOf(1, 2), byteArrayOf(3))) { seed, bits ->
                assertEquals("c2VlZA==", seed)
                assertEquals(8, bits)
                PowProofPayload(seed = seed, nonce = "bm9uY2U=", solution = "c29s")
            }
        }
        assertEquals(listOf(11L, 12L), result.ids)
        assertEquals(42uL, result.epoch)
        assertEquals("/native/ds/pow/challenge", seen[0].encodedPath)
        assertEquals("/native/ds/ds/group/$groupHex/knock", seen[1].encodedPath)
        val json = String(seen[1].body!!, Charsets.UTF_8)
        assertTrue(json, json.contains("\"payloads_b64\":[\"AQI=\",\"Aw==\"]"))
        assertTrue(json, json.contains("\"seed\":\"c2VlZA==\""))
        assertNull(seen[1].proof)
    }

    @Test
    fun `a probe on an unclaimed group reports no epoch`() = runTest {
        val seen = mutableListOf<Seen>()
        val result = client(
            listOf(Reply(HttpStatusCode.Created, """{"ids":[],"epoch":null}""")),
            seen,
        ).use { it.knock(groupId, emptyList(), PowProofPayload("a", "b", "c")) }
        assertTrue(result.ids.isEmpty())
        assertNull(result.epoch)
    }

    @Test
    fun `knocks are read from their own route, signed like a group read`() = runTest {
        val seen = mutableListOf<Seen>()
        val signer = FakeSigner(3uL, "k")
        client(listOf(Reply(HttpStatusCode.OK, emptyFetch)), seen).use {
            it.fetchKnocks(groupId, after = 4, waitSecs = 0, signer = signer)
        }
        assertEquals("/ds/group/$groupHex/knocks", signer.calls.single().path)
        assertEquals("after=4&wait=0", signer.calls.single().query)
        assertEquals("proof-k-1", seen.single().proof)
    }
}
