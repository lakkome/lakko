// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.client.plugins.HttpRequestTimeoutException
import io.ktor.client.plugins.HttpTimeoutCapability
import io.ktor.client.plugins.HttpTimeoutConfig
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpMethod
import io.ktor.http.HttpStatusCode
import io.ktor.http.content.OutgoingContent
import io.ktor.http.headersOf
import java.io.IOException
import java.util.Base64
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side unit tests for [DsClient]. MockEngine-backed; no
 * cdylib / JNA dependency, runs under `:app:testDebugUnitTest`
 * on the host JVM.
 *
 * Properties locked down here:
 *
 *   1. **Lowercase-hex URL path** for the 32-byte pseudonym
 *      (the DS server matches lowercase-only).
 *   2. **`application/octet-stream` content-type** on publish +
 *      the raw bytes go through as the body (no JSON wrapping,
 *      no base64).
 *   3. **JSON fetch envelope** decodes the snake_case wire
 *      (`payload_b64`, `created_at`, `has_more`) and the
 *      payload bytes round-trip through the base64 decoder.
 *   4. **Cursor / limit / wait query params** are appended when
 *      provided and omitted when `null` — the DS server treats
 *      omitted params as defaults (after=0, limit=server-default,
 *      wait=0).
 *   5. **Non-2xx throws [DsHttpError]**, transport failure
 *      throws [DsNetworkError]**, both with the right endpoint.
 *   6. **Arg-validation gates** fail-fast before any network
 *      call (wrong-length pseudonym, empty payload, empty
 *      groupId).
 */
class DsClientTest {

    private data class CapturedRequest(
        val method: HttpMethod,
        val url: String,
        val contentType: String?,
        val rawBody: ByteArray?,
    )

