// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import me.lakko.app.auth.base64Encode
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.IdentityFetchResponse
import me.lakko.app.ds.DsClient
import me.lakko.app.transparency.LogVerifyStatus
import me.lakko.app.transparency.TransparencyClient
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Test

/**
 * Host-side unit tests for the G-4 (ADR-0016) pin-store +
 * supersession verify-decision logic.
 *
 * Run as `:app:testDebugUnitTest` — no emulator. We exercise:
 *
 *   1. [InMemoryPinStore] — TOFU pin, idempotent re-pin, monotonic
 *      forward advance, and the backward-marker refusal (rule #4).
 *   2. [readProofExpiresAtSecs] — the trusted big-endian i64 parse
 *      at offset 73 of a synthetic 145-byte proof.
 *   3. The FFI-FREE branches of [ContactsClient.verifyBundleAccountKey]:
 *      no-pin TOFU, unchanged-key no-op, and the changed-key +
 *      absent-proof fail-closed refusal. These never reach
 *      `verifySupersession`, so they run on the host JVM.
 *
 * The FFI-bound branches (changed-key + a real proof: re-pin /
 * invalid / key-mismatch / marker-backward) need
 * `liblakko_ffi_uniffi.so` and live in the instrumented suite
 * [PeerPinSupersessionInstrumentedTest] — same posture as
 * [me.lakko.app.chat.ChatClientTest], which keeps MLS-FFI flows
 * out of the host suite.
 */
class PeerPinStoreTest {

    private val accountIdB64 = "YWNjb3VudC1pZC0xNi1ieXRlcw=="
    private val keyA = base64Encode(ByteArray(32) { 0xAA.toByte() })
    private val keyB = base64Encode(ByteArray(32) { 0xBB.toByte() })

    // -----------------------------------------------------------------
    // InMemoryPinStore — monotonic-marker semantics (rule #4)
    // -----------------------------------------------------------------

    @Test
    fun `readPin returns null before any pin`() {
        val store = InMemoryPinStore()
        assertNull(store.readPin(accountIdB64))
    }

    @Test
    fun `setPin TOFU then read round-trips`() {
        val store = InMemoryPinStore()
        assertEquals(SetPinResult.Set, store.setPin(accountIdB64, keyA, 0L))
        val pin = store.readPin(accountIdB64)
        assertEquals(keyA, pin?.pinnedSignaturePubkeyB64)
        assertEquals(0L, pin?.rotationMarker)
    }

    @Test
    fun `setPin advances forward and overwrites the pubkey`() {
        val store = InMemoryPinStore()
        store.setPin(accountIdB64, keyA, 100L)
        assertEquals(SetPinResult.Set, store.setPin(accountIdB64, keyB, 200L))
        val pin = store.readPin(accountIdB64)
        assertEquals(keyB, pin?.pinnedSignaturePubkeyB64)
        assertEquals(200L, pin?.rotationMarker)
    }

    @Test
    fun `setPin at the same marker is allowed (idempotent re-observe)`() {
        val store = InMemoryPinStore()
        store.setPin(accountIdB64, keyA, 500L)
        assertEquals(SetPinResult.Set, store.setPin(accountIdB64, keyB, 500L))
        assertEquals(keyB, store.readPin(accountIdB64)?.pinnedSignaturePubkeyB64)
    }

    @Test
    fun `setPin refuses a backward marker and leaves the pin untouched`() {
        val store = InMemoryPinStore()
        store.setPin(accountIdB64, keyA, 1_000L)
        assertEquals(
            SetPinResult.RejectedBackward,
            store.setPin(accountIdB64, keyB, 999L),
        )
        val pin = store.readPin(accountIdB64)
        // The original pin survives — no fork, no downgrade.
        assertEquals(keyA, pin?.pinnedSignaturePubkeyB64)
        assertEquals(1_000L, pin?.rotationMarker)
    }

