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

    /** Genesis gate that always succeeds — returns normally, exactly the
     *  native `verifyLeafGenesis` contract on a valid self-sig (G-30 #2). */
    private val genesisPass = GenesisGate { _, _, _ -> }

    /** Genesis gate that always fails (throws as the native verifier would
     *  on a self-sig that does not match the served key). */
    private val genesisFail = GenesisGate { _, _, _ ->
        throw FfiException.Identity("simulated genesis failure")
    }

    /** Rotation chain gate that always succeeds — returns the served
     *  (authorised-new) pubkey, exactly the native `verifySupersessionChain`
     *  contract on a passing chain (G-30 #2). */
    private val chainPass = ChainGate { _, _, _, served, _, _, _, _, _, _, _ -> served }

    /** Rotation chain gate that always fails (throws as the native verifier
     *  would on a chain contradiction — bad root sig / rollback / inclusion /
     *  supersession proof / non-newer version / freshness). */
    private val chainFail = ChainGate { _, _, _, _, _, _, _, _, _, _, _ ->
        throw FfiException.Identity("simulated chain failure")
    }

    /** A fixed 145-byte supersession proof + an old pubkey. These never
     *  reach a real verifier in the host suite (the [ChainGate] seam is
     *  stubbed), so their bytes are arbitrary. */
    private val supersessionProof = ByteArray(145) { 0x07 }
    private val pinnedOldPubkey = ByteArray(32) { 0x42 }

    /** ADR-0039 — a pinned witness key SET + co-sig blob fixtures. The bytes
     *  never reach a real verifier (the [WitnessGate] seam is stubbed; the real
     *  K-of-N quorum verify is covered by the Rust `witness.rs` + FFI-crate
     *  `verify_witness_quorum` tests). */
    private val witnessKey = ByteArray(32) { 0x5A }
    private val witnessKey2 = ByteArray(32) { 0x5B }
    private val witnessKey3 = ByteArray(32) { 0x5C }
    /** A witness co-sig blob fixture — exactly 105 bytes (`version ‖
     *  tree_root(32) ‖ log_size(8) ‖ sig(64)`), so it survives the gate's
     *  length pre-filter. */
    private val cosigB64 = base64Encode(ByteArray(105) { 0x6C })
    /** A distinct 105-byte foreign/garbage co-sig blob — never inflates or
     *  sinks the distinct-valid tally in the real verifier; here it just rides
     *  the list handed to the gate. */
    private val garbageCosigB64 = base64Encode(ByteArray(105) { 0x99.toByte() })
    /** A malformed-LENGTH (not 105-byte) wire entry — the gate must DROP it
     *  before handing the cosigs to the quorum verifier (fail-safe). */
    private val shortCosigB64 = base64Encode(ByteArray(10) { 0x01 })

    /** Witness quorum gate that always succeeds (returns normally). */
    private val witnessPass = WitnessGate { _, _, _, _, _ -> }

    /** Witness quorum gate that always fails (throws as the native verifier
     *  would on a sub-threshold / invalid quorum). */
    private val witnessFail = WitnessGate { _, _, _, _, _ ->
        throw FfiException.Identity("simulated sub-threshold / invalid witness quorum")
    }

    // -----------------------------------------------------------------
    // Mock-engine JSON response builders (match the Rust handler shapes)
    // -----------------------------------------------------------------

    private val jsonHeaders = headersOf("Content-Type", "application/json")

    private fun rootJson(logSize: Long, witnessCosigsB64: List<String> = emptyList()): String =
        """{"tree_root_b64":"${base64Encode(ByteArray(32))}",""" +
            """"log_size":$logSize,"timestamp_unix":1700000000,""" +
            """"signature_b64":"${base64Encode(ByteArray(64))}"""" +
            (if (witnessCosigsB64.isEmpty()) {
                ""
            } else {
                ""","witness_cosigs_b64":[""" +
                    witnessCosigsB64.joinToString(",") { "\"$it\"" } + "]"
            }) +
            "}"

    private fun latestJson(
        pubkey: ByteArray,
        idx: Long = 3,
        version: Long = 2,
        isReset: Boolean = false,
        leafAuthB64: String? = null,
    ): String =
        """{"idx":$idx,"version":$version,""" +
            """"identity_pubkey_b64":"${base64Encode(pubkey)}","is_reset":$isReset""" +
            (leafAuthB64?.let { ""","leaf_auth_b64":"$it"""" } ?: "") +
            "}"

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
        latestLeafAuthB64: String? = null,
        rootWitnessCosigsB64: List<String> = emptyList(),
    ): TransparencyClient {
        val engine = MockEngine { request ->
            val path = request.url.encodedPath
            when {
                path.endsWith("/transparency/root") ->
                    if (rootServerError) {
                        respond("boom", HttpStatusCode.InternalServerError, jsonHeaders)
                    } else {
                        respond(
                            rootJson(rootLogSize, witnessCosigsB64 = rootWitnessCosigsB64),
                            HttpStatusCode.OK,
                            jsonHeaders,
                        )
                    }
                path.contains("/transparency/latest/") ->
                    if (latestStatus == HttpStatusCode.OK) {
                        respond(
                            latestJson(
                                latestPubkey,
                                isReset = latestIsReset,
                                leafAuthB64 = latestLeafAuthB64,
                            ),
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
        genesisGate: GenesisGate = genesisPass,
        witnessKeys: List<ByteArray> = emptyList(),
        witnessThreshold: Int = 1,
        witnessGate: WitnessGate = witnessPass,
    ): LogVerifyResult = verifyIdentityInLog(
        accountId = accountId,
        publishedSignaturePubkey = pubkey,
        client = client,
        operatorKey = operatorKey,
        lastSeenRoot = lastSeen,
        witnessKeys = witnessKeys,
        witnessThreshold = witnessThreshold,
        accountKeyDeriver = deriver,
        inclusionGate = gate,
        genesisGate = genesisGate,
        witnessGate = witnessGate,
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

    // G-30 #2 (ADR-0019) — first-observation genesis self-sig.

    @Test
    fun `genesis self-sig confirms when valid`() = runTest {
        val leafAuth = base64Encode(ByteArray(64) { 0x47 })
        assertEquals(
            LogVerifyStatus.CONFIRMED,
            verifyResult(
                client = mockClient(latestLeafAuthB64 = leafAuth),
                operatorKey = operatorKey,
                genesisGate = genesisPass,
            ).status,
        )
    }

    @Test
    fun `genesis self-sig is mismatch when verification fails`() = runTest {
        val leafAuth = base64Encode(ByteArray(64) { 0x47 })
        assertEquals(
            LogVerifyStatus.MISMATCH,
            verifyResult(
                client = mockClient(latestLeafAuthB64 = leafAuth),
                operatorKey = operatorKey,
                genesisGate = genesisFail,
            ).status,
        )
    }

    @Test
    fun `genesis check is skipped for a 145-byte rotation proof`() = runTest {
        // genesisFail would throw IF called; a 145-byte leaf_auth is the
        // rotation case (verified by the chain elsewhere), so the genesis gate
        // must NOT run — a clean inclusion stays CONFIRMED.
        val rotationProof = base64Encode(ByteArray(145) { 0x52 })
        assertEquals(
            LogVerifyStatus.CONFIRMED,
            verifyResult(
                client = mockClient(latestLeafAuthB64 = rotationProof),
                operatorKey = operatorKey,
                genesisGate = genesisFail,
            ).status,
        )
    }

    @Test
    fun `genesis check is skipped when leaf_auth is absent`() = runTest {
        assertEquals(
            LogVerifyStatus.CONFIRMED,
            verifyResult(
                client = mockClient(latestLeafAuthB64 = null),
                operatorKey = operatorKey,
                genesisGate = genesisFail,
            ).status,
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

    // -----------------------------------------------------------------
    // G-30 #2 (ADR-0019) — verifyRotationChain verdict mapping
    // -----------------------------------------------------------------

    private suspend fun rotation(
        client: TransparencyClient?,
        operatorKey: ByteArray?,
        lastSeen: LastSeenRootStore = InMemoryLastSeenRoot(),
        pubkey: ByteArray = servedPubkey,
        gate: ChainGate = chainPass,
        pinnedLogVersion: Long = 0L,
        witnessKeys: List<ByteArray> = emptyList(),
        witnessThreshold: Int = 1,
        witnessGate: WitnessGate = witnessPass,
    ): RotationChainResult = verifyRotationChain(
        accountId = accountId,
        servedPubkey = pubkey,
        supersessionProof = supersessionProof,
        pinnedOldPubkey = pinnedOldPubkey,
        pinnedLogVersion = pinnedLogVersion,
        client = client,
        operatorKey = operatorKey,
        lastSeenRoot = lastSeen,
        witnessKeys = witnessKeys,
        witnessThreshold = witnessThreshold,
        accountKeyDeriver = deriver,
        chainGate = gate,
        witnessGate = witnessGate,
    )

    @Test
    fun `rotation off when client is null`() = runTest {
        val r = rotation(client = null, operatorKey = operatorKey)
        assertEquals(LogVerifyStatus.OFF, r.status)
        assertNull(r.isReset)
        assertNull(r.version)
    }

    @Test
    fun `rotation off when operator key is null`() = runTest {
        val r = rotation(client = mockClient(), operatorKey = null)
        assertEquals(LogVerifyStatus.OFF, r.status)
        assertNull(r.version)
    }

    @Test
    fun `rotation confirmed carries the latest version on a passing chain`() = runTest {
        // mockClient's latest defaults to version 2.
        val r = rotation(
            client = mockClient(latestIsReset = true),
            operatorKey = operatorKey,
            gate = chainPass,
        )
        assertEquals(LogVerifyStatus.CONFIRMED, r.status)
        assertEquals(true, r.isReset)
        assertEquals(2L, r.version)
    }

    @Test
    fun `rotation mismatch (no throw) when the chain gate fails`() = runTest {
        // A chain contradiction is surfaced as MISMATCH, NOT a throw / block.
        val r = rotation(
            client = mockClient(latestIsReset = false),
            operatorKey = operatorKey,
            gate = chainFail,
        )
        assertEquals(LogVerifyStatus.MISMATCH, r.status)
        // A latest entry existed, so the marker rides out.
        assertEquals(false, r.isReset)
        // No chain confirmation — no version to anchor a floor on.
        assertNull(r.version)
    }

    @Test
    fun `rotation rollback when the signed root shrank below the high-water mark`() = runTest {
        val lastSeen = InMemoryLastSeenRoot()
        lastSeen.checkAndAdvance(100L)
        val r = rotation(
            client = mockClient(rootLogSize = 6),
            operatorKey = operatorKey,
            lastSeen = lastSeen,
        )
        assertEquals(LogVerifyStatus.ROLLBACK, r.status)
        assertNull(r.version)
    }

    @Test
    fun `rotation absent when latest 404s`() = runTest {
        val r = rotation(
            client = mockClient(latestStatus = HttpStatusCode.NotFound),
            operatorKey = operatorKey,
        )
        assertEquals(LogVerifyStatus.ABSENT, r.status)
        assertNull(r.version)
    }

    @Test
    fun `rotation absent (no block) when the log head is not the served key`() = runTest {
        // The log's latest pubkey is NOT the served key (a not-yet-logged
        // rotation or a third key). We cannot chain-prove the served key, so
        // we surface ABSENT — the standalone gate-1 result stands; no block.
        val r = rotation(
            client = mockClient(latestPubkey = otherPubkey, latestIsReset = true),
            operatorKey = operatorKey,
        )
        assertEquals(LogVerifyStatus.ABSENT, r.status)
        // The marker is read off the (divergent) latest entry.
        assertEquals(true, r.isReset)
        assertNull(r.version)
    }

    @Test
    fun `rotation unreachable on a server error fetching the root`() = runTest {
        val r = rotation(
            client = mockClient(rootServerError = true),
            operatorKey = operatorKey,
        )
        assertEquals(LogVerifyStatus.UNREACHABLE, r.status)
        assertNull(r.version)
    }

    @Test
    fun `a confirmed rotation advances the no-rollback high-water mark`() = runTest {
        val lastSeen = InMemoryLastSeenRoot()
        rotation(client = mockClient(rootLogSize = 6), operatorKey = operatorKey, lastSeen = lastSeen)
        assertEquals(6L, lastSeen.loadLastSeenLogSize())
    }

    // -----------------------------------------------------------------
    // ADR-0039 (G-30 finding-1) — K-of-N witness-quorum gate
    // -----------------------------------------------------------------

    /** Captured [WitnessGate] args, so a test can assert the whole cosig
     *  LIST + the pinned key SET + the threshold are threaded through. */
    private class CapturedWitness(
        val cosigs: List<ByteArray>,
        val keys: List<ByteArray>,
        val threshold: Int,
        val logSize: ULong,
    )

    @Test
    fun `witness gate off (empty key set) confirms a root without a co-sig`() = runTest {
        // No pinned witness keys → operator-only transition posture.
        val result = verifyResult(
            client = mockClient(rootWitnessCosigsB64 = emptyList()),
            operatorKey = operatorKey,
            witnessKeys = emptyList(),
        )
        assertEquals(LogVerifyStatus.CONFIRMED, result.status)
    }

    @Test
    fun `1-of-1 confirms and hands the gate the cosig LIST plus key SET plus threshold`() =
        runTest {
            var captured: CapturedWitness? = null
            val capturing = WitnessGate { cosigs, keys, threshold, _, logSize ->
                captured = CapturedWitness(cosigs, keys, threshold, logSize)
            }
            val result = verifyResult(
                client = mockClient(rootLogSize = 6, rootWitnessCosigsB64 = listOf(cosigB64)),
                operatorKey = operatorKey,
                witnessKeys = listOf(witnessKey),
                // threshold omitted → the helper default of 1
                witnessGate = capturing,
            )
            assertEquals(LogVerifyStatus.CONFIRMED, result.status)
            val c = captured!!
            assertEquals(1, c.cosigs.size)
            assertEquals(listOf(witnessKey.toList()), c.keys.map { it.toList() })
            assertEquals(1, c.threshold)
            assertEquals(6UL, c.logSize)
        }

    @Test
    fun `K-of-N quorum satisfied (at least K distinct valid) confirms`() = runTest {
        // 3 pinned witnesses, threshold 2, 3 co-sigs served. The seam simulates
        // a satisfied quorum (returns). Assert the threshold + full key set are
        // threaded through.
        var captured: CapturedWitness? = null
        val capturing = WitnessGate { cosigs, keys, threshold, _, logSize ->
            captured = CapturedWitness(cosigs, keys, threshold, logSize)
        }
        val result = verifyResult(
            client = mockClient(
                rootWitnessCosigsB64 = listOf(cosigB64, cosigB64, cosigB64),
            ),
            operatorKey = operatorKey,
            witnessKeys = listOf(witnessKey, witnessKey2, witnessKey3),
            witnessThreshold = 2,
            witnessGate = capturing,
        )
        assertEquals(LogVerifyStatus.CONFIRMED, result.status)
        val c = captured!!
        assertEquals(3, c.keys.size)
        assertEquals(2, c.threshold)
    }

    @Test
    fun `K-of-N sub-threshold fails closed (mismatch)`() = runTest {
        // The seam THROWS to simulate the native sub-threshold refusal
        // (K-1 valid < K). The gate must fail closed.
        val result = verifyResult(
            client = mockClient(rootWitnessCosigsB64 = listOf(cosigB64)),
            operatorKey = operatorKey,
            witnessKeys = listOf(witnessKey, witnessKey2, witnessKey3),
            witnessThreshold = 2,
            witnessGate = witnessFail,
        )
        assertEquals(LogVerifyStatus.MISMATCH, result.status)
        assertNull(result.isReset)
    }

    @Test
    fun `K-of-N garbage among valid still confirms (whole 105-byte list handed to gate)`() =
        runTest {
            // A foreign/garbage 105-byte co-sig rides BESIDE the valid ones. It
            // survives the length pre-filter (it is 105 bytes) so the whole list
            // is handed to the gate; the real verifier tallies DISTINCT pinned
            // keys, so garbage never sinks an otherwise-valid quorum.
            var captured: CapturedWitness? = null
            val capturing = WitnessGate { cosigs, _, _, _, _ ->
                captured = CapturedWitness(cosigs, emptyList(), 0, 0UL)
            }
            val result = verifyResult(
                client = mockClient(
                    rootWitnessCosigsB64 = listOf(garbageCosigB64, cosigB64, cosigB64),
                ),
                operatorKey = operatorKey,
                witnessKeys = listOf(witnessKey, witnessKey2),
                witnessThreshold = 2,
                witnessGate = capturing,
            )
            assertEquals(LogVerifyStatus.CONFIRMED, result.status)
            // All three 105-byte blobs (garbage included) reach the gate.
            assertEquals(3, captured!!.cosigs.size)
        }

    @Test
    fun `the gate DROPS a non-105-byte wire entry before the quorum verifier`() = runTest {
        // A malformed-length entry can never be a valid co-sig; the gate
        // pre-filters it out so it cannot sink an otherwise-valid quorum.
        var captured: CapturedWitness? = null
        val capturing = WitnessGate { cosigs, _, _, _, _ ->
            captured = CapturedWitness(cosigs, emptyList(), 0, 0UL)
        }
        val result = verifyResult(
            client = mockClient(
                rootWitnessCosigsB64 = listOf(shortCosigB64, cosigB64),
            ),
            operatorKey = operatorKey,
            witnessKeys = listOf(witnessKey),
            witnessGate = capturing,
        )
        assertEquals(LogVerifyStatus.CONFIRMED, result.status)
        // Only the exactly-105-byte entry survives the pre-filter.
        assertEquals(1, captured!!.cosigs.size)
        assertEquals(WITNESS_COSIG_BYTES, captured!!.cosigs[0].size)
    }

    @Test
    fun `witness gate fails closed when a pinned key set but no co-sig`() = runTest {
        // No co-sig served (0 distinct valid < threshold 1) → the native
        // verifier throws; the gate maps it to a fail-closed refusal.
        val result = verifyResult(
            client = mockClient(rootWitnessCosigsB64 = emptyList()),
            operatorKey = operatorKey,
            witnessKeys = listOf(witnessKey),
            witnessGate = witnessFail,
        )
        assertEquals(LogVerifyStatus.MISMATCH, result.status)
        assertNull(result.isReset)
    }

    @Test
    fun `witness gate fails closed when the quorum does not verify`() = runTest {
        val result = verifyResult(
            client = mockClient(rootWitnessCosigsB64 = listOf(cosigB64)),
            operatorKey = operatorKey,
            witnessKeys = listOf(witnessKey),
            witnessGate = witnessFail,
        )
        assertEquals(LogVerifyStatus.MISMATCH, result.status)
        assertNull(result.isReset)
    }

    @Test
    fun `witness quorum gate runs after the rollback gate`() = runTest {
        // A shrunk root is rollback regardless of the witness quorum.
        val lastSeen = InMemoryLastSeenRoot()
        lastSeen.checkAndAdvance(100L)
        val result = verifyResult(
            client = mockClient(rootLogSize = 6, rootWitnessCosigsB64 = listOf(cosigB64)),
            operatorKey = operatorKey,
            lastSeen = lastSeen,
            witnessKeys = listOf(witnessKey),
            witnessGate = witnessFail, // would refuse, but must not be reached
        )
        assertEquals(LogVerifyStatus.ROLLBACK, result.status)
    }

    @Test
    fun `rotation quorum satisfied proceeds (confirmed)`() = runTest {
        val r = rotation(
            client = mockClient(rootWitnessCosigsB64 = listOf(cosigB64, cosigB64)),
            operatorKey = operatorKey,
            witnessKeys = listOf(witnessKey, witnessKey2),
            witnessThreshold = 2,
            witnessGate = witnessPass,
        )
        assertEquals(LogVerifyStatus.CONFIRMED, r.status)
        assertEquals(2L, r.version)
    }

    @Test
    fun `rotation no pinned keys (empty set) leaves rotation behaviour unchanged`() = runTest {
        val r = rotation(
            client = mockClient(rootWitnessCosigsB64 = emptyList()),
            operatorKey = operatorKey,
            witnessKeys = emptyList(),
        )
        assertEquals(LogVerifyStatus.CONFIRMED, r.status)
        assertEquals(2L, r.version)
    }

    @Test
    fun `rotation witness gate fails closed without a co-sig`() = runTest {
        val r = rotation(
            client = mockClient(rootWitnessCosigsB64 = emptyList()),
            operatorKey = operatorKey,
            witnessKeys = listOf(witnessKey),
            witnessGate = witnessFail,
        )
        assertEquals(LogVerifyStatus.MISMATCH, r.status)
        assertNull(r.version)
    }

    @Test
    fun `rotation witness gate fails closed on a sub-threshold quorum`() = runTest {
        val r = rotation(
            client = mockClient(rootWitnessCosigsB64 = listOf(cosigB64)),
            operatorKey = operatorKey,
            witnessKeys = listOf(witnessKey, witnessKey2),
            witnessThreshold = 2,
            witnessGate = witnessFail,
        )
        assertEquals(LogVerifyStatus.MISMATCH, r.status)
        assertNull(r.version)
    }

    private companion object {
        /** RFC-fixed witness co-sig blob length (mirror of the gate's
         *  `WITNESS_COSIG_LEN`). */
        private const val WITNESS_COSIG_BYTES: Int = 105
    }
}
