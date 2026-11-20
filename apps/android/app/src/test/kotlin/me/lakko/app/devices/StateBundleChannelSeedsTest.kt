// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import java.util.Base64
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0050 section 2.9 amendment / docs/ux/second-device.md section 4a — the
 * pairing state bundle v4 carries the newest content-key seeds of each
 * channel the account's register lists as live, at most 32 per channel, and
 * the new device adds them without ever overwriting a seed it holds. Golden
 * from the contract: channel id 16 x 0xC1, epoch 41 = 32 x 0x01, epoch 42 =
 * 32 x 0x02, standard base64 with padding.
 */
class StateBundleChannelSeedsTest {

    private val enc = Base64.getEncoder()
    private val channelHex = "c1".repeat(16)
    private val channelB64 = "wcHBwcHBwcHBwcHBwcHBwQ=="
    private val seed41 = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE="
    private val seed42 = "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI="

    private fun wire(channelSeeds: String) =
        """{"v":4,"contacts":[],"pins":[],"knownGroupIdsB64":[],"ownPredecessors":[],"groupNames":[],""" +
            """"profiles":[],"pendingContacts":[],"channelSeeds":[$channelSeeds]}"""

    private val golden =
        """{"channelIdB64":"$channelB64","seeds":[{"epoch":41,"seedB64":"$seed41"},{"epoch":42,"seedB64":"$seed42"}]}"""

    private val empty = StateBundle.Bundle(contacts = emptyList(), pins = emptyList(), knownGroupIdsB64 = emptyList())

    @Test
    fun `decodes the golden entry the three clients agreed on`() {
        assertEquals(channelB64, enc.encodeToString(ByteArray(16) { 0xC1.toByte() }))
        assertEquals(seed41, enc.encodeToString(ByteArray(32) { 1 }))
        val bundle = StateBundle.decode(wire(golden).toByteArray())!!
        val entry = bundle.channelSeeds.single()
        assertEquals(channelB64, entry.channelIdB64)
        assertEquals(listOf(41L, 42L), entry.seeds.map { it.epoch })
        assertEquals(listOf(seed41, seed42), entry.seeds.map { it.seedB64 })
        assertEquals(bundle, StateBundle.decode(StateBundle.encode(bundle)))
    }

    @Test
    fun `the sender writes the golden entry from its seed map`() {
        val built = StateBundle.withChannelSeeds(
            empty,
            mapOf(channelHex to mapOf(42u to ByteArray(32) { 2 }, 41u to ByteArray(32) { 1 })),
        )
        val text = String(StateBundle.encode(built))
        assertTrue(text, text.contains("\"channelSeeds\":[$golden]"))
    }

    @Test
    fun `channelSeeds is required and the whole bundle is refused on any violation`() {
        val noField = wire("").replace(",\"channelSeeds\":[]", "")
        assertNull(StateBundle.decode(noField.toByteArray()))
        val one = """{"epoch":1,"seedB64":"$seed41"}"""
        val refused = listOf(
            // The same channel twice.
            "$golden,$golden",
            // No seeds, or more than 32.
            """{"channelIdB64":"$channelB64","seeds":[]}""",
            """{"channelIdB64":"$channelB64","seeds":[${(0 until 33).joinToString(",") { """{"epoch":$it,"seedB64":"$seed41"}""" }}]}""",
            // Epochs not strictly ascending, or out of range.
            """{"channelIdB64":"$channelB64","seeds":[{"epoch":42,"seedB64":"$seed42"},{"epoch":41,"seedB64":"$seed41"}]}""",
            """{"channelIdB64":"$channelB64","seeds":[$one,$one]}""",
            """{"channelIdB64":"$channelB64","seeds":[{"epoch":4294967296,"seedB64":"$seed41"}]}""",
            """{"channelIdB64":"$channelB64","seeds":[{"epoch":-1,"seedB64":"$seed41"}]}""",
            """{"channelIdB64":"$channelB64","seeds":[{"epoch":"1","seedB64":"$seed41"}]}""",
            // A seed or an id of the wrong length.
            """{"channelIdB64":"$channelB64","seeds":[{"epoch":1,"seedB64":"AQEB"}]}""",
            """{"channelIdB64":"AQEB","seeds":[$one]}""",
        )
        for (r in refused) assertNull(r.take(80), StateBundle.decode(wire(r).toByteArray()))
        val many = (0 until 1001).joinToString(",") { i ->
            val id = enc.encodeToString(ByteArray(16) { j -> if (j < 2) (i shr (8 * j)).toByte() else 0 })
            """{"channelIdB64":"$id","seeds":[$one]}"""
        }
        assertNull(StateBundle.decode(wire(many).toByteArray()))
        assertNotNull(StateBundle.decode(wire("""{"channelIdB64":"$channelB64","seeds":[{"epoch":4294967295,"seedB64":"$seed41"}]}""").toByteArray()))
    }

    @Test
    fun `the sender carries the newest 32 per channel, ascending, channels in id order`() {
        val many = (0u until 40u).associateWith { e -> ByteArray(32) { e.toByte() } }
        val entries = StateBundle.carriedChannelSeeds(mapOf("ff".repeat(16) to many, "00".repeat(16) to mapOf(5u to ByteArray(32))), 32)
        assertEquals(listOf("00".repeat(16), "ff".repeat(16)), entries.map { hex(Base64.getDecoder().decode(it.channelIdB64)) })
        assertEquals((8L until 40L).toList(), entries[1].seeds.map { it.epoch })
        // A malformed id or seed is skipped, a channel with nothing left too.
        assertTrue(StateBundle.carriedChannelSeeds(mapOf("zz" to many, "11".repeat(16) to mapOf(1u to ByteArray(3))), 32).isEmpty())
    }

    @Test
    fun `seeds shrink for every channel at once and never push out another array`() {
        val seeds = (0 until 20).associate { c ->
            "%02x".format(c).repeat(16) to (0u until 32u).associateWith { ByteArray(32) { 7 } }
        }
        val full = StateBundle.withChannelSeeds(empty, seeds)
        assertEquals(32, full.channelSeeds.first().seeds.size)
        val fullSize = StateBundle.encode(full).size
        // A cap the 32-per-channel bundle does not fit, the 16 one does.
        val sixteen = StateBundle.withChannelSeeds(empty, seeds, cap = fullSize - 1)
        assertTrue(sixteen.channelSeeds.all { it.seeds.size == 16 })
        val none = StateBundle.withChannelSeeds(empty, seeds, cap = StateBundle.encode(empty).size)
        assertTrue(none.channelSeeds.isEmpty())
        assertEquals(empty.copy(channelSeeds = emptyList()), none)
    }

    @Test
    fun `the receiver never overwrites a held seed`() {
        val carried = StateBundle.decode(wire(golden).toByteArray())!!.channelSeeds
        val add = StateBundle.channelSeedsToAdd(carried, mapOf(channelHex.uppercase() to mapOf(41u to ByteArray(32) { 9 })))
        assertEquals(setOf(42u), add[channelHex]!!.keys)
        assertArrayEquals(ByteArray(32) { 2 }, add[channelHex]!![42u])
        val all = StateBundle.channelSeedsCarried(carried)
        assertEquals(setOf(41u, 42u), all[channelHex]!!.keys)
    }

    @Test
    fun `a carried seed never prints`() {
        val s = StateBundle.CarriedChannelSeed(1L, seed41).toString()
        assertTrue(!s.contains(seed41))
    }

    private fun hex(b: ByteArray) = b.joinToString("") { "%02x".format(it) }
}