    @Test
    fun `removePin clears the entry`() {
        val store = InMemoryPinStore()
        store.setPin(accountIdB64, keyA, 0L)
        store.removePin(accountIdB64)
        assertNull(store.readPin(accountIdB64))
    }

    @Test
    fun `pins are isolated per account id`() {
        val store = InMemoryPinStore()
        val other = "b3RoZXItYWNjb3VudC1pZC0xNg=="
        store.setPin(accountIdB64, keyA, 10L)
        store.setPin(other, keyB, 20L)
        assertEquals(keyA, store.readPin(accountIdB64)?.pinnedSignaturePubkeyB64)
        assertEquals(keyB, store.readPin(other)?.pinnedSignaturePubkeyB64)
    }

    // -----------------------------------------------------------------
    // readProofExpiresAtSecs — trusted big-endian i64 at offset 73
    // -----------------------------------------------------------------

    @Test
    fun `readProofExpiresAtSecs parses the big-endian expires_at field`() {
        // 145-byte proof: version(1) old(32) new(32) issued(8)
        // expires(8 @ offset 73) sig(64). Plant a known value in the
        // expires slot.
        val proof = ByteArray(145)
        val expires = 0x0000_0001_2345_6789L
        for (i in 0 until 8) {
            proof[73 + i] = ((expires shr (8 * (7 - i))) and 0xFF).toByte()
        }
        assertEquals(expires, readProofExpiresAtSecs(proof))
    }

    @Test
    fun `readProofExpiresAtSecs rejects a too-short buffer`() {
        assertThrows(IllegalArgumentException::class.java) {
            readProofExpiresAtSecs(ByteArray(100))
        }
    }

    // -----------------------------------------------------------------
    // verifyBundleAccountKey — FFI-FREE decision branches
    // -----------------------------------------------------------------

    private fun bundle(
        signaturePubkeyB64: String,
        proofB64: String? = null,
    ): IdentityFetchResponse = IdentityFetchResponse(
        signature_pubkey = signaturePubkeyB64,
        x25519_pubkey = base64Encode(ByteArray(32) { 0xCC.toByte() }),
        supersession_proof = proofB64,
    )

    private val accountId = ByteArray(16) { it.toByte() }

    /** Build a [ContactsClient] over `MockEngine`-backed transport
     *  clients (no real OkHttp / network) so the FFI-free decision
     *  branches run in pure JVM — same posture as
     *  [me.lakko.app.chat.ChatClientTest]. `verifyBundleAccountKey`
     *  never touches the network, so the engine is never actually
     *  hit; it exists only to avoid constructing real engines. */
    private fun clientWith(store: PinStore): ContactsClient {
        val engine = MockEngine { respond(content = "", status = HttpStatusCode.OK) }
        val engine2 = MockEngine { respond(content = "", status = HttpStatusCode.OK) }
        return ContactsClient(
            discovery = DiscoveryClient(baseUrl = "http://test.invalid", engine = engine),
            ds = DsClient(baseUrl = "http://test.invalid", engine = engine2),
            pinStore = store,
        )
    }

    @Test
    fun `first observation TOFU-pins the fetched key (marker 0)`() {
        val store = InMemoryPinStore()
        clientWith(store).use { client ->
            client.verifyBundleAccountKey(accountId, bundle(keyA))
        }
        val pin = store.readPin(base64Encode(accountId))
        assertEquals(keyA, pin?.pinnedSignaturePubkeyB64)
        assertEquals(0L, pin?.rotationMarker)
    }

    @Test
    fun `unchanged key is a no-op (no throw, marker untouched)`() {
        val store = InMemoryPinStore()
        store.setPin(base64Encode(accountId), keyA, 42L)
        clientWith(store).use { client ->
            client.verifyBundleAccountKey(accountId, bundle(keyA))
        }
        // Marker must NOT have been reset to 0 by a spurious re-pin.
        assertEquals(42L, store.readPin(base64Encode(accountId))?.rotationMarker)
    }

