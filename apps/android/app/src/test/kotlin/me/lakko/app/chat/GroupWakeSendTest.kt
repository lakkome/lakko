// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import me.lakko.app.ds.DS_WAKE_HEADER
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsSignerRegistry
import me.lakko.app.ds.DsWake
import me.lakko.app.ds.DsWakeKind
import me.lakko.app.push.PushClient
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0054 decision 1 — the send path wakes the group through the signed DS
 * publish and talks to nothing else: one request, to the group journal,
 * carrying `x-lakko-wake`, and no request to push-gw.
 */
class GroupWakeSendTest {

    private data class Seen(val path: String, val wake: String?)

    private val groupId = ByteArray(32) { 7 }

    private fun chatClient(seen: MutableList<Seen>): ChatClient {
        val engine = MockEngine { request ->
            seen += Seen(request.url.encodedPath, request.headers[DS_WAKE_HEADER])
            respond(
                """{"id":11,"created_at":"t"}""",
                HttpStatusCode.OK,
                headersOf(HttpHeaders.ContentType, "application/json"),
            )
        }
        return ChatClient(ds = DsClient(baseUrl = "https://test.invalid", engine = engine, signers = DsSignerRegistry()))
    }

    @Test
    fun `a message send is one DS publish carrying the wake and no push-gw call`() = runTest {
        val seen = mutableListOf<Seen>()
        val wake = DsWake.forPublish(ByteArray(32), DsWakeKind.MESSAGE)
        val result = chatClient(seen).use {
            it.publishPreparedSend(it.preparedForTest(groupId, byteArrayOf(1, 2)), wake = wake)
        }
        assertEquals(11L, result.messageId)
        val request = seen.single()
        assertEquals("/ds/group/${DsClient.pseudonymHex(groupId)}/messages", request.path)
        assertEquals("A".repeat(43) + ";message", request.wake)
        assertFalse(seen.any { it.path.contains("/push/") })
    }

    @Test
    fun `a control publish carries no wake`() = runTest {
        val seen = mutableListOf<Seen>()
        chatClient(seen).use {
            it.publishPreparedSend(it.preparedForTest(groupId, byteArrayOf(1)))
        }
        assertNull(seen.single().wake)
    }

    @Test
    fun `push-gw offers no client wake any more`() {
        // The per-group wake moved to the DS; a client-side notify would be a
        // second, anonymous route to wake a group (ADR-0054 context, finding 1).
        val names = PushClient::class.java.declaredMethods.map { it.name }
        assertTrue(names.none { it.startsWith("notify") })
    }
}
