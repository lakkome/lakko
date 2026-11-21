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
import me.lakko.app.anon.AnonTokenContext
import me.lakko.app.anon.FakeAnonTokenSource
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

        override val verifyingKey: ByteArray = ByteArray(32) { epoch.toByte() }

        override fun mailboxClaimBody(): String = """{"mailbox_pk_b64":"$tag"}"""
    }

    private data class Seen(
        val method: HttpMethod,
        val encodedPath: String,
        val encodedQuery: String,
        val proof: String?,
        val body: ByteArray?,
        val contentType: String?,
        val token: String? = null,
        val authorization: String? = null,
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
        tokens: FakeAnonTokenSource = FakeAnonTokenSource(),
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
                token = request.headers["x-lakko-anon-token"],
                authorization = request.headers[HttpHeaders.Authorization],
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
        return DsClient(
            baseUrl = "https://test.invalid/native/ds",
            engine = engine,
            signers = registry,
            nowMs = { nowMs },
            anonTokens = tokens,
        )
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
        assertNull("a proofless mailbox write carries no proof", seen[1].proof)
        assertEquals("tok-1", seen[1].token)
    }

    // ------------------------------------------------------------ ADR-0078 mailbox-write token

    @Test
    fun `a proofless mailbox write spends one mailbox-write token, with no session`() = runTest {
        val seen = mutableListOf<Seen>()
        val tokens = FakeAnonTokenSource()
        val row = ByteArray(1073) { 5 }
        val result = client(listOf(Reply(HttpStatusCode.OK, published)), seen, tokens = tokens).use {
            it.publishMailbox(mailbox, row)
        }
        assertEquals(7L, result.id)
        assertEquals(listOf(AnonTokenContext.MAILBOX_WRITE), tokens.taken)
        assertEquals("tok-1", seen.single().token)
        assertNull(seen.single().proof)
        assertNull(seen.single().authorization)
        assertEquals("/native/ds/ds/mailbox/${DsClient.pseudonymHex(mailbox)}/messages", seen.single().encodedPath)
        assertTrue(row.contentEquals(seen.single().body!!))
    }

    @Test
    fun `a mailbox write signed by a reach grant spends no token`() = runTest {
        val seen = mutableListOf<Seen>()
        val tokens = FakeAnonTokenSource()
        client(listOf(Reply(HttpStatusCode.OK, published)), seen, tokens = tokens).use {
            it.publishMailbox(mailbox, ByteArray(1073) { 5 }, reach = DsReachAuth(FakeSigner(0uL, "r")))
        }
        assertTrue(tokens.taken.isEmpty())
        assertNull(seen.single().token)
        assertEquals("proof-r-1", seen.single().proof)
    }

    @Test
    fun `a proofless mailbox write whose token was spent is retried once with another`() = runTest {
        val seen = mutableListOf<Seen>()
        client(
            listOf(
                Reply(HttpStatusCode.Unauthorized, """{"error":"anon-token-spent"}"""),
                Reply(HttpStatusCode.OK, published),
            ),
            seen,
        ).use { it.publishMailbox(mailbox, ByteArray(1073) { 5 }) }
        assertEquals(listOf("tok-1", "tok-2"), seen.map { it.token })
    }

    // ------------------------------------------------------------ rendezvous-write token (2026-11-21)

    @Test
    fun `a rendezvous write spends one rendezvous-write token, with no session and no proof`() = runTest {
        val seen = mutableListOf<Seen>()
        val tokens = FakeAnonTokenSource()
        val rendezvous = ByteArray(32) { (0x40 + it).toByte() }
        val result = client(listOf(Reply(HttpStatusCode.OK, published)), seen, tokens = tokens).use {
            it.publishRendezvous(rendezvous, byteArrayOf(9, 9, 9))
        }
        assertEquals(7L, result.id)
        assertEquals(listOf(AnonTokenContext.RENDEZVOUS_WRITE), tokens.taken)
        val request = seen.single()
        assertEquals(HttpMethod.Post, request.method)
        assertEquals("tok-1", request.token)
        assertNull(request.proof)
        assertNull(request.authorization)
        assertEquals("/native/ds/ds/rendezvous/${DsClient.pseudonymHex(rendezvous)}/messages", request.encodedPath)
        assertArrayEquals(byteArrayOf(9, 9, 9), request.body)
        assertTrue(request.contentType?.startsWith("application/octet-stream") == true)
    }

    @Test
    fun `a rendezvous write whose token was spent or expired is retried once with another`() = runTest {
        for (code in listOf("anon-token-spent", "anon-token-expired")) {
            val seen = mutableListOf<Seen>()
            val tokens = FakeAnonTokenSource()
            client(
                listOf(
                    Reply(HttpStatusCode.Unauthorized, """{"error":"$code"}"""),
                    Reply(HttpStatusCode.OK, published),
                ),
                seen,
                tokens = tokens,
            ).use { it.publishRendezvous(ByteArray(32) { 1 }, byteArrayOf(1)) }
            assertEquals(code, listOf("tok-1", "tok-2"), seen.map { it.token })
            assertEquals(code, listOf(AnonTokenContext.RENDEZVOUS_WRITE, AnonTokenContext.RENDEZVOUS_WRITE), tokens.taken)
        }
    }

    @Test
    fun `a rendezvous write with no token to be had fails as a network failure and sends nothing`() = runTest {
        val seen = mutableListOf<Seen>()
        val tokens = FakeAnonTokenSource(
            failWith = me.lakko.app.anon.AnonTokenUnavailable(AnonTokenContext.RENDEZVOUS_WRITE, "empty"),
        )
        val client = client(emptyList(), seen, tokens = tokens)
        val error = try {
            client.publishRendezvous(ByteArray(32) { 1 }, byteArrayOf(1))
            null
        } catch (e: DsNetworkError) {
            e
        } finally {
            client.close()
        }
        assertTrue(error != null)
        assertTrue(error!!.cause is me.lakko.app.anon.AnonTokenUnavailable)
        assertTrue(seen.isEmpty())
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
    fun `a plain member never claims on a refusal - the 401 surfaces`() = runTest {
        // ADR-0057 obligation 6 — no owner hook installed: no claim, no
        // self-registration, the refusal is the answer.
        val seen = mutableListOf<Seen>()
        val signer = FakeSigner(6uL, "cur")
        val error = assertThrows(DsProofRefusedError::class.java) {
            kotlinx.coroutines.runBlocking {
                client(
                    listOf(Reply(HttpStatusCode.Unauthorized), Reply(HttpStatusCode.NoContent)),
                    seen,
                ).use { it.fetchGroup(groupId, signer = signer) }
            }
        }
        assertEquals(401, error.status)
        assertEquals(listOf(HttpMethod.Get), seen.map { it.method })
    }

    @Test
    fun `the owner's lazy claim runs once on a refusal and the read is retried`() = runTest {
        // ADR-0057 obligation 2 — only a journal whose source carries the
        // owner hook claims on a 401.
        val seen = mutableListOf<Seen>()
        val registry = DsSignerRegistry()
        var claims = 0
        val signer = FakeSigner(6uL, "cur")
        registry.install(
            dsGroupKey(groupId),
            object : DsSignerSource {
                override fun epoch(): ULong = 6uL
                override fun derive(): DsJournalSigner = signer
                override val ownerClaim: (suspend () -> Boolean) = {
                    claims += 1
                    true
                }
            },
        )
        client(
            listOf(Reply(HttpStatusCode.Unauthorized), Reply(HttpStatusCode.OK, emptyFetch)),
            seen,
            registry,
        ).use { it.fetchGroup(groupId) }
        assertEquals(1, claims)
        assertEquals(listOf(HttpMethod.Get, HttpMethod.Get), seen.map { it.method })
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

    private fun listeningRegistry(epoch: ULong, heard: MutableList<ULong>): DsSignerRegistry {
        val registry = DsSignerRegistry()
        val signer = FakeSigner(epoch, "cur")
        registry.install(
            dsGroupKey(groupId),
            object : DsSignerSource {
                override fun epoch(): ULong = epoch
                override fun derive(): DsJournalSigner = signer
                override val onRefusedAtHead: ((ULong) -> Unit) = { heard += it }
            },
        )
        return registry
    }

    @Test
    fun `a final refusal at this device's epoch is reported for the repair check`() = runTest {
        // ADR-0057 second security pass, finding 1.
        val heard = mutableListOf<ULong>()
        val seen = mutableListOf<Seen>()
        assertThrows(DsProofRefusedError::class.java) {
            kotlinx.coroutines.runBlocking {
                client(listOf(Reply(HttpStatusCode.Unauthorized)), seen, listeningRegistry(7uL, heard)).use {
                    it.fetchGroup(groupId)
                }
            }
        }
        assertEquals(listOf(7uL), heard)
    }

    @Test
    fun `a refusal blamed on the device clock is not reported`() = runTest {
        val now = 1_800_000_000_000L
        val serverDate = DateTimeFormatter.RFC_1123_DATE_TIME
            .format(Instant.ofEpochMilli(now + 5 * 60_000L).atZone(ZoneOffset.UTC))
        val heard = mutableListOf<ULong>()
        assertThrows(DsProofRefusedError::class.java) {
            kotlinx.coroutines.runBlocking {
                client(
                    listOf(Reply(HttpStatusCode.Unauthorized, date = serverDate)),
                    mutableListOf(),
                    listeningRegistry(7uL, heard),
                    nowMs = now,
                ).use { it.fetchGroup(groupId) }
            }
        }
        assertTrue(heard.isEmpty())
    }

    @Test
    fun `a request that succeeds reports nothing`() = runTest {
        val heard = mutableListOf<ULong>()
        client(listOf(Reply(HttpStatusCode.OK, emptyFetch)), mutableListOf(), listeningRegistry(7uL, heard)).use {
            it.fetchGroup(groupId)
        }
        assertTrue(heard.isEmpty())
    }

    // ------------------------------------------------------------ claims

    @Test
    fun `a statement is PUT to its own path with its JSON body and no proof`() = runTest {
        val seen = mutableListOf<Seen>()
        client(listOf(Reply(HttpStatusCode.NoContent)), seen).use {
            it.putStatement { DsStatement("/ds/group/$groupHex/access", """{"statement_b64":"AA=="}""") }
        }
        val put = seen.single()
        assertEquals(HttpMethod.Put, put.method)
        assertEquals("/native/ds/ds/group/$groupHex/access", put.encodedPath)
        assertEquals("""{"statement_b64":"AA=="}""", String(put.body!!, Charsets.UTF_8))
        assertNull("a statement carries its own signature", put.proof)
        assertTrue(put.contentType!!.startsWith("application/json"))
    }

    @Test
    fun `a 409 is retried once with a freshly signed statement`() = runTest {
        val seen = mutableListOf<Seen>()
        var signed = 0
        val error = assertThrows(DsHttpError::class.java) {
            kotlinx.coroutines.runBlocking {
                client(
                    listOf(Reply(HttpStatusCode.Conflict), Reply(HttpStatusCode.Conflict), Reply(HttpStatusCode.NoContent)),
                    seen,
                ).use {
                    it.putStatement {
                        signed += 1
                        DsStatement("/ds/group/$groupHex/claim", """{"n":$signed}""")
                    }
                }
            }
        }
        assertEquals(409, error.status)
        assertEquals(2, signed)
        assertEquals(2, seen.size)
        assertEquals("""{"n":2}""", String(seen[1].body!!, Charsets.UTF_8))
    }

    @Test
    fun `a lock race 409 then success lands`() = runTest {
        val seen = mutableListOf<Seen>()
        client(listOf(Reply(HttpStatusCode.Conflict), Reply(HttpStatusCode.NoContent)), seen).use {
            it.putStatement { DsStatement("/ds/group/$groupHex/access", "{}") }
        }
        assertEquals(2, seen.size)
    }

    @Test
    fun `any other refusal surfaces at once with its status`() = runTest {
        for (status in listOf(HttpStatusCode.BadRequest, HttpStatusCode.Unauthorized, HttpStatusCode.Forbidden, HttpStatusCode.ServiceUnavailable)) {
            val seen = mutableListOf<Seen>()
            val error = assertThrows(DsHttpError::class.java) {
                kotlinx.coroutines.runBlocking {
                    client(listOf(Reply(status)), seen).use {
                        it.putStatement { DsStatement("/ds/group/$groupHex/push-binding", "{}") }
                    }
                }
            }
            assertEquals(status.value, error.status)
            assertEquals(1, seen.size)
        }
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

    // ------------------------------------- the owner's reach read (ADR-0060 §1)

    @Test
    fun `the owner reads its reach keys signed with the mailbox key`() = runTest {
        val seen = mutableListOf<Seen>()
        val signer = FakeSigner(0uL, "id")
        val pk = ByteArray(32) { 7 }
        val body = """{"keys":[{"reach_pk_b64":"${java.util.Base64.getEncoder().encodeToString(pk)}",""" +
            """"expires_at_ms":null}],"open":false}"""
        val list = client(listOf(Reply(HttpStatusCode.OK, body)), seen).use { it.fetchMailboxReach(mailbox, signer) }
        assertEquals(1, list.keys.size)
        assertArrayEquals(pk, list.keys.single().publicKey)
        assertNull(list.keys.single().expiresAtMs)
        assertFalse(list.open)
        val req = seen.single()
        assertEquals(HttpMethod.Get, req.method)
        assertEquals("/native/ds/ds/mailbox/${DsClient.pseudonymHex(mailbox)}/reach", req.encodedPath)
        assertEquals("proof-id-1", req.proof)
        assertEquals("/ds/mailbox/${DsClient.pseudonymHex(mailbox)}/reach", signer.calls.single().path)
    }

    @Test
    fun `an unclaimed mailbox is claimed and read once more, a second refusal throws`() = runTest {
        val seen = mutableListOf<Seen>()
        val signer = FakeSigner(0uL, "id")
        val list = client(
            listOf(
                Reply(HttpStatusCode.Unauthorized),
                Reply(HttpStatusCode.NoContent),
                Reply(HttpStatusCode.OK, """{"keys":[],"open":false}"""),
            ),
            seen,
        ).use { it.fetchMailboxReach(mailbox, signer) }
        assertTrue(list.keys.isEmpty())
        assertEquals(3, seen.size)
        assertTrue(seen[1].encodedPath.endsWith("/access"))

        // Refused again: the read fails, and the caller registers nothing.
        val again = mutableListOf<Seen>()
        assertThrows(DsHttpError::class.java) {
            kotlinx.coroutines.runBlocking {
                client(
                    listOf(
                        Reply(HttpStatusCode.Unauthorized),
                        Reply(HttpStatusCode.NoContent),
                        Reply(HttpStatusCode.Unauthorized),
                    ),
                    again,
                ).use { it.fetchMailboxReach(mailbox, signer) }
            }
        }
    }

    // ------------------------------------------------------------ knock

    @Test
    fun `a knock spends a ds-knock token, carries standard base64 payloads, unsigned, no session`() = runTest {
        val seen = mutableListOf<Seen>()
        val tokens = FakeAnonTokenSource()
        val result = client(
            listOf(Reply(HttpStatusCode.Created, """{"ids":[11,12],"epoch":42}""")),
            seen,
            tokens = tokens,
        ).use { client ->
            client.knock(groupId, listOf(byteArrayOf(1, 2), byteArrayOf(3)))
        }
        assertEquals(listOf(11L, 12L), result.ids)
        assertEquals(42uL, result.epoch)
        assertEquals(1, seen.size)
        assertEquals("/native/ds/ds/group/$groupHex/knock", seen[0].encodedPath)
        val json = String(seen[0].body!!, Charsets.UTF_8)
        assertTrue(json, json.contains("\"payloads_b64\":[\"AQI=\",\"Aw==\"]"))
        assertTrue(json, !json.contains("pow"))
        assertNull(seen[0].proof)
        assertNull(seen[0].authorization)
        assertEquals("tok-1", seen[0].token)
        assertEquals(listOf(AnonTokenContext.DS_KNOCK), tokens.taken)
    }

    @Test
    fun `a knock whose token was spent is retried once with another`() = runTest {
        val seen = mutableListOf<Seen>()
        val result = client(
            listOf(
                Reply(HttpStatusCode.Unauthorized, """{"error":"anon-token-spent"}"""),
                Reply(HttpStatusCode.Created, """{"ids":[],"epoch":3}"""),
            ),
            seen,
        ).use { it.knock(groupId, emptyList()) }
        assertEquals(3uL, result.epoch)
        assertEquals(listOf("tok-1", "tok-2"), seen.map { it.token })
    }

    @Test
    fun `a probe on an unclaimed group reports no epoch`() = runTest {
        val seen = mutableListOf<Seen>()
        val result = client(
            listOf(Reply(HttpStatusCode.Created, """{"ids":[],"epoch":null}""")),
            seen,
        ).use { it.knock(groupId, emptyList()) }
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