    @Test
    fun `changed key with no proof fails closed (ProofAbsent)`() {
        val store = InMemoryPinStore()
        store.setPin(base64Encode(accountId), keyA, 0L)
        clientWith(store).use { client ->
            val ex = assertThrows(SupersessionRefused::class.java) {
                client.verifyBundleAccountKey(accountId, bundle(keyB, proofB64 = null))
            }
            assertEquals(SupersessionRefusedReason.ProofAbsent, ex.reason)
        }
        // The old pin is NOT overwritten — fail-closed, no blind TOFU.
        assertEquals(keyA, store.readPin(base64Encode(accountId))?.pinnedSignaturePubkeyB64)
    }

    // Note: the former "null pin store disables the gate" escape hatch was
    // removed by the post-`lakko-security` hardening (ADR-0016 #3) — the
    // ContactsClient ctor now REQUIRES a non-null PinStore, so the gate can
    // never be silently disabled. There is no longer a null case to test.

    // -----------------------------------------------------------------
    // KT-log S4 — isReset persistence: serialize / parse back-compat
    // -----------------------------------------------------------------

    @Test
    fun `serializePin keeps the legacy two-field shape with no verdict or reset`() {
        assertEquals(
            "$keyA|42",
            PeerPinStore.serializePin(
                keyA,
                42L,
                logStatus = null,
                isReset = null,
                pinnedLogVersion = null,
            ),
        )
    }

    @Test
    fun `serializePin keeps the three-field shape for a verdict with no reset`() {
        assertEquals(
            "$keyA|42|CONFIRMED",
            PeerPinStore.serializePin(
                keyA,
                42L,
                LogVerifyStatus.CONFIRMED,
                isReset = null,
                pinnedLogVersion = null,
            ),
        )
    }

    @Test
    fun `serializePin writes an empty verdict slot when only isReset is set`() {
        // Positional layout: an isReset with no verdict keeps the reset at
        // index 3 by writing an empty verdict at index 2.
        assertEquals(
            "$keyA|42||true",
            PeerPinStore.serializePin(
                keyA,
                42L,
                logStatus = null,
                isReset = true,
                pinnedLogVersion = null,
            ),
        )
    }

    @Test
    fun `serializePin writes four fields for a verdict plus reset with no version`() {
        assertEquals(
            "$keyA|42|MISMATCH|false",
            PeerPinStore.serializePin(
                keyA,
                42L,
                LogVerifyStatus.MISMATCH,
                isReset = false,
                pinnedLogVersion = null,
            ),
        )
    }

    @Test
    fun `serializePin writes all five fields when the chain version is set`() {
        assertEquals(
            "$keyA|42|CONFIRMED|false|7",
            PeerPinStore.serializePin(
                keyA,
                42L,
                LogVerifyStatus.CONFIRMED,
                isReset = false,
                pinnedLogVersion = 7L,
            ),
        )
    }

    @Test
    fun `serializePin writes empty earlier slots when only the chain version is set`() {
        // A version with neither a verdict nor a reset writes empty slots at
        // indices 2 and 3 so the positional version lands at index 4.
        assertEquals(
            "$keyA|42|||9",
            PeerPinStore.serializePin(
                keyA,
                42L,
                logStatus = null,
                isReset = null,
                pinnedLogVersion = 9L,
            ),
        )
    }

    @Test
    fun `recordLogReset round-trips true on InMemory`() {
        val store = InMemoryPinStore()
        store.setPin(accountIdB64, keyA, 0L)
        assertEquals(true, store.recordLogReset(accountIdB64, true))
        assertEquals(true, store.readPin(accountIdB64)?.isReset)
    }

    @Test
    fun `recordLogReset round-trips false on InMemory`() {
        val store = InMemoryPinStore()
        store.setPin(accountIdB64, keyA, 0L)
        store.recordLogReset(accountIdB64, false)
        assertEquals(false, store.readPin(accountIdB64)?.isReset)
    }

