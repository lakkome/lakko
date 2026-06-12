// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.transparency

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import me.lakko.app.auth.base64Encode
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiException

/**
 * Host-side unit tests for the KT-log S3e [verifyIdentityInLog] verdict
 * state machine. Mirror of the web `transparency` log-posture test.
 *
 * Run as `:app:testDebugUnitTest` — no emulator, no native lib. Both
 * native dependencies are injected behind seams:
 *
 *   * [AccountKeyDeriver] — replaces `transparencyAccountKey` with a fixed
 *     32-byte stub (the real BLAKE3 derivation needs `liblakko_ffi_uniffi.so`).
 *   * [InclusionGate]     — replaces `verifyLogInclusion`; it THROWS
 *     [FfiException] (a pure-Kotlin sealed class — no JNI to construct it)
 *     to simulate a proof failure and RETURNS to simulate success, exactly
 *     the native contract.
 *
 * The TransparencyClient itself runs over a `MockEngine`, so the
 * fetch→decode→verdict pipeline is exercised end-to-end without a network.
 * The native-lib-backed production path (`NativeAccountKeyDeriver` +
 * `NativeInclusionGate`, which compute a real account-key + verify a real
 * Merkle proof) is covered by the instrumented suite.
 */
class TransparencyVerifyTest {

    private val accountId = ByteArray(16) { it.toByte() }
    private val operatorKey = ByteArray(32) { 0x11 }
    private val servedPubkey = ByteArray(32) { 0xAB.toByte() }
    private val otherPubkey = ByteArray(32) { 0xCD.toByte() }
    private val usernameHash = ByteArray(32) { 0x55 }

    /** Fixed account-key seam — no native `transparencyAccountKey`. */
    private val deriver = AccountKeyDeriver { usernameHash }

    /** Inclusion gate that always succeeds (returns normally). */
    private val gatePass = InclusionGate { _, _, _, _, _, _ -> }

    /** Inclusion gate that always fails (throws as the native verifier
     *  would on a tampered proof / bad root signature). */
    private val gateFail = InclusionGate { _, _, _, _, _, _ ->
        throw FfiException.Identity("simulated inclusion failure")
    }

    // -----------------------------------------------------------------
    // Mock-engine JSON response builders (match the Rust handler shapes)
    // -----------------------------------------------------------------

    private val jsonHeaders = headersOf("Content-Type", "application/json")

    private fun rootJson(logSize: Long): String =
        """{"tree_root_b64":"${base64Encode(ByteArray(32))}",""" +
            """"log_size":$logSize,"timestamp_unix":1700000000,""" +
            """"signature_b64":"${base64Encode(ByteArray(64))}"}"""

    private fun latestJson(
        pubkey: ByteArray,
        idx: Long = 3,
        version: Long = 2,
        isReset: Boolean = false,
    ): String =
        """{"idx":$idx,"version":$version,""" +
            """"identity_pubkey_b64":"${base64Encode(pubkey)}","is_reset":$isReset}"""

    private fun proofJson(idx: Long = 3, logSize: Long = 6): String =
        """{"idx":$idx,"leaf_b64":"${base64Encode(ByteArray(8))}",""" +
            """"audit_path":["${base64Encode(ByteArray(32))}"],"log_size":$logSize}"""

    /**
     * Build a [TransparencyClient] over a [MockEngine] whose response is
     * chosen by the request path. `latestStatus` lets a test 404 the
     * `/latest` lookup; `errorEverything` lets a test force a transport
     * failure on `/root`.
     */
    private fun mockClient(
        rootLogSize: Long = 6,
        latestPubkey: ByteArray = servedPubkey,
        latestStatus: HttpStatusCode = HttpStatusCode.OK,
        rootServerError: Boolean = false,
        latestIsReset: Boolean = false,
    ): TransparencyClient {
        val engine = MockEngine { request ->
            val path = request.url.encodedPath
            when {
                path.endsWith("/transparency/root") ->
                    if (rootServerError) {
                        respond("boom", HttpStatusCode.InternalServerError, jsonHeaders)
                    } else {
                        respond(rootJson(rootLogSize), HttpStatusCode.OK, jsonHeaders)
                    }
                path.contains("/transparency/latest/") ->
                    if (latestStatus == HttpStatusCode.OK) {
                        respond(
                            latestJson(latestPubkey, isReset = latestIsReset),
                            HttpStatusCode.OK,
                            jsonHeaders,
                        )
                    } else {
                        respond("not found", latestStatus, jsonHeaders)
                    }
                path.contains("/transparency/proof/") ->
                    respond(proofJson(), HttpStatusCode.OK, jsonHeaders)
                else -> respond("not found", HttpStatusCode.NotFound, jsonHeaders)
            }
        }
        return TransparencyClient("http://transparency.test", engine)
    }

