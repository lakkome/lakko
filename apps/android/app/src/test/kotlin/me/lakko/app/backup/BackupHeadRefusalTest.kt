// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * Base scenario 4b: a device whose keys were reset elsewhere still holds an
 * access token storage accepts (storage does not ask auth). The backup-head
 * mint at auth is where that ends: its `401` stops the upload, with no
 * unframed legacy envelope written in its place.
 */
class BackupHeadRefusalTest {

    private fun headClient(status: HttpStatusCode, body: String) = BackupHeadClient(
        baseUrl = "http://test.invalid",
        engine = MockEngine {
            respond(body, status, headersOf(HttpHeaders.ContentType, "application/json"))
        },
    )

    private suspend fun mintFailure(status: HttpStatusCode, body: String): Throwable {
        headClient(status, body).use { client ->
            try {
                client.mint("v3.token", ByteArray(32))
            } catch (cause: Throwable) {
                return cause
            }
        }
        fail("expected the mint to fail")
        throw IllegalStateException()
    }

    @Test
    fun `a superseded session stops the upload`() = runTest {
        val cause = mintFailure(HttpStatusCode.Unauthorized, """{"error":"key-generation-superseded"}""")
        assertTrue(cause is BackupHeadSessionRefused)
        assertEquals("key-generation-superseded", (cause as BackupHeadSessionRefused).code)
        assertFalse(headMintFailureAllowsLegacy(cause))
    }

    @Test
    fun `a removed device or a session auth no longer accepts stops it too`() = runTest {
        assertFalse(headMintFailureAllowsLegacy(mintFailure(HttpStatusCode.Unauthorized, """{"error":"device-revoked"}""")))
        assertFalse(headMintFailureAllowsLegacy(mintFailure(HttpStatusCode.Unauthorized, "")))
    }

    @Test
    fun `a server fault or a missing token still writes the legacy envelope`() = runTest {
        assertTrue(headMintFailureAllowsLegacy(mintFailure(HttpStatusCode.ServiceUnavailable, "{}")))
        assertTrue(headMintFailureAllowsLegacy(mintFailure(HttpStatusCode.TooManyRequests, """{"error":"rate-limited"}""")))
        headClient(HttpStatusCode.OK, "{}").use { client ->
            try {
                client.mint(null, ByteArray(32))
                fail("expected no token to fail")
            } catch (cause: IllegalStateException) {
                assertTrue(headMintFailureAllowsLegacy(cause))
            }
        }
    }

    @Test
    fun `a minted head is read back`() = runTest {
        headClient(HttpStatusCode.OK, """{"backup_head_b64":"AQID","backup_version":7}""").use { client ->
            val minted = client.mint("v3.token", ByteArray(32))
            assertEquals(7uL, minted.version)
            assertEquals(3, minted.head.size)
        }
    }
}
