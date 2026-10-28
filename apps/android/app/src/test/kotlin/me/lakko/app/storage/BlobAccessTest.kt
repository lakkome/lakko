// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.storage

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0065 section 5 / 6 — the pass cache, the refusal mapping, and the
 *  storage client's pass + delete-key handling (MockEngine, no native). */
class BlobAccessTest {
    private val json = headersOf("Content-Type", "application/json")

    @Test
    fun a_cached_pass_is_used_until_a_minute_before_it_expires() {
        var now = 0L
        val cache = BlobPassCache(now = { now })
        cache.put(BlobPass("p", BlobPassKind.READ, expiresAtMs = 3_600_000))
        assertEquals("p", cache.current(BlobPassKind.READ)?.value)
        assertNull(cache.current(BlobPassKind.UPLOAD))
        now = 3_600_000 - BlobPassCache.MARGIN_MS - 1
        assertEquals("p", cache.current(BlobPassKind.READ)?.value)
        now += 1
        assertNull(cache.current(BlobPassKind.READ))
    }

    @Test
    fun invalidate_drops_only_the_refused_pass() {
        val cache = BlobPassCache(now = { 0 })
        cache.put(BlobPass("new", BlobPassKind.UPLOAD, 10_000_000))
        cache.invalidate(BlobPassKind.UPLOAD, "old")
        assertEquals("new", cache.current(BlobPassKind.UPLOAD)?.value)
        cache.invalidate(BlobPassKind.UPLOAD, "new")
        assertNull(cache.current(BlobPassKind.UPLOAD))
    }

    @Test
    fun refusals_map_to_the_ux_states() {
        assertEquals(BlobRefusal.GONE, classifyBlobRefusal(404, """{"error":"blob-not-found"}""", false))
        for (code in listOf("blob-pass-required", "blob-pass-invalid", "blob-pass-expired")) {
            assertEquals(BlobRefusal.RENEW_PASS, classifyBlobRefusal(401, """{"error":"$code"}""", false))
            assertEquals(BlobRefusal.OTHER, classifyBlobRefusal(401, """{"error":"$code"}""", true))
        }
        assertEquals(BlobRefusal.RENEW_PASS, classifyBlobRefusal(429, """{"error":"blob-read-budget-exhausted"}""", false))
        assertEquals(BlobRefusal.LIMIT, classifyBlobRefusal(429, """{"error":"blob-read-budget-exhausted"}""", true))
        assertEquals(BlobRefusal.LIMIT, classifyBlobRefusal(429, """{"error":"blob-upload-budget-exhausted"}""", true))
        // nginx answers 429 with HTML: no code, the generic failure.
        assertEquals(BlobRefusal.OTHER, classifyBlobRefusal(429, "<html>Too Many Requests</html>", true))
        assertEquals(BlobRefusal.OTHER, classifyBlobRefusal(503, """{"error":"blob-node-unavailable"}""", false))
    }

    @Test
    fun a_pass_answer_parses() {
        val p = parseBlobPass(BlobPassKind.READ, """{"pass":"abc","kind":"read","expires_at_unix_secs":100,"budget":{"downloads":3000,"keeps":5000}}""")
        assertEquals(BlobPass("abc", BlobPassKind.READ, 100_000), p)
        assertNull(parseBlobPass(BlobPassKind.READ, "{}"))
        assertNull(parseBlobPass(BlobPassKind.READ, "not json"))
    }

    private class FakePasses : BlobPassSource {
        var issued = 0
        val refused = mutableListOf<String>()
        override suspend fun pass(kind: BlobPassKind, refresh: Boolean): String {
            if (refresh || issued == 0) issued += 1
            return "${kind.path}-$issued"
        }

        override fun refused(kind: BlobPassKind, value: String) {
            refused += value
        }
    }

    private object FakeKeys : BlobDeleteKeys {
        override fun publicKey(blobId: ByteArray): ByteArray = ByteArray(32) { 7 }
        override fun deleteProof(blobId: ByteArray, path: String): Pair<String, String> = "x-lakko-storage-proof" to "v1.sig"
    }

    private val blob = ByteArray(32) { 1 }

    @Test
    fun a_put_carries_the_pass_and_the_delete_key_and_renews_a_stale_pass_once() = runTest {
        val seen = mutableListOf<Pair<String?, String?>>()
        var call = 0
        val engine = MockEngine { req ->
            seen += req.headers[BLOB_PASS_HEADER] to req.headers[BLOB_DELETE_KEY_HEADER]
            call += 1
            if (call == 1) {
                respond("""{"error":"blob-pass-expired"}""", HttpStatusCode.Unauthorized, json)
            } else {
                respond("", HttpStatusCode.Created)
            }
        }
        val passes = FakePasses()
        StorageClient("http://s", engine, passes, FakeKeys).putBlob(blob, byteArrayOf(1, 2, 3))
        assertEquals(listOf("upload-1", "upload-2"), seen.map { it.first })
        val key = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32) { 7 })
        assertTrue(seen.all { it.second == key })
        assertEquals(listOf("upload-1"), passes.refused)
    }

    @Test
    fun a_budget_refusal_under_a_fresh_pass_is_the_limit() = runTest {
        val engine = MockEngine { respond("""{"error":"blob-read-budget-exhausted"}""", HttpStatusCode.TooManyRequests, json) }
        val err = assertThrows(StorageLimitError::class.java) {
            kotlinx.coroutines.runBlocking { StorageClient("http://s", engine, FakePasses(), FakeKeys).getBlob(blob) }
        }
        assertEquals(BlobPassKind.READ, err.kind)
    }

    @Test
    fun a_missing_blob_is_null_and_html_429_is_a_plain_error() = runTest {
        val gone = MockEngine { respond("""{"error":"blob-not-found"}""", HttpStatusCode.NotFound, json) }
        assertNull(StorageClient("http://s", gone, FakePasses(), FakeKeys).getBlob(blob))
        val nginx = MockEngine { respond("<html>busy</html>", HttpStatusCode.TooManyRequests) }
        assertThrows(StorageHttpError::class.java) {
            kotlinx.coroutines.runBlocking { StorageClient("http://s", nginx, FakePasses(), FakeKeys).getBlob(blob) }
        }
    }

    @Test
    fun delete_is_signed_and_a_refusal_is_final() = runTest {
        var header: String? = null
        val ok = MockEngine { req ->
            header = req.headers["x-lakko-storage-proof"]
            respond("", HttpStatusCode.NoContent)
        }
        assertTrue(StorageClient("http://s", ok, null, FakeKeys).deleteBlob(blob))
        assertEquals("v1.sig", header)
        val refused = MockEngine { respond("""{"error":"blob-delete-refused"}""", HttpStatusCode.Forbidden, json) }
        assertFalse(StorageClient("http://s", refused, null, FakeKeys).deleteBlob(blob))
    }

    @Test
    fun keep_sends_hex_ids_under_a_read_pass() = runTest {
        var pass: String? = null
        var body = ""
        val engine = MockEngine { req ->
            pass = req.headers[BLOB_PASS_HEADER]
            body = (req.body as io.ktor.http.content.OutgoingContent.ByteArrayContent).bytes().toString(Charsets.UTF_8)
            respond("", HttpStatusCode.NoContent)
        }
        StorageClient("http://s", engine, FakePasses(), FakeKeys).keep(listOf(blob))
        assertEquals("read-1", pass)
        assertEquals("{\"ids\":[\"${"01".repeat(32)}\"]}", body)
    }
}
