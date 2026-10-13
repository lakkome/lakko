// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsSignerRegistry
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The own-echo filter belongs to one session.
 *
 * It is keyed on `(journal, row id)` and nothing else, and the chat client
 * lives for the whole process. Two accounts on one phone can be members of
 * the same group: a row the first one published and never polled back, left
 * registered, is dropped by the second one's poll as "my own echo" - a
 * message it never shows, or a commit it never applies.
 */
class OwnEchoSessionTest {

    private val groupId = ByteArray(32) { 7 }
    private val groupIdHex = DsClient.pseudonymHex(groupId)

    private fun chatClient(): ChatClient {
        val engine = MockEngine {
            respond(
                """{"id":11,"created_at":"t"}""",
                HttpStatusCode.OK,
                headersOf(HttpHeaders.ContentType, "application/json"),
            )
        }
        return ChatClient(ds = DsClient(baseUrl = "https://test.invalid", engine = engine, signers = DsSignerRegistry()))
    }

    @Test
    fun `a published row is registered as this session's echo`() = runTest {
        chatClient().use { client ->
            client.publishPreparedSend(client.preparedForTest(groupId, byteArrayOf(1)))
            assertEquals(1, client.outstandingOwnEchoes)
        }
    }

    @Test
    fun `the next session starts with no echoes of the one that left`() = runTest {
        chatClient().use { client ->
            client.publishPreparedSend(client.preparedForTest(groupId, byteArrayOf(1)))
            client.markOwnPublishedCommit(groupIdHex, 12L)
            assertEquals(2, client.outstandingOwnEchoes)

            client.forgetOwnEchoes()

            assertEquals(0, client.outstandingOwnEchoes)
        }
    }

    @Test
    fun `a publish still in flight at the sign-out registers nothing for the next session`() = runTest {
        chatClient().use { client ->
            // Prepared (sealed, persisted) under the session that then leaves.
            val prepared = client.preparedForTest(groupId, byteArrayOf(1))

            client.forgetOwnEchoes()
            client.publishPreparedSend(prepared)

            // Row 11 of this journal is NOT the next account's echo: its poll
            // must apply it.
            assertEquals(0, client.outstandingOwnEchoes)
        }
    }
}
