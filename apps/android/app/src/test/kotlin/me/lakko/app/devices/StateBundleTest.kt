// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0050 section 2.9 — mirror of `apps/web/src/devices/state-bundle.test.ts`. */
class StateBundleTest {

    private val bundle = StateBundle.Bundle(
        contacts = listOf(
            StateBundle.CarriedContact(
                signaturePubkeyB64 = "c2ln",
                displayName = "Bob",
                acceptedAtMs = 1_700_000_000_000L,
                accountIdHex = "00112233445566778899aabbccddeeff",
            ),
        ),
        pins = listOf(StateBundle.CarriedPin("ABEiM0RVZneImaq7zN3u/w==", "c2ln", 0L)),
        knownGroupIdsB64 = listOf("Z3JvdXA="),
    )

    @Test
    fun `round-trips`() {
        assertEquals(bundle, StateBundle.decode(StateBundle.encode(bundle)))
    }

    @Test
    fun `decodes the web encoding`() {
        // `JSON.stringify` of the web test fixture.
        val web = """{"v":1,"contacts":[{"signaturePubkeyB64":"c2ln","displayName":"Bob",""" +
            """"acceptedAtMs":1700000000000,"accountIdHex":"00112233445566778899aabbccddeeff"}],""" +
            """"pins":[{"accountIdB64":"ABEiM0RVZneImaq7zN3u/w==","pinnedSignaturePubkeyB64":"c2ln",""" +
            """"rotationMarker":0}],"knownGroupIdsB64":["Z3JvdXA="]}"""
        assertEquals(bundle, StateBundle.decode(web.toByteArray()))
    }

    @Test
    fun `refuses a bundle of another version or a malformed contact`() {
        val other = String(StateBundle.encode(bundle)).replace("\"v\":1", "\"v\":2")
        assertNull(StateBundle.decode(other.toByteArray()))
        val broken = """{"v":1,"contacts":[{"displayName":1}],"pins":[],"knownGroupIdsB64":[]}"""
        assertNull(StateBundle.decode(broken.toByteArray()))
        assertNull(StateBundle.decode(byteArrayOf(0x7b)))
    }

    @Test
    fun `tags the mailbox row so the other polls can skip it`() {
        val sealed = byteArrayOf(1, 2, 3, 4)
        val row = StateBundle.wrapRow(sealed)
        assertTrue(StateBundle.isStateRow(row))
        assertArrayEquals(sealed, StateBundle.unwrapRow(row))
        assertFalse(StateBundle.isStateRow(sealed))
        assertNull(StateBundle.unwrapRow(sealed))
        // The magic is the web's `"lakko/state-bundle-row/v1\0"`.
        assertArrayEquals("lakko/state-bundle-row/v1\u0000".toByteArray(), row.copyOfRange(0, 26))
    }
}
