// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpStatusCode
import me.lakko.app.auth.base64Encode
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.IdentityFetchResponse
import me.lakko.app.ds.DsClient
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
    private fun clientWith(store: PinStore?): ContactsClient {
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

    @Test
    fun `null pin store disables the gate (direct-caller escape hatch)`() {
        // No store wired → the gate is skipped entirely. Must not
        // throw even on a "changed key, no proof" bundle.
        clientWith(null).use { client ->
            client.verifyBundleAccountKey(accountId, bundle(keyB, proofB64 = null))
        }
    }
}