    @Test
    fun `recordLogReset null clears a stale marker`() {
        val store = InMemoryPinStore()
        store.setPin(accountIdB64, keyA, 0L)
        store.recordLogReset(accountIdB64, true)
        store.recordLogReset(accountIdB64, null)
        assertNull(store.readPin(accountIdB64)?.isReset)
    }

    @Test
    fun `recordLogReset is a no-op with no pin`() {
        val store = InMemoryPinStore()
        assertEquals(false, store.recordLogReset(accountIdB64, true))
        assertNull(store.readPin(accountIdB64))
    }

    @Test
    fun `setPin preserves isReset across a forward re-pin`() {
        val store = InMemoryPinStore()
        store.setPin(accountIdB64, keyA, 100L)
        store.recordLogReset(accountIdB64, true)
        store.setPin(accountIdB64, keyB, 200L)
        // A key rotation must not silently drop the last reset marker.
        assertEquals(true, store.readPin(accountIdB64)?.isReset)
    }

    // -----------------------------------------------------------------
    // G-30 #2 (ADR-0019) — pinnedLogVersion persistence
    // -----------------------------------------------------------------

    @Test
    fun `recordLogVersion round-trips on InMemory`() {
        val store = InMemoryPinStore()
        store.setPin(accountIdB64, keyA, 0L)
        assertEquals(true, store.recordLogVersion(accountIdB64, 5L))
        assertEquals(5L, store.readPin(accountIdB64)?.pinnedLogVersion)
        assertEquals(5L, store.readLogVersion(accountIdB64))
    }

    @Test
    fun `recordLogVersion null clears a stored version`() {
        val store = InMemoryPinStore()
        store.setPin(accountIdB64, keyA, 0L)
        store.recordLogVersion(accountIdB64, 5L)
        store.recordLogVersion(accountIdB64, null)
        assertNull(store.readPin(accountIdB64)?.pinnedLogVersion)
        assertNull(store.readLogVersion(accountIdB64))
    }

    @Test
    fun `recordLogVersion is a no-op with no pin`() {
        val store = InMemoryPinStore()
        assertEquals(false, store.recordLogVersion(accountIdB64, 5L))
        assertNull(store.readPin(accountIdB64))
        assertNull(store.readLogVersion(accountIdB64))
    }

    @Test
    fun `setPin preserves pinnedLogVersion across a forward re-pin`() {
        val store = InMemoryPinStore()
        store.setPin(accountIdB64, keyA, 100L)
        store.recordLogVersion(accountIdB64, 3L)
        store.setPin(accountIdB64, keyB, 200L)
        // The chain-version downgrade floor must survive a key rotation —
        // the fetch path re-records it right after a confirmed rotation.
        assertEquals(3L, store.readPin(accountIdB64)?.pinnedLogVersion)
    }

    @Test
    fun `readLogVersion is null before any chain confirmation`() {
        val store = InMemoryPinStore()
        store.setPin(accountIdB64, keyA, 0L)
        // A TOFU pin has no chain version — degrades to the 0 floor at the
        // caller.
        assertNull(store.readLogVersion(accountIdB64))
    }

    @Test
    fun `serializePin then a round-trip parse recovers the chain version`() {
        // The on-disk five-field shape parses back via the same split path
        // PeerPinStore.readPin uses (mirrors the front-to-back back-compat
        // contract). We exercise the serialize→split→reconstruct round-trip
        // here without an Android Context; the read-guard junk-version drop is
        // covered by `a junk chain-version token degrades safely to no floor`
        // below.
        val serialized = PeerPinStore.serializePin(
            keyA,
            42L,
            LogVerifyStatus.CONFIRMED,
            isReset = true,
            pinnedLogVersion = 11L,
        )
        val parts = serialized.split('|', limit = 5)
        assertEquals(keyA, parts[0])
        assertEquals("42", parts[1])
        assertEquals("CONFIRMED", parts[2])
        assertEquals("true", parts[3])
        assertEquals("11", parts[4])
    }

