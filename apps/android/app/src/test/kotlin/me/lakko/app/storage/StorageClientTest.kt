// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.storage

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpMethod
import io.ktor.http.HttpStatusCode
import io.ktor.http.content.ByteArrayContent
import io.ktor.http.content.OutgoingContent
import io.ktor.http.headersOf
import java.io.IOException
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side unit tests for [StorageClient]. MockEngine-backed;
 * no cdylib / JNA dependency, runs under
 * `:app:testDebugUnitTest` on the host JVM.
 *
 * Properties locked down here:
 *
 *   1. **Lowercase-hex URL path** for the 32-byte blob_id
 *      (the storage server matches lowercase-only).
 *   2. **`application/octet-stream` content-type** on PUT + the
 *      raw bytes go through as the body (no JSON wrapping, no
 *      base64).
 *   3. **200 and 201 are both success** on PUT (idempotent
 *      re-upload vs first upload).
 *   4. **404 returns `null`** on GET / HEAD — typed not-found
 *      path so the UI can branch without a try / catch.
 *   5. **Non-2xx / non-404 throws [StorageHttpError]**,
 *      transport failure throws [StorageNetworkError], both
 *      with the right endpoint.
 *   6. **Arg-validation gates** fail-fast before any network
 *      call (wrong-length blob_id, empty / oversize ciphertext).
 */
class StorageClientTest {

    private data class CapturedRequest(
        val method: HttpMethod,
        val url: String,
        val contentType: String?,
        val rawBody: ByteArray?,
    )

    private fun mockEngine(
        captured: MutableList<CapturedRequest>,
        steps: List<MockStep>,
    ): MockEngine {
        var index = 0
        return MockEngine { request ->
            val rawBody: ByteArray? = when (val outgoing = request.body) {
                is ByteArrayContent -> outgoing.bytes()
                is OutgoingContent.NoContent -> null
                else -> error("unexpected OutgoingContent: ${outgoing::class}")
            }
            captured.add(
                CapturedRequest(
                    method = request.method,
                    url = request.url.toString(),
                    contentType = request.body.contentType?.toString(),
                    rawBody = rawBody,
                ),
            )
            val step = steps[index]
            index += 1
            val baseHeaders = listOf(HttpHeaders.ContentType to "application/octet-stream")
            val allHeaders = baseHeaders + step.headers.toList()
            respond(
                content = step.body,
                status = step.status,
                headers = headersOf(
                    *allHeaders.map { (k, v) -> k to listOf(v) }.toTypedArray(),
                ),
            )
        }
    }

    private data class MockStep(
        val status: HttpStatusCode,
        val body: ByteArray = ByteArray(0),
        val headers: Map<String, String> = emptyMap(),
    )

    // ===================================================================
    // blobIdHex helper
    // ===================================================================

    @Test
    fun `blobIdHex emits lowercase hex with no separators`() {
        val bytes = byteArrayOf(0x00, 0x0F.toByte(), 0xFF.toByte(), 0xA5.toByte())
        assertEquals("000fffa5", StorageClient.blobIdHex(bytes))
    }

    @Test
    fun `blobIdHex round-trips 32 bytes`() {
        val bytes = ByteArray(32) { (it * 11).toByte() }
        val hex = StorageClient.blobIdHex(bytes)
        assertEquals(64, hex.length)
        assertEquals(hex.lowercase(), hex)
        assertTrue(hex.all { it in '0'..'9' || it in 'a'..'f' })
    }

    // ===================================================================
    // putBlob — happy path
    // ===================================================================

