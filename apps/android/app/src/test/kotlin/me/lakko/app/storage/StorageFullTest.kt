// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.storage

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

/** docs/ux/storage-full.md — `507 storage-full` is its own refusal, never
 *  retried, and a backup 507 stays an ordinary failed upload. */
class StorageFullTest {
    private val json = headersOf("Content-Type", "application/json")
    private val blob = ByteArray(32) { 1 }
    private val full = """{"error":"storage-full"}"""

    private class CountingPasses : BlobPassSource {
        var issued = 0
        override suspend fun pass(kind: BlobPassKind, refresh: Boolean): String {
            issued += 1
            return "${kind.path}-$issued"
        }

        override fun refused(kind: BlobPassKind, value: String) = Unit
    }

    private object Keys : BlobDeleteKeys {
        override fun publicKey(blobId: ByteArray): ByteArray = ByteArray(32) { 7 }
        override fun deleteProof(blobId: ByteArray, path: String): Pair<String, String> = "x" to "y"
    }

    @Test
    fun a_507_storage_full_classifies_as_full_whatever_the_pass() {
        assertEquals(BlobRefusal.FULL, classifyBlobRefusal(507, full, renewed = false))
        assertEquals(BlobRefusal.FULL, classifyBlobRefusal(507, full, renewed = true))
        // Another 507 body is the generic failure.
        assertEquals(BlobRefusal.OTHER, classifyBlobRefusal(507, "<html>full</html>", renewed = true))
        assertEquals(BlobRefusal.OTHER, classifyBlobRefusal(500, full, renewed = true))
    }

    @Test
    fun a_put_refused_as_full_throws_once_without_renewing_the_pass() {
        var calls = 0
        val engine = MockEngine {
            calls += 1
            respond(full, HttpStatusCode(507, "Insufficient Storage"), json)
        }
        val passes = CountingPasses()
        assertThrows(StorageFullError::class.java) {
            runBlocking { StorageClient("http://s", engine, passes, Keys).putBlob(blob, byteArrayOf(1, 2, 3)) }
        }
        assertEquals(1, calls)
        assertEquals(1, passes.issued)
    }

    @Test
    fun a_put_without_a_pass_source_also_reports_full() {
        val engine = MockEngine { respond(full, HttpStatusCode(507, "Insufficient Storage"), json) }
        assertThrows(StorageFullError::class.java) {
            runBlocking { StorageClient("http://s", engine).putBlob(blob, byteArrayOf(1, 2, 3)) }
        }
    }

    @Test
    fun a_backup_507_is_an_ordinary_failed_upload() {
        val engine = MockEngine { respond("storage-full", HttpStatusCode(507, "Insufficient Storage")) }
        val err = assertThrows(StorageHttpError::class.java) {
            runBlocking { StorageClient("http://s", engine).putBackup(byteArrayOf(1)) }
        }
        assertEquals(507, err.status)
    }
}
