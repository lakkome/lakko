// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.lakko_ffi_uniffi.composePushRoutingSecret
import uniffi.lakko_ffi_uniffi.decodePushRoutingSecret
import uniffi.lakko_ffi_uniffi.derivePushGid
import uniffi.lakko_ffi_uniffi.pushRoutingSecretIsNewer

/**
 * ADR-0042 / ADR-0044 — instrumented coverage of the REAL keyed-BLAKE3
 * `push_gid` derivation, the v2 distribution-envelope compose/decode
 * round-trip, and the canonical recency ORDER.
 *
 * These call `liblakko_ffi_uniffi.so` (`derivePushGid` /
 * `composePushRoutingSecret` / `decodePushRoutingSecret` /
 * `pushRoutingSecretIsNewer`), so they run inside the emulator / device
 * process, not the host JVM — the same host-vs-instrumented split as
 * [me.lakko.app.transparency.TransparencyAccountKeyInstrumentedTest]. The
 * host suite [GroupPushRoutingTest] covers the orchestration state machine
 * behind the [PushRoutingPorts] seam; this pins the native derivation to
 * the cross-platform golden vector shared with the Rust core golden
 * (`crates/lakko-contacts/src/push_routing.rs::derive_push_gid_golden`) +
 * the web/iOS clients, so every platform derives the SAME push_gid — and
 * pins that the REAL order this client routes every accept/refuse decision
 * through behaves as the host fake models it.
 */
@RunWith(AndroidJUnit4::class)
class GroupPushRoutingInstrumentedTest {

    /** The Rust golden secret: 0x42 * 32. */
    private val sampleSecret = ByteArray(32) { 0x42 }

    /** The cross-platform golden push_gid for [sampleSecret], pinned in
     *  `crates/lakko-contacts/src/push_routing.rs::derive_push_gid_golden`. */
    private val goldenPushGid = byteArrayOf(
        0x61, 0xf2.toByte(), 0xd2.toByte(), 0x24, 0xca.toByte(), 0x4d, 0x00, 0xe4.toByte(),
        0x32, 0x7d, 0x98.toByte(), 0x39, 0x02, 0x74, 0xa4.toByte(), 0x90.toByte(),
        0xdc.toByte(), 0x2d, 0x0f, 0x03, 0x21, 0x98.toByte(), 0xd0.toByte(), 0x3c,
        0xb7.toByte(), 0x27, 0x44, 0xeb.toByte(), 0xa4.toByte(), 0x01, 0x2a, 0x8a.toByte(),
    )

    @Test
    fun derivePushGidMatchesCrossPlatformGolden() {
        assertArrayEquals(goldenPushGid, derivePushGid(sampleSecret))
    }

    @Test
    fun derivePushGidIsDeterministicAndOneWay() {
        val a = derivePushGid(sampleSecret)
        val b = derivePushGid(sampleSecret)
        assertArrayEquals("same secret -> same push_gid", a, b)
        assertFalse("push_gid is not the secret echoed", a.contentEquals(sampleSecret))
    }

    @Test
    fun composeThenDecodeRoundTrips() {
        val secret = ByteArray(32) { (it * 7 + 3).toByte() }
        val envelope = composePushRoutingSecret(4242uL, secret)
        // ADR-0044 v2: 29-byte domain + 1 version byte + the 8-byte
        // big-endian mint_epoch + the 32-byte secret == 70.
        assertEquals(70, envelope.size)
        assertEquals("domain separator starts with 'l'", 0x6c, envelope[0].toInt() and 0xFF)
        assertTrue(GroupPushRouting.looksLikeSecretEnvelope(envelope))
        val decoded = decodePushRoutingSecret(envelope)
        assertArrayEquals(secret, decoded.secret)
        assertEquals("the MINTER's stamp survives the wire", 4242uL, decoded.mintEpoch)
    }

    @Test
    fun mintEpochRoundTripsAtTheExtremes() {
        // The stamp is a full u64 and Kotlin has no unsigned primitive on
        // the JVM, so a value past Long.MAX_VALUE is where a `toLong()`
        // shortcut anywhere in the binding would silently corrupt it — and
        // it is exactly the value an attacker stamps to attempt the
        // permanent freeze the client-side clamp refuses.
        val secret = ByteArray(32) { 0x11 }
        for (epoch in listOf(0uL, 1uL, ULong.MAX_VALUE)) {
            val decoded = decodePushRoutingSecret(composePushRoutingSecret(epoch, secret))
            assertEquals(epoch, decoded.mintEpoch)
            assertArrayEquals(secret, decoded.secret)
        }
    }

