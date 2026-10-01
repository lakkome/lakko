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
 * ADR-0054 decision 1 — the `x-lakko-wake` header on the signed group
 * publish. The DS answers `400` to a malformed value, which would fail the
 * SEND, so the one builder is pinned here against fixed vectors and the
 * publish path is checked to carry it (and only it) where asked.
 */
class DsWakeTest {

    private val zeros = ByteArray(32)

    @Test
    fun `the header is 43 base64url characters without padding then the kind`() {
        assertEquals("A".repeat(43) + ";message", DsWake(zeros, DsWakeKind.MESSAGE).headerValue())
        assertEquals("A".repeat(43) + ";call", DsWake(zeros, DsWakeKind.CALL).headerValue())
        // The URL-safe alphabet: 0xFF.. ends in '_' ... '8', never '/' or '='.
        assertEquals("_".repeat(42) + "8;message", DsWake(ByteArray(32) { -1 }, DsWakeKind.MESSAGE).headerValue())
        val dashes = ByteArray(32).also {
            it[0] = 0xFB.toByte()
            it[1] = 0xEF.toByte()
            it[2] = 0xBE.toByte()
        }
        assertTrue(DsWake(dashes, DsWakeKind.CALL).headerValue().startsWith("----"))
    }

    @Test
    fun `every value matches the wire grammar`() {
        val grammar = Regex("^[A-Za-z0-9_-]{43};(message|call)$")
        for (seed in 0 until 64) {
            val gid = ByteArray(32) { (it * 37 + seed * 11).toByte() }
            for (kind in DsWakeKind.entries) {
                val value = DsWake(gid, kind).headerValue()
                assertTrue(value, grammar.matches(value))
            }
        }
    }

    @Test
    fun `the constructor refuses anything but 32 bytes`() {
        assertThrows(IllegalArgumentException::class.java) { DsWake(ByteArray(31), DsWakeKind.MESSAGE) }
        assertThrows(IllegalArgumentException::class.java) { DsWake(ByteArray(33), DsWakeKind.MESSAGE) }
        assertThrows(IllegalArgumentException::class.java) { DsWake(ByteArray(0), DsWakeKind.CALL) }
    }

    @Test
    fun `a publish with no kind, no push_gid or a bad push_gid wakes nobody`() {
        // A control envelope: no kind, no header.
        assertNull(DsWake.forPublish(zeros, null))
        // ADR-0042 defer: no routing secret held yet, and never the group id.
        assertNull(DsWake.forPublish(null, DsWakeKind.MESSAGE))
        // Fail toward silence rather than a refused send.
        assertNull(DsWake.forPublish(ByteArray(16), DsWakeKind.CALL))
        assertEquals(DsWakeKind.CALL, DsWake.forPublish(zeros, DsWakeKind.CALL)?.kind)
    }

    @Test
    fun `the value does not change when the caller reuses its array`() {
        val gid = ByteArray(32)
        val wake = DsWake(gid, DsWakeKind.MESSAGE)
        gid.fill(-1)
        assertEquals("A".repeat(43) + ";message", wake.headerValue())
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
        return DsClient(baseUrl = "https://test.invalid", engine = engine, signers = registry)
    }

    private class Signer : DsJournalSigner {
        override val epoch: ULong = 1uL
        override fun sign(method: String, path: String, query: String, body: ByteArray): String = "proof"
        override val verifyingKey: ByteArray = ByteArray(32)
        override fun mailboxClaimBody(): String = """{"identity_pk_b64":"k"}"""
    }

    private val published = """{"id":7,"created_at":"t"}"""

    @Test
    fun `a publish asked to wake carries the header`() = runTest {
        val seen = mutableListOf<Seen>()
        client(listOf(HttpStatusCode.OK to published), seen).use {
            it.publishGroup(groupId, byteArrayOf(1), wake = DsWake(zeros, DsWakeKind.MESSAGE))
        }
        assertEquals("A".repeat(43) + ";message", seen.single().wake)
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
            it.publishGroup(groupId, byteArrayOf(1), wake = DsWake(zeros, DsWakeKind.CALL))
        }
        assertEquals(1, claims)
        assertEquals(2, seen.size)
        assertEquals(HttpMethod.Post, seen[0].method)
        assertEquals("A".repeat(43) + ";call", seen[0].wake)
        assertEquals(HttpMethod.Post, seen[1].method)
        assertEquals("A".repeat(43) + ";call", seen[1].wake)
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