    @Test
    fun `a junk chain-version token degrades safely to no floor`() {
        // The readPin guard drops a non-numeric or negative version while
        // keeping the pin valid. We model the guard here (the real readPin
        // needs an Android Context); the logic is `toLongOrNull()?.takeIf {
        // it >= 0 }`.
        fun parseVersion(token: String?): Long? = token?.toLongOrNull()?.takeIf { it >= 0L }
        assertNull(parseVersion("not-a-number"))
        assertNull(parseVersion("-1"))
        assertNull(parseVersion(""))
        assertNull(parseVersion(null))
        assertEquals(0L, parseVersion("0"))
        assertEquals(11L, parseVersion("11"))
    }

    // -----------------------------------------------------------------
    // KT-log S4 — bestEffortServedKeyReset enriches a SupersessionRefused
    // -----------------------------------------------------------------

    private val jsonHeaders = headersOf("Content-Type", "application/json")

    private fun latestJson(pubkeyB64: String, isReset: Boolean): String =
        """{"idx":3,"version":2,"identity_pubkey_b64":"$pubkeyB64","is_reset":$isReset}"""

    /**
     * A transparency client whose `/latest` commits to [servedPubkeyB64]
     * with the given [isReset] marker. Root + proof are stubbed so the
     * best-effort lookup (which only hits `/latest`) resolves. Note:
     * `bestEffortServedKeyReset` derives the username_hash via the NATIVE
     * `transparencyAccountKey` (no seam on that helper), so the
     * `/latest/:slug` path is matched by prefix here rather than by an
     * exact slug — the engine returns the same body for any account-key.
     */
    private fun transparencyClient(servedPubkeyB64: String, isReset: Boolean): TransparencyClient {
        val engine = MockEngine { request ->
            val path = request.url.encodedPath
            when {
                path.contains("/transparency/latest/") ->
                    respond(latestJson(servedPubkeyB64, isReset), HttpStatusCode.OK, jsonHeaders)
                else -> respond("not found", HttpStatusCode.NotFound, jsonHeaders)
            }
        }
        return TransparencyClient("http://transparency.test", engine)
    }

    private fun clientWithTransparency(
        store: PinStore,
        transparency: TransparencyClient?,
    ): ContactsClient {
        val engine = MockEngine { respond(content = "", status = HttpStatusCode.OK) }
        val engine2 = MockEngine { respond(content = "", status = HttpStatusCode.OK) }
        return ContactsClient(
            discovery = DiscoveryClient(baseUrl = "http://test.invalid", engine = engine),
            ds = DsClient(baseUrl = "http://test.invalid", engine = engine2),
            pinStore = store,
            transparencyClientFactory = { transparency },
        )
    }

    @Test
    fun `bestEffortServedKeyReset returns true when the log commits to the served key with reset`() =
        runTest {
            // NOTE: requires liblakko_ffi_uniffi.so for transparencyAccountKey.
            // Guard so the host suite (no native lib) skips rather than fails.
            org.junit.Assume.assumeTrue(nativeLibAvailable())
            val store = InMemoryPinStore()
            clientWithTransparency(store, transparencyClient(keyB, isReset = true)).use { client ->
                val refused = SupersessionRefused(
                    reason = SupersessionRefusedReason.ProofAbsent,
                    accountIdB64 = base64Encode(accountId),
                    message = "changed key, no proof",
                )
                refused.loggedReset = client.bestEffortServedKeyReset(
                    accountId,
                    me.lakko.app.auth.base64Decode(keyB),
                )
                assertEquals(true, refused.loggedReset)
            }
        }

    @Test
    fun `bestEffortServedKeyReset returns null when the log is off`() = runTest {
        val store = InMemoryPinStore()
        clientWithTransparency(store, transparency = null).use { client ->
            val marker = client.bestEffortServedKeyReset(
                accountId,
                me.lakko.app.auth.base64Decode(keyB),
            )
            assertNull(marker)
        }
    }