    private suspend fun verifyResult(
        client: TransparencyClient?,
        operatorKey: ByteArray?,
        lastSeen: LastSeenRootStore = InMemoryLastSeenRoot(),
        pubkey: ByteArray = servedPubkey,
        gate: InclusionGate = gatePass,
    ): LogVerifyResult = verifyIdentityInLog(
        accountId = accountId,
        publishedSignaturePubkey = pubkey,
        client = client,
        operatorKey = operatorKey,
        lastSeenRoot = lastSeen,
        accountKeyDeriver = deriver,
        inclusionGate = gate,
    )

    /** Status-only convenience wrapper for the existing verdict tests. */
    private suspend fun verify(
        client: TransparencyClient?,
        operatorKey: ByteArray?,
        lastSeen: LastSeenRootStore = InMemoryLastSeenRoot(),
        pubkey: ByteArray = servedPubkey,
        gate: InclusionGate = gatePass,
    ): LogVerifyStatus =
        verifyResult(client, operatorKey, lastSeen, pubkey, gate).status

    // -----------------------------------------------------------------
    // Verdict mapping
    // -----------------------------------------------------------------

    @Test
    fun `off when client is null`() = runTest {
        assertEquals(LogVerifyStatus.OFF, verify(client = null, operatorKey = operatorKey))
    }

    @Test
    fun `off when operator key is null`() = runTest {
        assertEquals(LogVerifyStatus.OFF, verify(client = mockClient(), operatorKey = null))
    }

    @Test
    fun `confirmed on signed root plus matching pubkey plus valid proof`() = runTest {
        assertEquals(
            LogVerifyStatus.CONFIRMED,
            verify(client = mockClient(), operatorKey = operatorKey, gate = gatePass),
        )
    }

    @Test
    fun `rollback when the signed root shrank below the high-water mark`() = runTest {
        val lastSeen = InMemoryLastSeenRoot()
        lastSeen.checkAndAdvance(100L) // we have seen a larger tree before
        assertEquals(
            LogVerifyStatus.ROLLBACK,
            verify(
                client = mockClient(rootLogSize = 6),
                operatorKey = operatorKey,
                lastSeen = lastSeen,
            ),
        )
    }

    @Test
    fun `absent when latest 404s`() = runTest {
        assertEquals(
            LogVerifyStatus.ABSENT,
            verify(
                client = mockClient(latestStatus = HttpStatusCode.NotFound),
                operatorKey = operatorKey,
            ),
        )
    }

    @Test
    fun `mismatch when the logged pubkey differs from what discovery served`() = runTest {
        assertEquals(
            LogVerifyStatus.MISMATCH,
            verify(
                client = mockClient(latestPubkey = otherPubkey),
                operatorKey = operatorKey,
            ),
        )
    }

    @Test
    fun `mismatch when the inclusion proof fails to verify`() = runTest {
        assertEquals(
            LogVerifyStatus.MISMATCH,
            verify(client = mockClient(), operatorKey = operatorKey, gate = gateFail),
        )
    }

    @Test
    fun `unreachable on a server error fetching the root`() = runTest {
        assertEquals(
            LogVerifyStatus.UNREACHABLE,
            verify(client = mockClient(rootServerError = true), operatorKey = operatorKey),
        )
    }

    // -----------------------------------------------------------------
    // The CONFIRMED path advances the no-rollback high-water mark
    // -----------------------------------------------------------------

    @Test
    fun `a confirmed verify advances the last-seen mark to the root log size`() = runTest {
        val lastSeen = InMemoryLastSeenRoot()
        verify(client = mockClient(rootLogSize = 6), operatorKey = operatorKey, lastSeen = lastSeen)
        assertEquals(6L, lastSeen.loadLastSeenLogSize())
    }

    // -----------------------------------------------------------------
    // KT-log S4 — the `is_reset` marker rides out on a read latest entry
    // -----------------------------------------------------------------

