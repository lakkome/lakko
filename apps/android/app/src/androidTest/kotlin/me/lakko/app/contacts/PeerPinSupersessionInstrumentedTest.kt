// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import androidx.test.ext.junit.runners.AndroidJUnit4
import me.lakko.app.auth.base64Encode
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.IdentityFetchResponse
import me.lakko.app.ds.DsClient
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.signSupersessionProof

/**
 * G-4 (ADR-0016) — instrumented coverage of the FFI-bound branches
 * of [ContactsClient.verifyBundleAccountKey].
 *
 * These paths call the real `verifySupersession` (and we mint real
 * proofs with `signSupersessionProof`), which need
 * `liblakko_ffi_uniffi.so` — so they run inside the emulator /
 * device process, not the host JVM. The host-side suite
 * [PeerPinStoreTest] covers the FFI-free branches (TOFU /
 * unchanged / changed-absent) + the pin-store monotonic semantics.
 *
 * The decision table proven here (ADR-0016 rule #2):
 *   * changed key + a VALID proof authorising exactly the served
 *     key → re-pin (marker advanced to the proof's expiry).
 *   * changed key + a proof authorising a DIFFERENT key → refuse
 *     (`KeyMismatch`, defence in depth).
 *   * changed key + a proof whose embedded old key !=
 *     the pinned key → refuse (`ProofInvalid`).
 */
@RunWith(AndroidJUnit4::class)
class PeerPinSupersessionInstrumentedTest {

    private val accountId = ByteArray(16) { it.toByte() }
    private val accountIdB64 = base64Encode(accountId)

    /** Deterministic 32-byte export-key-shaped seed. */
    private fun seed(fill: Int): ByteArray = ByteArray(32) { fill.toByte() }

    private fun ed25519PubkeyB64(exportKey: ByteArray): String =
        Identity.fromSeed(exportKey).use { base64Encode(it.signaturePublicKey()) }

    private fun ed25519Seed(exportKey: ByteArray): ByteArray =
        Identity.fromSeed(exportKey).use { it.ed25519Seed() }

    private fun client(store: PinStore): ContactsClient =
        // `verifyBundleAccountKey` makes NO network call, so the
        // transport clients are never hit — the invalid base URLs
        // (real OkHttp engines) are constructed but unused. We don't
        // wire a MockEngine here because `ktor-client-mock` is a
        // `testImplementation`-only dep, not on the androidTest
        // classpath.
        ContactsClient(
            discovery = DiscoveryClient(baseUrl = "http://test.invalid"),
            ds = DsClient(baseUrl = "http://test.invalid"),
            pinStore = store,
        )

    private fun bundle(
        signaturePubkeyB64: String,
        proofB64: String?,
    ): IdentityFetchResponse = IdentityFetchResponse(
        signature_pubkey = signaturePubkeyB64,
        x25519_pubkey = base64Encode(ByteArray(32) { 0xCC.toByte() }),
        supersession_proof = proofB64,
    )

    @Test
    fun changedKeyWithValidProofRePinsAndAdvancesMarker() {
        val oldExport = seed(0x11)
        val newExport = seed(0x22)
        val oldPubB64 = ed25519PubkeyB64(oldExport)
        val newPubB64 = ed25519PubkeyB64(newExport)

        val store = InMemoryPinStore()
        store.setPin(accountIdB64, oldPubB64, 0L)

        val now = System.currentTimeMillis() / 1000L
        val expires = now + 14L * 24 * 60 * 60
        val proof = signSupersessionProof(
            ed25519Seed(oldExport),
            ed25519Seed(newExport),
            now,
            expires,
        )

        client(store).use { c ->
            c.verifyBundleAccountKey(accountId, bundle(newPubB64, base64Encode(proof)))
        }

        val pin = store.readPin(accountIdB64)
        assertEquals(newPubB64, pin?.pinnedSignaturePubkeyB64)
        // Marker advanced to the proof's own expires_at.
        assertEquals(expires, pin?.rotationMarker)
    }

    @Test
    fun changedKeyWithProofForADifferentKeyFailsClosed() {
        val oldExport = seed(0x11)
        val newExport = seed(0x22)
        val thirdExport = seed(0x33)
        val oldPubB64 = ed25519PubkeyB64(oldExport)
        val thirdPubB64 = ed25519PubkeyB64(thirdExport)

        val store = InMemoryPinStore()
        store.setPin(accountIdB64, oldPubB64, 0L)

        // A valid old->NEW proof, but the bundle serves a THIRD key.
        val now = System.currentTimeMillis() / 1000L
        val proof = signSupersessionProof(
            ed25519Seed(oldExport),
            ed25519Seed(newExport),
            now,
            now + 14L * 24 * 60 * 60,
        )

        client(store).use { c ->
            val ex = assertThrows(SupersessionRefused::class.java) {
                c.verifyBundleAccountKey(accountId, bundle(thirdPubB64, base64Encode(proof)))
            }
            assertEquals(SupersessionRefusedReason.KeyMismatch, ex.reason)
        }
        // Pin untouched — no downgrade.
        assertEquals(oldPubB64, store.readPin(accountIdB64)?.pinnedSignaturePubkeyB64)
    }

    @Test
    fun changedKeyWithProofFromWrongOldKeyFailsClosed() {
        // The pinned key is `pinned`, but the proof was signed by a
        // DIFFERENT old key (`imposterOld`) — the verifier rejects
        // the old-key mismatch and we refuse (ProofInvalid).
        val pinnedExport = seed(0x11)
        val imposterOldExport = seed(0x44)
        val newExport = seed(0x22)
        val pinnedPubB64 = ed25519PubkeyB64(pinnedExport)
        val newPubB64 = ed25519PubkeyB64(newExport)

        val store = InMemoryPinStore()
        store.setPin(accountIdB64, pinnedPubB64, 0L)

        val now = System.currentTimeMillis() / 1000L
        val proof = signSupersessionProof(
            ed25519Seed(imposterOldExport),
            ed25519Seed(newExport),
            now,
            now + 14L * 24 * 60 * 60,
        )

        client(store).use { c ->
            val ex = assertThrows(SupersessionRefused::class.java) {
                c.verifyBundleAccountKey(accountId, bundle(newPubB64, base64Encode(proof)))
            }
            assertEquals(SupersessionRefusedReason.ProofInvalid, ex.reason)
        }
        assertEquals(pinnedPubB64, store.readPin(accountIdB64)?.pinnedSignaturePubkeyB64)
    }
}