    @Test
    fun theRealOrderMatchesWhatTheHostSuiteModels() {
        // The canonical `(mintEpoch, secretBytes)` order, from the audited
        // Rust core. The host suite fakes it (the `.so` will not load on a
        // host JVM), so this is where the REAL predicate every accept /
        // refuse decision routes through is exercised.
        val low = ByteArray(32) { 0x11 }
        val high = ByteArray(32) { 0x99.toByte() }

        // Epoch first, and it DOMINATES the byte tiebreak.
        assertTrue(pushRoutingSecretIsNewer(11uL, low, 10uL, high))
        assertFalse(pushRoutingSecretIsNewer(10uL, high, 11uL, low))
        // Exact tie -> the 32 secret bytes decide, lexicographically.
        assertTrue(pushRoutingSecretIsNewer(5uL, high, 5uL, low))
        assertFalse(pushRoutingSecretIsNewer(5uL, low, 5uL, high))
        // Equal is NOT newer — what keeps a benign retry from reading as
        // an update, and what makes the accept-identical path safe.
        assertFalse(pushRoutingSecretIsNewer(5uL, low, 5uL, low))
        // Fail-closed on a wrong-length secret on either side.
        assertFalse(pushRoutingSecretIsNewer(99uL, ByteArray(31), 1uL, low))
        assertFalse(pushRoutingSecretIsNewer(99uL, low, 1uL, ByteArray(33)))
    }

    @Test
    fun ownerAndPeerDeriveTheSamePushGid() {
        // The publisher (owner) and a subscriber (peer) that both hold the
        // same secret derive the SAME push_gid — the notify JOIN property.
        val secret = ByteArray(32) { (0xA0 + it).toByte() }
        val envelope = composePushRoutingSecret(7uL, secret)
        val peerSecret = decodePushRoutingSecret(envelope).secret
        assertArrayEquals(derivePushGid(secret), derivePushGid(peerSecret))
    }

    @Test
    fun thePushGidIgnoresTheMintEpoch() {
        // C-60's stability property: the routing id has NO epoch input, so
        // a subscriber survives membership commits without re-subscribing.
        // The ADR-0044 stamp orders envelopes and must never reach the
        // derivation.
        val secret = ByteArray(32) { 0x42 }
        val early = decodePushRoutingSecret(composePushRoutingSecret(1uL, secret))
        val late = decodePushRoutingSecret(composePushRoutingSecret(ULong.MAX_VALUE, secret))
        assertArrayEquals(derivePushGid(early.secret), derivePushGid(late.secret))
    }

    @Test
    fun decodeIsFailClosedOnNonEnvelope() {
        // A plain chat JSON is not a push-routing envelope -> throws.
        val chat = "{\"v\":1,\"t\":\"hi\"}".toByteArray(Charsets.UTF_8)
        var threw = false
        try {
            decodePushRoutingSecret(chat)
        } catch (_: Throwable) {
            threw = true
        }
        assertTrue("non-envelope decode must fail closed", threw)
    }

    @Test
    fun aRetiredV1EnvelopeIsRefused() {
        // ADR-0044 is a HARD flag day: a v1 envelope (domain || 0x01 ||
        // secret, 62 bytes) carries no mint_epoch, so it has no position in
        // the order at all, and admitting one would re-open the very F-A
        // replay v2 closes. The refusal is fail-closed to DEFER — a member
        // never falls back to the leaky group_id — so invariant #4 holds
        // across the transition.
        // The 29-byte separator, NUL written as the Kotlin escape (a
        // RAW NUL in the source would make git classify this file as
        // binary and `grep -r` skip it silently -- the ADR-0042
        // MEDIUM-3 class).
        val domain = "lakko/push-routing-secret/v1\u0000".toByteArray(Charsets.UTF_8)
        val v1 = domain + byteArrayOf(0x01) + ByteArray(32) { 0x42 }
        assertEquals(62, v1.size)
        var threw = false
        try {
            decodePushRoutingSecret(v1)
        } catch (_: Throwable) {
            threw = true
        }
        assertTrue("a retired v1 envelope must be refused", threw)
    }
}