    @Test
    fun `confirmed carries the latest entry is_reset true`() = runTest {
        val result = verifyResult(
            client = mockClient(latestIsReset = true),
            operatorKey = operatorKey,
            gate = gatePass,
        )
        assertEquals(LogVerifyStatus.CONFIRMED, result.status)
        assertEquals(true, result.isReset)
    }

    @Test
    fun `confirmed carries the latest entry is_reset false`() = runTest {
        val result = verifyResult(
            client = mockClient(latestIsReset = false),
            operatorKey = operatorKey,
            gate = gatePass,
        )
        assertEquals(LogVerifyStatus.CONFIRMED, result.status)
        assertEquals(false, result.isReset)
    }

    @Test
    fun `mismatch on a divergent pubkey still carries the latest is_reset`() = runTest {
        // A latest entry EXISTS (its pubkey just differs from what was
        // served), so the marker rides out on the MISMATCH verdict.
        val result = verifyResult(
            client = mockClient(latestPubkey = otherPubkey, latestIsReset = true),
            operatorKey = operatorKey,
        )
        assertEquals(LogVerifyStatus.MISMATCH, result.status)
        assertEquals(true, result.isReset)
    }

    @Test
    fun `mismatch on a proof failure still carries the latest is_reset`() = runTest {
        val result = verifyResult(
            client = mockClient(latestIsReset = true),
            operatorKey = operatorKey,
            gate = gateFail,
        )
        assertEquals(LogVerifyStatus.MISMATCH, result.status)
        assertEquals(true, result.isReset)
    }

    @Test
    fun `absent has a null is_reset (no latest entry was read)`() = runTest {
        val result = verifyResult(
            client = mockClient(latestStatus = HttpStatusCode.NotFound),
            operatorKey = operatorKey,
        )
        assertEquals(LogVerifyStatus.ABSENT, result.status)
        assertNull(result.isReset)
    }

    @Test
    fun `off has a null is_reset`() = runTest {
        val result = verifyResult(client = null, operatorKey = operatorKey)
        assertEquals(LogVerifyStatus.OFF, result.status)
        assertNull(result.isReset)
    }

    @Test
    fun `unreachable has a null is_reset`() = runTest {
        val result = verifyResult(
            client = mockClient(rootServerError = true),
            operatorKey = operatorKey,
        )
        assertEquals(LogVerifyStatus.UNREACHABLE, result.status)
        assertNull(result.isReset)
    }

    @Test
    fun `rollback has a null is_reset (short-circuited before latest)`() = runTest {
        val lastSeen = InMemoryLastSeenRoot()
        lastSeen.checkAndAdvance(100L)
        val result = verifyResult(
            client = mockClient(rootLogSize = 6),
            operatorKey = operatorKey,
            lastSeen = lastSeen,
        )
        assertEquals(LogVerifyStatus.ROLLBACK, result.status)
        assertNull(result.isReset)
    }

    // -----------------------------------------------------------------
    // KT-log S4 — lookupServedKeyReset best-effort marker
    // -----------------------------------------------------------------

    private suspend fun lookup(
        client: TransparencyClient?,
        servedPubkey: ByteArray = this.servedPubkey,
    ): Boolean? = lookupServedKeyReset(
        accountId = accountId,
        servedSignaturePubkey = servedPubkey,
        client = client,
        accountKeyDeriver = deriver,
    )

    @Test
    fun `lookupServedKeyReset returns true when latest commits to the served key with reset`() =
        runTest {
            assertEquals(
                true,
                lookup(client = mockClient(latestPubkey = servedPubkey, latestIsReset = true)),
            )
        }

    @Test
    fun `lookupServedKeyReset returns false when latest commits to the served key without reset`() =
        runTest {
            assertEquals(
                false,
                lookup(client = mockClient(latestPubkey = servedPubkey, latestIsReset = false)),
            )
        }

    @Test
    fun `lookupServedKeyReset returns null when the log is off`() = runTest {
        assertNull(lookup(client = null))
    }

    @Test
    fun `lookupServedKeyReset returns null when the account is absent`() = runTest {
        assertNull(lookup(client = mockClient(latestStatus = HttpStatusCode.NotFound)))
    }

    @Test
    fun `lookupServedKeyReset returns null when latest commits to a different key`() = runTest {
        // The marker (even if true) describes some OTHER key, so it tells
        // us nothing about the served one — null.
        assertNull(
            lookup(
                client = mockClient(latestPubkey = otherPubkey, latestIsReset = true),
                servedPubkey = servedPubkey,
            ),
        )
    }
}
