// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpMethod
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0054 decision 1 with ADR-0078 section 3 — the `x-lakko-wake` header on
 * the signed group publish is a wake sealed to push-gw's key. The DS answers
 * `400` to a malformed value, which would fail the SEND, so [DsWake] admits
 * only the sealed shape, a sealing failure fails toward silence, and the
 * publish path carries it (and only it) where asked. The sealing itself runs
 * in the core (host JVM tests load no native library); its vectors are pinned
 * in `crates/lakko-contacts`.
 */
class DsWakeTest {

    /** A well-formed header value (312 base64url characters). */
    private val sealedA = "A".repeat(DsWake.HEADER_VALUE_LEN)
    private val gwKey = ByteArray(32) { 9 }
    private val secret = ByteArray(32) { 4 }

    @Test
    fun `only the sealed shape is a header`() {
        assertEquals(sealedA, DsWake(sealedA).headerValue())
        assertTrue(DsWake.isWellFormed("-_09azAZ".repeat(39)))
        assertThrows(IllegalArgumentException::class.java) { DsWake("A".repeat(311)) }
        assertThrows(IllegalArgumentException::class.java) { DsWake("A".repeat(313)) }
        assertThrows(IllegalArgumentException::class.java) { DsWake("A".repeat(311) + "=") }
        assertThrows(IllegalArgumentException::class.java) {
            DsWake("A".repeat(43) + ";message") // the pre-ADR-0078 clear header
        }
    }

    @Test
    fun `the sealer is handed the key, the secret and the kind`() {
        val seen = mutableListOf<DsWakeKind>()
        val sealer = PushWakeSealer { key, sec, kind, ts ->
            assertTrue(key.contentEquals(gwKey))
            assertTrue(sec.contentEquals(secret))
            assertEquals(1234L, ts)
            seen += kind
            sealedA
        }
        assertEquals(sealedA, DsWake.seal(gwKey, secret, DsWakeKind.CALL, 1234L, sealer)?.headerValue())
        DsWake.seal(gwKey, secret, DsWakeKind.MESSAGE, 1234L, sealer)
        assertEquals(listOf(DsWakeKind.CALL, DsWakeKind.MESSAGE), seen)
    }

    @Test
    fun `a sealing failure or a malformed result wakes nobody`() {
        assertNull(DsWake.seal(gwKey, secret, DsWakeKind.MESSAGE, 1L) { _, _, _, _ -> throw IllegalStateException("x") })
        assertNull(DsWake.seal(gwKey, secret, DsWakeKind.MESSAGE, 1L) { _, _, _, _ -> "short" })
    }

    // ------------------------------------------------------------ publish

    private data class Seen(val method: HttpMethod, val path: String, val wake: String?)

    private val groupId = ByteArray(32) { (it + 1).toByte() }

    private fun client(
        replies: List<Pair<HttpStatusCode, String>>,
        seen: MutableList<Seen>,
        registry: DsSignerRegistry = DsSignerRegistry(),
    ): DsClient {
        var index = 0
        val engine = MockEngine { request ->
            seen += Seen(request.method, request.url.encodedPath, request.headers[DS_WAKE_HEADER])
            val (status, body) = replies[index++]
            respond(body, status, headersOf(HttpHeaders.ContentType, "application/json"))
        }
        return DsClient(
            baseUrl = "https://test.invalid",
            engine = engine,
            signers = registry,
            // ADR-0078: a proofless mailbox write spends a `mailbox-write` token.
            anonTokens = me.lakko.app.anon.FakeAnonTokenSource(),
        )
    }

    private class Signer : DsJournalSigner {
        override val epoch: ULong = 1uL
        override fun sign(method: String, path: String, query: String, body: ByteArray): String = "proof"
        override val verifyingKey: ByteArray = ByteArray(32)
        override fun mailboxClaimBody(): String = """{"mailbox_pk_b64":"k"}"""
    }

    private val published = """{"id":7,"created_at":"t"}"""

    @Test
    fun `a publish asked to wake carries the header`() = runTest {
        val seen = mutableListOf<Seen>()
        client(listOf(HttpStatusCode.OK to published), seen).use {
            it.publishGroup(groupId, byteArrayOf(1), wake = DsWake(sealedA))
        }
        assertEquals(sealedA, seen.single().wake)
    }

    @Test
    fun `a publish with no wake sends no header`() = runTest {
        val seen = mutableListOf<Seen>()
        client(listOf(HttpStatusCode.OK to published), seen).use {
            it.publishGroup(groupId, byteArrayOf(1))
        }
        assertNull(seen.single().wake)
    }

    @Test
    fun `the header rides the retry after the owner's claim`() = runTest {
        // ADR-0057 — the claim is the owner's root statement, run by the
        // journal source's hook (not a request this client builds); the
        // publish retried after it carries the wake again.
        val seen = mutableListOf<Seen>()
        val registry = DsSignerRegistry()
        var claims = 0
        registry.install(
            dsGroupKey(groupId),
            object : DsSignerSource {
                override fun epoch(): ULong = 1uL
                override fun derive(): DsJournalSigner = Signer()
                override val ownerClaim: (suspend () -> Boolean) = {
                    claims += 1
                    true
                }
            },
        )
        val replies = listOf(
            HttpStatusCode.Unauthorized to "",
            HttpStatusCode.OK to published,
        )
        client(replies, seen, registry).use {
            it.publishGroup(groupId, byteArrayOf(1), wake = DsWake(sealedA))
        }
        assertEquals(1, claims)
        assertEquals(2, seen.size)
        assertEquals(HttpMethod.Post, seen[0].method)
        assertEquals(sealedA, seen[0].wake)
        assertEquals(HttpMethod.Post, seen[1].method)
        assertEquals(sealedA, seen[1].wake)
    }

    @Test
    fun `a mailbox publish never carries a wake`() = runTest {
        val seen = mutableListOf<Seen>()
        client(listOf(HttpStatusCode.OK to published), seen).use {
            it.publishMailbox(ByteArray(32), byteArrayOf(1))
        }
        assertNull(seen.single().wake)
    }
}