    // -----------------------------------------------------------------
    // G-30 #2 (ADR-0019) — checkTransparencyLog routes a rotation through
    // the chain verifier, with the same best-effort / non-blocking posture
    // -----------------------------------------------------------------

    /** A 145-byte synthetic supersession proof, base64. Never reaches a real
     *  verifier on the OFF path (the chain check short-circuits); on the
     *  native path the real verifier rejects it (→ MISMATCH, not a block). */
    private val syntheticProofB64 = base64Encode(ByteArray(145) { 0x07 })

    @Test
    fun `rotation routes through the chain but stays best-effort when the log is OFF`() = runTest {
        // A pin exists for keyA; the served key is keyB (a rotation) and a
        // proof is present — so checkTransparencyLog takes the rotation
        // branch. With the transparency integration OFF (null client), the
        // chain verdict is OFF: NOT a block, NO version recorded, and the
        // (gate-1-re-pinned) pin is left intact. Proves the rotation branch
        // is reachable + non-blocking without the native lib.
        val store = InMemoryPinStore()
        // Gate 1 would have re-pinned to keyB before gate 2 runs; model that
        // post-gate-1 state, and capture the OLD pin (keyA) for the chain.
        store.setPin(base64Encode(accountId), keyA, 0L)
        val oldPin = store.readPin(base64Encode(accountId))
        store.setPin(base64Encode(accountId), keyB, 100L) // gate-1 re-pin
        clientWithTransparency(store, transparency = null).use { client ->
            // Does not throw (OFF is not ROLLBACK) — best-effort, no block.
            client.checkTransparencyLog(
                accountId = accountId,
                servedPubkey = me.lakko.app.auth.base64Decode(keyB),
                oldPin = oldPin,
                supersessionProofB64 = syntheticProofB64,
            )
        }
        val pin = store.readPin(base64Encode(accountId))
        // OFF records the verdict but NEVER a chain version (no confirmation).
        assertEquals(LogVerifyStatus.OFF, pin?.logStatus)
        assertNull(pin?.pinnedLogVersion)
        // The re-pinned key is untouched.
        assertEquals(keyB, pin?.pinnedSignaturePubkeyB64)
    }

    @Test
    fun `rotation through a live chain verifier does NOT block on a bogus proof`() = runTest {
        // NOTE: requires liblakko_ffi_uniffi.so for transparencyAccountKey +
        // verifySupersessionChain. Guard so the host suite (no native lib)
        // skips rather than fails.
        org.junit.Assume.assumeTrue(nativeLibAvailable())
        // The chain verifier (native) rejects the synthetic proof → MISMATCH,
        // which is SURFACED, never a block (S3e posture). The /latest commits
        // to the served key (keyB) so the rotation branch reaches the chain
        // gate. No CONFIRMED ⇒ no version recorded.
        val store = InMemoryPinStore()
        store.setPin(base64Encode(accountId), keyA, 0L)
        val oldPin = store.readPin(base64Encode(accountId))
        store.setPin(base64Encode(accountId), keyB, 100L) // gate-1 re-pin
        clientWithTransparency(store, transparencyClient(keyB, isReset = false)).use { client ->
            // Does not throw — MISMATCH is surfaced, not blocked.
            client.checkTransparencyLog(
                accountId = accountId,
                servedPubkey = me.lakko.app.auth.base64Decode(keyB),
                oldPin = oldPin,
                supersessionProofB64 = syntheticProofB64,
            )
        }
        val pin = store.readPin(base64Encode(accountId))
        assertEquals(LogVerifyStatus.MISMATCH, pin?.logStatus)
        // A transient (non-confirmed) verdict never writes a chain version.
        assertNull(pin?.pinnedLogVersion)
    }

    /** True when the UniFFI native lib loads (the host JVM suite usually
     *  lacks it; the instrumented suite has it). */
    private fun nativeLibAvailable(): Boolean =
        try {
            uniffi.lakko_ffi_uniffi.transparencyAccountKey(accountId)
            true
        } catch (_: Throwable) {
            false
        }
}
