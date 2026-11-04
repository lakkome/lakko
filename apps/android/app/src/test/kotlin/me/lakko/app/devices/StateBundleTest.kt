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
    fun `decodes the v3 shape the three clients agreed on`() {
        // ADR-0074: `v` 2, `provenPredecessorsB64` on each pin and the
        // top-level `ownPredecessors`; field names as `JSON.stringify` on web
        // writes them.
        val seed = b64(ByteArray(64) { 9 })
        val old = b64(ByteArray(32) { 1 })
        val wire = """{"v":3,"contacts":[{"signaturePubkeyB64":"c2ln","displayName":"Bob",""" +
            """"acceptedAtMs":1700000000000,"accountIdHex":"00112233445566778899aabbccddeeff"}],""" +
            """"pins":[{"accountIdB64":"ABEiM0RVZneImaq7zN3u/w==","pinnedSignaturePubkeyB64":"c2ln",""" +
            """"rotationMarker":0,"provenPredecessorsB64":["$old"]}],"knownGroupIdsB64":["Z3JvdXA="],""" +
            """"ownPredecessors":[{"seedB64":"$seed","adoptedAtMs":1700000000123}],"groupNames":[],"profiles":[],"pendingContacts":[]}"""
        val expected = bundle.copy(
            pins = listOf(bundle.pins.single().copy(provenPredecessorsB64 = listOf(old))),
            ownPredecessors = listOf(StateBundle.CarriedOwnPredecessor(seed, 1_700_000_000_123L)),
        )
        assertEquals(expected, StateBundle.decode(wire.toByteArray()))
        assertEquals(expected, StateBundle.decode(StateBundle.encode(expected)))
    }

    @Test
    fun `an account that never rotated carries empty predecessor arrays`() {
        val text = String(StateBundle.encode(bundle))
        assertTrue(text.contains("\"v\":3"))
        assertTrue(text.contains("\"groupNames\":[]"))
        assertTrue(text.contains("\"profiles\":[]"))
        assertTrue(text.contains("\"pendingContacts\":[]"))
        assertTrue(text.contains("\"ownPredecessors\":[]"))
        assertTrue(text.contains("\"provenPredecessorsB64\":[]"))
    }

    @Test
    fun `refuses a bundle of another version or a malformed contact`() {
        // Flag day: v2 is refused, and so is anything that is not 3.
        for (v in listOf("1", "2", "4", "\"3\"", "3.5")) {
            val other = String(StateBundle.encode(bundle)).replace("\"v\":3", "\"v\":$v")
            assertNull("v=$v", StateBundle.decode(other.toByteArray()))
        }
        val v1 = """{"v":1,"contacts":[],"pins":[],"knownGroupIdsB64":[]}"""
        assertNull(StateBundle.decode(v1.toByteArray()))
        val broken = """{"v":3,"contacts":[{"displayName":1}],"pins":[],"knownGroupIdsB64":[],"ownPredecessors":[],"groupNames":[],"profiles":[],"pendingContacts":[]}"""
        assertNull(StateBundle.decode(broken.toByteArray()))
        assertNull(StateBundle.decode(byteArrayOf(0x7b)))
    }

    private fun own(vararg entries: String) =
        """{"v":3,"contacts":[],"pins":[],"knownGroupIdsB64":[],"ownPredecessors":[${entries.joinToString(",")}],"groupNames":[],"profiles":[],"pendingContacts":[]}"""

    private fun entry(seedB64: String, at: String = "5") = """{"seedB64":"$seedB64","adoptedAtMs":$at}"""

    @Test
    fun `the own predecessors are bounded and each bound refuses the whole bundle`() {
        val seed = b64(ByteArray(64) { 3 })
        assertEquals(8, StateBundle.decode(own(*Array(8) { entry(seed) }).toByteArray())!!.ownPredecessors.size)
        // More than eight.
        assertNull(StateBundle.decode(own(*Array(9) { entry(seed) }).toByteArray()))
        // A wrong seed length, and bytes that are not standard base64.
        assertNull(StateBundle.decode(own(entry(b64(ByteArray(63)))).toByteArray()))
        assertNull(StateBundle.decode(own(entry(b64(ByteArray(65)))).toByteArray()))
        assertNull(StateBundle.decode(own(entry("not base64!")).toByteArray()))
        // A time that is not an integer.
        assertNull(StateBundle.decode(own(entry(seed, "1.5")).toByteArray()))
        assertNull(StateBundle.decode(own(entry(seed, "\"5\"")).toByteArray()))
        assertNull(StateBundle.decode(own(entry(seed, "true")).toByteArray()))
        assertNull(StateBundle.decode(own("""{"seedB64":"$seed"}""").toByteArray()))
        // The field itself is part of the bundle.
        assertNull(StateBundle.decode("""{"v":3,"contacts":[],"pins":[],"knownGroupIdsB64":[],"groupNames":[],"profiles":[],"pendingContacts":[]}""".toByteArray()))
    }

    private fun pinWith(proven: String) =
        """{"v":3,"contacts":[],"pins":[{"accountIdB64":"AA==","pinnedSignaturePubkeyB64":"c2ln",""" +
            """"rotationMarker":0$proven}],"knownGroupIdsB64":[],"ownPredecessors":[],"groupNames":[],"profiles":[],"pendingContacts":[]}"""

    @Test
    fun `a pin's proven predecessors are bounded 32-byte keys`() {
        val key = b64(ByteArray(32) { 4 })
        fun list(n: Int, k: String = key) = ",\"provenPredecessorsB64\":[" + List(n) { "\"$k\"" }.joinToString(",") + "]"
        assertEquals(8, StateBundle.decode(pinWith(list(8)).toByteArray())!!.pins.single().provenPredecessorsB64.size)
        assertNull(StateBundle.decode(pinWith(list(9)).toByteArray()))
        assertNull(StateBundle.decode(pinWith(list(1, b64(ByteArray(31)))).toByteArray()))
        assertNull(StateBundle.decode(pinWith(",\"provenPredecessorsB64\":[7]").toByteArray()))
        assertNull(StateBundle.decode(pinWith("").toByteArray()))
    }

    @Test
    fun `a carried seed never prints`() {
        val carried = StateBundle.CarriedOwnPredecessor(b64(ByteArray(64) { 9 }), 1L)
        assertFalse(carried.toString().contains(carried.seedB64))
        assertFalse(bundle.copy(ownPredecessors = listOf(carried)).toString().contains(carried.seedB64))
    }

    private fun b64(bytes: ByteArray): String = java.util.Base64.getEncoder().encodeToString(bytes)

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