    private fun client(
        responses: List<Pair<HttpStatusCode, String>>,
        captured: MutableList<CapturedRequest> = mutableListOf(),
    ): DsClient {
        var index = 0
        val engine = MockEngine { request ->
            // Match the ABSTRACT `OutgoingContent.ByteArrayContent`, not the
            // concrete `io.ktor.http.content.ByteArrayContent`. Ktor's
            // `defaultTransformers` wraps a raw `ByteArray` body in an
            // ANONYMOUS subclass of the abstract type, so an `is` check
            // against the concrete class silently misses every request the
            // client actually sends. The abstract type covers all three
            // shapes we can see here: that anonymous wrapper, the concrete
            // `ByteArrayContent`, and `TextContent`.
            val rawBody: ByteArray? = when (val outgoing = request.body) {
                is OutgoingContent.ByteArrayContent -> outgoing.bytes()
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
            val (status, body) = responses[index]
            index += 1
            respond(
                content = body,
                status = status,
                headers = headersOf(HttpHeaders.ContentType, "application/json"),
            )
        }
        return DsClient(baseUrl = "http://test.invalid", engine = engine)
    }

    // ===================================================================
    // pseudonymHex helper
    // ===================================================================

    @Test
    fun `pseudonymHex emits lowercase hex with no separators`() {
        val bytes = byteArrayOf(0x00, 0x0F.toByte(), 0xFF.toByte(), 0xA5.toByte())
        assertEquals("000fffa5", DsClient.pseudonymHex(bytes))
    }

    @Test
    fun `pseudonymHex round-trips 32 bytes`() {
        val bytes = ByteArray(32) { (it * 7).toByte() }
        val hex = DsClient.pseudonymHex(bytes)
        assertEquals(64, hex.length)
        // No uppercase letters, all hex chars.
        assertEquals(hex.lowercase(), hex)
        assertTrue(hex.all { it in '0'..'9' || it in 'a'..'f' })
    }

    // ===================================================================
    // publishMailbox — happy path, content-type, octet-stream body
    // ===================================================================

    @Test
    fun `publishMailbox sends octet-stream + parses id and createdAt`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(
                HttpStatusCode.OK to """{"id":42,"created_at":"2026-05-12T10:30:00Z"}""",
            ),
            captured = captured,
        ).use { client ->
            val pseudonym = ByteArray(32) { it.toByte() }
            val payload = "hello world".toByteArray(Charsets.UTF_8)
            val result = client.publishMailbox(pseudonym, payload)
            assertEquals(42L, result.id)
            assertEquals("2026-05-12T10:30:00Z", result.createdAt)
        }
        val req = captured[0]
        assertEquals(HttpMethod.Post, req.method)
        assertTrue(req.contentType?.startsWith("application/octet-stream") == true)
        // Body is the raw payload — not JSON-wrapped, not base64.
        assertArrayEquals("hello world".toByteArray(Charsets.UTF_8), req.rawBody)
        // URL path includes lowercase hex of the pseudonym (32 bytes → 64 chars).
        assertTrue(req.url.contains("/ds/mailbox/"))
        val hexSegment = req.url
            .substringAfter("/ds/mailbox/")
            .substringBefore("/messages")
        assertEquals(64, hexSegment.length)
        assertEquals(hexSegment.lowercase(), hexSegment)
    }

    // ===================================================================
    // publishGroup parity
    // ===================================================================

    @Test
    fun `publishGroup uses the group path`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(HttpStatusCode.OK to """{"id":1,"created_at":"t"}"""),
            captured = captured,
        ).use { client ->
            client.publishGroup(byteArrayOf(1, 2, 3, 4), byteArrayOf(0x55))
        }
        assertTrue(captured[0].url.contains("/ds/group/"))
        // Group id can be any non-empty length — encoded as hex.
        val hexSegment = captured[0].url
            .substringAfter("/ds/group/")
            .substringBefore("/messages")
        assertEquals("01020304", hexSegment)
    }

    // ===================================================================
    // fetchMailbox — JSON wire shape, base64 decode, has_more
    // ===================================================================

    @Test
    fun `fetchMailbox decodes payload_b64 and surfaces hasMore`() = runTest {
        val rawPayload = "encrypted".toByteArray(Charsets.UTF_8)
        val payloadB64 = Base64.getEncoder().encodeToString(rawPayload)
        val body = """
            {
              "messages": [
                {"id":1,"payload_b64":"$payloadB64","created_at":"2026-05-12T10:00:00Z"},
                {"id":2,"payload_b64":"$payloadB64","created_at":"2026-05-12T10:00:01Z"}
              ],
              "has_more": true
            }
        """.trimIndent()
        client(
            responses = listOf(HttpStatusCode.OK to body),
        ).use { client ->
            val result = client.fetchMailbox(ByteArray(32))
            assertEquals(2, result.messages.size)
            assertEquals(1L, result.messages[0].id)
            assertArrayEquals(rawPayload, result.messages[0].payload)
            assertEquals("2026-05-12T10:00:00Z", result.messages[0].createdAt)
            assertEquals(2L, result.messages[1].id)
            assertTrue(result.hasMore)
        }
    }

    @Test
    fun `fetchMailbox empty response surfaces empty list + hasMore false`() = runTest {
        client(
            responses = listOf(HttpStatusCode.OK to """{"messages":[],"has_more":false}"""),
        ).use { client ->
            val result = client.fetchMailbox(ByteArray(32))
            assertTrue(result.messages.isEmpty())
            assertFalse(result.hasMore)
        }
    }

    // ===================================================================
    // Cursor / limit / wait params
    // ===================================================================

    @Test
    fun `fetchMailbox omits query params when null`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(HttpStatusCode.OK to """{"messages":[],"has_more":false}"""),
            captured = captured,
        ).use { client ->
            client.fetchMailbox(ByteArray(32))
        }
        // No `?after=`, `?limit=`, `?wait=` in the URL.
        assertFalse(captured[0].url.contains("after="))
        assertFalse(captured[0].url.contains("limit="))
        assertFalse(captured[0].url.contains("wait="))
    }

    @Test
    fun `fetchMailbox appends after limit and wait params when provided`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(HttpStatusCode.OK to """{"messages":[],"has_more":false}"""),
            captured = captured,
        ).use { client ->
            client.fetchMailbox(
                pseudonym = ByteArray(32),
                after = 42L,
                limit = 100,
                waitSecs = 30,
            )
        }
        val url = captured[0].url
        assertTrue("after=42 not found in $url", url.contains("after=42"))
        assertTrue("limit=100 not found in $url", url.contains("limit=100"))
        assertTrue("wait=30 not found in $url", url.contains("wait=30"))
    }

    // ===================================================================
    // Error surfaces
    // ===================================================================

    @Test
    fun `non-2xx response throws DsHttpError with status and endpoint`() {
        val err = assertThrows(DsHttpError::class.java) {
            runTest {
                client(
                    responses = listOf(HttpStatusCode.PayloadTooLarge to "too big"),
                ).use { client ->
                    client.publishMailbox(ByteArray(32), ByteArray(1024 * 1024))
                }
            }
        }
        assertEquals(413, err.status)
        assertTrue(err.endpoint.contains("/ds/mailbox/"))
    }

    @Test
    fun `transport failure throws DsNetworkError`() {
        val engine = MockEngine { throw IOException("simulated DNS failure") }
        val err = assertThrows(DsNetworkError::class.java) {
            runTest {
                DsClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
                    client.fetchMailbox(ByteArray(32))
                }
            }
        }
        assertTrue(err.endpoint.contains("/ds/mailbox/"))
        assertTrue(err.message!!.contains("simulated DNS failure"))
    }

    // ===================================================================
    // Arg-validation gates — fail-fast before any network call
    // ===================================================================

    @Test
    fun `publishMailbox rejects wrong-length pseudonym before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DsClientArgError::class.java) {
            runTest {
                DsClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
                    client.publishMailbox(ByteArray(16), byteArrayOf(1))
                }
            }
        }
    }

    @Test
    fun `publishMailbox rejects empty payload before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DsClientArgError::class.java) {
            runTest {
                DsClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
                    client.publishMailbox(ByteArray(32), ByteArray(0))
                }
            }
        }
    }

    @Test
    fun `publishGroup rejects empty groupId before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DsClientArgError::class.java) {
            runTest {
                DsClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
                    client.publishGroup(ByteArray(0), byteArrayOf(1))
                }
            }
        }
    }

    @Test
    fun `fetchMailbox rejects wrong-length pseudonym before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DsClientArgError::class.java) {
            runTest {
                DsClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
                    client.fetchMailbox(ByteArray(8))
                }
            }
        }
    }

    // ===================================================================
    // G-37 — explicit request timeouts
    //
    // Before this, nothing installed `HttpTimeout`: the client had only
    // `ContentNegotiation` over an empty `OkHttp.create {}`, so the effective
    // bound was whatever the engine inherited — unknown. Every DS publish runs
    // inside `ChatRepository`'s single repository-wide `groupsLock`, so an
    // unbounded publish is an unbounded hold of a lock every other group's send
    // queues behind.
    //
    // The timeout VALUES are asserted here off the per-request capability rather
    // than by waiting for one to fire — a firing test would have to burn the real
    // 15 s / 40 s budget in wall-clock time (Ktor's timeout machinery runs on the
    // engine's dispatcher, not the virtual-time test scheduler). What the plugin
    // does once a budget elapses is Ktor's own well-tested behaviour; what is
    // ours — and what these pin — is (a) that a budget is configured at all,
    // (b) that it is the intended value, (c) that a long poll gets a WIDER one so
    // it is not severed by design, and (d) that the resulting exception is routed
    // to `DsNetworkError` rather than mistaken for cancellation.
    // ===================================================================

    /** Capture the effective per-request timeout config the client attached. */
    private fun timeoutCapturingClient(
        onCapture: (HttpTimeoutConfig?) -> Unit,
    ): DsClient {
        val engine = MockEngine { request ->
            onCapture(request.getCapabilityOrNull(HttpTimeoutCapability))
            respond(
                content = """{"messages":[],"has_more":false}""",
                status = HttpStatusCode.OK,
                headers = headersOf(HttpHeaders.ContentType, "application/json"),
            )
        }
        return DsClient(baseUrl = "http://test.invalid", engine = engine)
    }

    @Test
    fun `G-37 — a non-long-poll fetch carries the default 15s request budget`() = runTest {
        var captured: HttpTimeoutConfig? = null
        timeoutCapturingClient { captured = it }.use { client ->
            client.fetchGroup(ByteArray(32) { 1 }, after = 4)
        }
        assertEquals("a wait=0 fetch is bounded at the default", 15_000L, captured?.requestTimeoutMillis)
        assertEquals(10_000L, captured?.connectTimeoutMillis)
    }

    @Test
    fun `G-37 — a publish carries the default 15s request budget`() = runTest {
        var captured: HttpTimeoutConfig? = null
        val engine = MockEngine { request ->
            captured = request.getCapabilityOrNull(HttpTimeoutCapability)
            respond(
                content = """{"id":1,"created_at":"2026-01-01T00:00:00Z"}""",
                status = HttpStatusCode.OK,
                headers = headersOf(HttpHeaders.ContentType, "application/json"),
            )
        }
        DsClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
            client.publishGroup(ByteArray(32) { 2 }, byteArrayOf(9))
        }
        assertEquals(
            "every publish — all of which run under groupsLock — is bounded",
            15_000L,
            captured?.requestTimeoutMillis,
        )
    }

    @Test
    fun `G-37 — a long-poll fetch widens BOTH request and socket budgets to waitSecs plus 10`() =
        runTest {
            var captured: HttpTimeoutConfig? = null
            timeoutCapturingClient { captured = it }.use { client ->
                client.fetchGroup(ByteArray(32) { 3 }, after = 0, waitSecs = 30)
            }
            // Mirrors iOS `DsClient.swift` (`timeoutInterval = waitSecs + 10`).
            assertEquals(
                "a 30s long poll must NOT be severed by the 15s default",
                40_000L,
                captured?.requestTimeoutMillis,
            )
            assertEquals(
                "a long poll is an idle socket by definition — the socket budget " +
                    "must widen too or it fires first",
                40_000L,
                captured?.socketTimeoutMillis,
            )
        }

    @Test
    fun `G-37 — a request timeout surfaces as DsNetworkError, not as cancellation`() {
        // Ktor's HttpRequestTimeoutException extends java.io.IOException (verified
        // against ktor-client-core 3.0.1), so it takes the generic branch and is
        // wrapped — which is what lets `ChatClient.sendMessage`'s narrow
        // `catch (e: DsNetworkError)` convert it into a `ChatError` instead of
        // letting a raw throwable escape past callers that expect one.
        val engine = MockEngine {
            throw HttpRequestTimeoutException("http://test.invalid", 15_000L)
        }
        val thrown = assertThrows(DsNetworkError::class.java) {
            runTest {
                DsClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
                    client.publishGroup(ByteArray(32) { 4 }, byteArrayOf(1))
                }
            }
        }
        assertTrue(
            "the timeout must be preserved as the cause",
            thrown.cause is HttpRequestTimeoutException,
        )
    }

    @Test
    fun `G-37 — a genuine cancellation propagates verbatim and is NOT relabelled`() {
        // Structured concurrency: a cancelled send is not a failed send. Before
        // this, `catch (cause: Throwable)` swallowed CancellationException into a
        // DsNetworkError, which callers surface to the user as a failed send.
        val engine = MockEngine { throw CancellationException("scope cancelled") }
        val thrown = assertThrows(CancellationException::class.java) {
            runTest {
                DsClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
                    client.publishGroup(ByteArray(32) { 5 }, byteArrayOf(1))
                }
            }
        }
        // `assertThrows(CancellationException::class)` above already proves it was
        // not re-labelled (DsNetworkError is not a CancellationException). Pin the
        // message too, so it is the ORIGINAL cancellation propagating verbatim and
        // not a fresh one manufactured somewhere in the catch chain.
        assertEquals("scope cancelled", thrown.message)
    }
}
