// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import java.io.IOException
import kotlinx.coroutines.test.runTest
import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.GroupInviteUploadRequest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The group-invite upload and revoke are behind discovery's `require_session`
 * layer. The repository's shared discovery client carries no token, so those
 * two go through a short-lived client stamped with the session token read at
 * call time; a missing session is loud, and only a transport failure falls
 * back to the inline link.
 */
class SessionDiscoveryTest {

    private val blobIdB64 = "A".repeat(8)

    private fun factory(
        seen: MutableList<String?>,
        status: HttpStatusCode = HttpStatusCode.OK,
        fail: Boolean = false,
    ): () -> DiscoveryClient = {
        val engine = MockEngine { request ->
            seen += request.headers[HttpHeaders.Authorization]
            if (fail) throw IOException("no route")
            respond(
                if (status == HttpStatusCode.OK) """{"blob_id":"$blobIdB64"}""" else "",
                status,
                headersOf(HttpHeaders.ContentType, "application/json"),
            )
        }
        DiscoveryClient(
            baseUrl = "http://test.invalid",
            engine = engine,
            inviteLinkLengths = { me.lakko.app.discovery.TEST_INVITE_LINK_LENGTHS },
        )
    }

    /** The wire half of `uploadGroupInvite`. */
    private val upload: suspend (DiscoveryClient) -> ByteArray = { client ->
        base64UrlDecodeNoPad(
            client.groupInviteUpload(GroupInviteUploadRequest(capability = "AA==", max_uses = 1L)).blob_id,
        )
    }

    @Test
    fun `the upload request carries the session token`() = runTest {
        val seen = mutableListOf<String?>()
        val blobId = uploadGroupInviteOrNull("tok-1", factory(seen), upload) {}
        assertArrayEquals(ByteArray(6), blobId)
        assertEquals(listOf<String?>("Bearer tok-1"), seen)
    }

    @Test
    fun `the token is the one read at call time`() = runTest {
        val seen = mutableListOf<String?>()
        var current = "old"
        val f = factory(seen)
        uploadGroupInviteOrNull(current, f, upload) {}
        current = "renewed"
        uploadGroupInviteOrNull(current, f, upload) {}
        assertEquals(listOf<String?>("Bearer old", "Bearer renewed"), seen)
    }

    @Test
    fun `no session fails loudly before any request`() = runTest {
        val seen = mutableListOf<String?>()
        assertThrows(ChatError::class.java) {
            kotlinx.coroutines.runBlocking { uploadGroupInviteOrNull(null, factory(seen), upload) {} }
        }
        assertThrows(ChatError::class.java) {
            kotlinx.coroutines.runBlocking { uploadGroupInviteOrNull("", factory(seen), upload) {} }
        }
        assertTrue(seen.isEmpty())
    }

    @Test
    fun `a refusal is thrown with its status, never turned into an inline link`() = runTest {
        val seen = mutableListOf<String?>()
        val logged = mutableListOf<String>()
        val error = assertThrows(ChatError::class.java) {
            kotlinx.coroutines.runBlocking {
                uploadGroupInviteOrNull("tok", factory(seen, status = HttpStatusCode.Unauthorized), upload) { logged += it }
            }
        }
        assertTrue(error.message!!.contains("401"))
        assertEquals(listOf("group invite upload refused: HTTP 401"), logged)
    }

    @Test
    fun `a transport failure answers null so the caller shares the inline link`() = runTest {
        val seen = mutableListOf<String?>()
        val logged = mutableListOf<String>()
        assertNull(uploadGroupInviteOrNull("tok", factory(seen, fail = true), upload) { logged += it })
        assertEquals(1, logged.size)
        assertTrue(logged.single().startsWith("group invite upload unreachable"))
    }

    @Test
    fun `a session-gated call runs on a client stamped with the token`() = runTest {
        val seen = mutableListOf<String?>()
        val token = withSessionDiscovery("tok-r", "revoke", factory(seen)) { it.sessionToken }
        assertEquals("tok-r", token)
    }
}