    @Test
    fun `putBlob sends octet-stream body and accepts 201 Created`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        val engine = mockEngine(
            captured,
            listOf(MockStep(HttpStatusCode.Created)),
        )
        StorageClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
            val blobId = ByteArray(32) { it.toByte() }
            val ciphertext = "ciphertext".toByteArray(Charsets.UTF_8)
            client.putBlob(blobId, ciphertext)
        }
        val req = captured[0]
        assertEquals(HttpMethod.Put, req.method)
        assertTrue(req.contentType?.startsWith("application/octet-stream") == true)
        assertArrayEquals("ciphertext".toByteArray(Charsets.UTF_8), req.rawBody)
        // URL: /blob/<64-char lowercase hex>
        val hex = req.url.substringAfterLast("/blob/")
        assertEquals(64, hex.length)
        assertEquals(hex.lowercase(), hex)
    }

    @Test
    fun `putBlob accepts 200 OK as idempotent re-upload`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        val engine = mockEngine(captured, listOf(MockStep(HttpStatusCode.OK)))
        StorageClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
            client.putBlob(ByteArray(32), byteArrayOf(1, 2, 3))
        }
        assertEquals(1, captured.size)
    }

    // ===================================================================
    // getBlob — 200 / 404
    // ===================================================================

    @Test
    fun `getBlob returns raw bytes on 200`() = runTest {
        val payload = "ciphertext bytes".toByteArray(Charsets.UTF_8)
        val captured = mutableListOf<CapturedRequest>()
        val engine = mockEngine(
            captured,
            listOf(MockStep(HttpStatusCode.OK, body = payload)),
        )
        StorageClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
            val got = client.getBlob(ByteArray(32))
            assertArrayEquals(payload, got)
        }
        assertEquals(HttpMethod.Get, captured[0].method)
    }

    @Test
    fun `getBlob returns null on 404`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        val engine = mockEngine(captured, listOf(MockStep(HttpStatusCode.NotFound)))
        StorageClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
            assertNull(client.getBlob(ByteArray(32)))
        }
    }

    // ===================================================================
    // headBlob — Content-Length parse
    // ===================================================================

    @Test
    fun `headBlob parses Content-Length on 200`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        val engine = mockEngine(
            captured,
            listOf(
                MockStep(
                    HttpStatusCode.OK,
                    headers = mapOf(HttpHeaders.ContentLength to "1234"),
                ),
            ),
        )
        StorageClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
            assertEquals(1234L, client.headBlob(ByteArray(32)))
        }
        assertEquals(HttpMethod.Head, captured[0].method)
    }

    @Test
    fun `headBlob returns null on 404`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        val engine = mockEngine(captured, listOf(MockStep(HttpStatusCode.NotFound)))
        StorageClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
            assertNull(client.headBlob(ByteArray(32)))
        }
    }

    // ===================================================================
    // Error surfaces
    // ===================================================================

    @Test
    fun `putBlob with hash mismatch throws StorageHttpError 400`() {
        val captured = mutableListOf<CapturedRequest>()
        val engine = mockEngine(
            captured,
            listOf(MockStep(HttpStatusCode.BadRequest, body = "hash mismatch".toByteArray())),
        )
        val err = assertThrows(StorageHttpError::class.java) {
            runTest {
                StorageClient(
                    baseUrl = "http://test.invalid",
                    engine = engine,
                ).use { client ->
                    client.putBlob(ByteArray(32), byteArrayOf(1, 2, 3))
                }
            }
        }
        assertEquals(400, err.status)
        assertTrue(err.endpoint.startsWith("/blob/"))
    }

    @Test
    fun `transport failure throws StorageNetworkError`() {
        val engine = MockEngine { throw IOException("simulated DNS failure") }
        val err = assertThrows(StorageNetworkError::class.java) {
            runTest {
                StorageClient(
                    baseUrl = "http://test.invalid",
                    engine = engine,
                ).use { client ->
                    client.getBlob(ByteArray(32))
                }
            }
        }
        assertTrue(err.endpoint.startsWith("/blob/"))
        assertTrue(err.message!!.contains("simulated DNS failure"))
    }

    // ===================================================================
    // Arg-validation gates — fail-fast before any network call
    // ===================================================================

    @Test
    fun `putBlob rejects wrong-length blobId before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(StorageClientArgError::class.java) {
            runTest {
                StorageClient(
                    baseUrl = "http://test.invalid",
                    engine = engine,
                ).use { client ->
                    client.putBlob(ByteArray(16), byteArrayOf(1))
                }
            }
        }
    }

    @Test
    fun `putBlob rejects empty ciphertext before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(StorageClientArgError::class.java) {
            runTest {
                StorageClient(
                    baseUrl = "http://test.invalid",
                    engine = engine,
                ).use { client ->
                    client.putBlob(ByteArray(32), ByteArray(0))
                }
            }
        }
    }

    @Test
    fun `putBlob rejects oversize ciphertext before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(StorageClientArgError::class.java) {
            runTest {
                StorageClient(
                    baseUrl = "http://test.invalid",
                    engine = engine,
                ).use { client ->
                    // One byte past the 50 MB ceiling.
                    val tooBig = ByteArray(StorageClient.MAX_BLOB_BYTES + 1)
                    client.putBlob(ByteArray(32), tooBig)
                }
            }
        }
    }

    @Test
    fun `getBlob rejects wrong-length blobId before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(StorageClientArgError::class.java) {
            runTest {
                StorageClient(
                    baseUrl = "http://test.invalid",
                    engine = engine,
                ).use { client ->
                    client.getBlob(ByteArray(8))
                }
            }
        }
    }

    @Test
    fun `headBlob rejects wrong-length blobId before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(StorageClientArgError::class.java) {
            runTest {
                StorageClient(
                    baseUrl = "http://test.invalid",
                    engine = engine,
                ).use { client ->
                    client.headBlob(ByteArray(0))
                }
            }
        }
    }
}
