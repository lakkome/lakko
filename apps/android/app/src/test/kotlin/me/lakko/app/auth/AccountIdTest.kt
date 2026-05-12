// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test

/**
 * Host-side unit tests for [generateAccountId] + the base64 codec
 * pair. Run as `:app:testDebugUnitTest` — no emulator, no UniFFI
 * native library load required.
 *
 * The UniFFI-driven flows (auth + PoW) live next door in
 * [AuthClientTest]; these tests cover the pure-JVM helpers in
 * isolation so a regression in the version-7 layout, the
 * variant-bits placement, or the base64 alphabet surfaces in
 * the smallest possible failing test.
 */
class AccountIdTest {

    @Test
    fun `account id is 16 bytes`() {
        val id = generateAccountId()
        assertEquals(16, id.size)
    }

    @Test
    fun `account id encodes version 7 in byte 6 high nibble`() {
        val id = generateAccountId()
        // High nibble of byte 6 == 0x7 — RFC 9562 §5.7 version field.
        assertEquals(0x70, id[6].toInt() and 0xF0)
    }

    @Test
    fun `account id encodes RFC variant in byte 8 top two bits`() {
        val id = generateAccountId()
        // Top two bits of byte 8 == 0b10 — RFC variant.
        assertEquals(0x80, id[8].toInt() and 0xC0)
    }

    @Test
    fun `two consecutive ids differ`() {
        // Sanity check on the randomness path — 74 bits of entropy
        // make a collision astronomically unlikely even within the
        // same millisecond. If this fails we likely have a
        // `SecureRandom` wiring bug.
        val a = generateAccountId()
        val b = generateAccountId()
        assertNotEquals(a.toList(), b.toList())
    }

    @Test
    fun `base64 round trip is loss-free`() {
        val original = ByteArray(32) { (it * 7).toByte() }
        val encoded = base64Encode(original)
        val decoded = base64Decode(encoded)
        assertArrayEquals(original, decoded)
    }

    @Test
    fun `base64 standard alphabet has no line breaks`() {
        // 60 bytes encodes to 80 chars + padding — well over the
        // 76-char limit that some Java Base64 modes split on. We
        // need single-line output for the JSON body to be valid.
        val long = ByteArray(60) { it.toByte() }
        val encoded = base64Encode(long)
        assertEquals(-1, encoded.indexOf('\n'))
        assertEquals(-1, encoded.indexOf('\r'))
    }

    @Test
    fun `base64url no-pad has no padding chars`() {
        val original = ByteArray(7) { it.toByte() }
        val encoded = base64UrlEncodeNoPad(original)
        assertEquals(-1, encoded.indexOf('='))
        // base64url uses `-` and `_` instead of `+` and `/`.
        assertEquals(-1, encoded.indexOf('+'))
        assertEquals(-1, encoded.indexOf('/'))
        // Round-trips back to the original bytes.
        val decoded = base64UrlDecodeNoPad(encoded)
        assertArrayEquals(original, decoded)
    }
}
