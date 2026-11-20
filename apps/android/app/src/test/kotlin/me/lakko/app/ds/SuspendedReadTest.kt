// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpMethod
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import java.util.Base64
import kotlinx.coroutines.test.runTest
import me.lakko.app.auth.SuspensionGate
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
import me.lakko.app.transport.SessionBearerEngine
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * ADR-0098 section 4 - reading while suspended. The transport refuses every
 * write to the Delivery Service before it leaves the device; what a chat
 * READ needs from the DS is a signed GET, so it still goes out, again and
 * again, and a refused write in between (a read receipt, a claim) does not
 * stop the next read. A mailbox read refused at the DS tries a re-claim (a
 * PUT): that write is refused too, and the read fails the way it does when
 * the claim does not land, not with a new kind of error.
 */
class SuspendedReadTest {

    private val groupId = ByteArray(32) { (it + 1).toByte() }
    private val mailbox = ByteArray(32) { (0xF0 + it % 16).toByte() }
    private val payload = Base64.getEncoder().encodeToString("sealed".toByteArray())
    private val rows = """{"messages":[{"id":1,"payload_b64":"$payload","created_at":"t"},""" +
        """{"id":2,"payload_b64":"$payload","created_at":"t"}],"has_more":false}"""
    private val seen = mutableListOf<HttpMethod>()

    @Before
    fun setUp() {
        SuspensionGate.set(true)
    }

    @After
    fun tearDown() {
        SuspensionGate.set(false)
    }

    private fun client(registry: DsSignerRegistry, status: (HttpMethod) -> Pair<HttpStatusCode, String>): DsClient {
        val mock = MockEngine { request ->
            seen += request.method
            val (code, body) = status(request.method)
            respond(body, code, headersOf(HttpHeaders.ContentType, "application/json"))
        }
        return DsClient(
            baseUrl = ApiEndpoints.baseUrlFor(ApiService.DS),
            engine = SessionBearerEngine(mock),
            signers = registry,
            anonTokens = me.lakko.app.anon.FakeAnonTokenSource(),
        )
    }

    @Test
    fun `a chat's journal is read and read again while every write is refused`() = runTest {
        val registry = DsSignerRegistry()
        registry.installStatic(dsGroupKey(groupId), FakeJournalSigner(3uL))
        client(registry) { HttpStatusCode.OK to rows }.use { ds ->
            assertEquals(listOf(1L, 2L), ds.fetchGroup(groupId, after = 0).messages.map { it.id })
            // A write the reading screen may still attempt (a read receipt):
            // refused before it leaves the device.
            val refused = runCatching { ds.publishGroup(groupId, byteArrayOf(1, 2, 3)) }.exceptionOrNull()
            assertTrue("the write was refused", refused != null)
            // The poll goes on: the next read is unaffected.
            assertEquals(listOf(1L, 2L), ds.fetchGroup(groupId, after = 0).messages.map { it.id })
        }
        assertEquals(listOf(HttpMethod.Get, HttpMethod.Get), seen)
    }

    @Test
    fun `a refused mailbox read tries no write and fails like a refusal, then reads again`() = runTest {
        val registry = DsSignerRegistry()
        registry.installStatic(dsMailboxKey(mailbox), FakeJournalSigner(0uL))
        var refuseReads = true
        client(registry) { method ->
            when {
                method == HttpMethod.Get && refuseReads -> HttpStatusCode.Unauthorized to "{}"
                method == HttpMethod.Get -> HttpStatusCode.OK to rows
                else -> HttpStatusCode.OK to "{}"
            }
        }.use { ds ->
            val failure = runCatching { ds.fetchMailbox(mailbox, after = 0) }.exceptionOrNull()
            assertTrue("refused as a proof refusal: $failure", failure is DsProofRefusedError)
            refuseReads = false
            assertEquals(2, ds.fetchMailbox(mailbox, after = 0).messages.size)
        }
        // The re-claim PUT never reached the network.
        assertTrue(seen.none { it == HttpMethod.Put || it == HttpMethod.Post })
    }
}
