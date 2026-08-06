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

/**
 * ADR-0042 — instrumented coverage of the REAL keyed-BLAKE3 `push_gid`
 * derivation + the distribution-envelope compose/decode round-trip.
 *
 * These call `liblakko_ffi_uniffi.so` (`derivePushGid` /
 * `composePushRoutingSecret` / `decodePushRoutingSecret`), so they run
 * inside the emulator / device process, not the host JVM — the same
 * host-vs-instrumented split as
 * [me.lakko.app.transparency.TransparencyAccountKeyInstrumentedTest]. The
 * host suite [GroupPushRoutingTest] covers the orchestration state machine
 * behind the [PushRoutingPorts] seam; this pins the native derivation to
 * the cross-platform golden vector shared with the Rust core golden
 * (`crates/lakko-contacts/src/push_routing.rs::derive_push_gid_golden`) +
 * the web/iOS clients, so every platform derives the SAME push_gid.
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
        val envelope = composePushRoutingSecret(secret)
        // 29-byte domain + 1 version byte + 32-byte secret == 62.
        assertEquals(62, envelope.size)
        assertEquals("domain separator starts with 'l'", 0x6c, envelope[0].toInt() and 0xFF)
        assertTrue(GroupPushRouting.looksLikeSecretEnvelope(envelope))
        assertArrayEquals(secret, decodePushRoutingSecret(envelope))
    }

    @Test
    fun ownerAndPeerDeriveTheSamePushGid() {
        // The publisher (owner) and a subscriber (peer) that both hold the
        // same secret derive the SAME push_gid — the notify JOIN property.
        val secret = ByteArray(32) { (0xA0 + it).toByte() }
        val envelope = composePushRoutingSecret(secret)
        val peerSecret = decodePushRoutingSecret(envelope)
        assertArrayEquals(derivePushGid(secret), derivePushGid(peerSecret))
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
}
