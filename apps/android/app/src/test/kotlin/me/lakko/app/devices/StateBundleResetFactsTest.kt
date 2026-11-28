// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import java.util.Base64
import me.lakko.app.contacts.InMemoryResetLedgerStore
import me.lakko.app.contacts.ResetLedgerRow
import me.lakko.app.contacts.StartedOver
import me.lakko.app.contacts.load
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0101 alignment (2026-11-27): the pairing state bundle carries, per
 * contact, the highest seal-announcement number kept (`sealFloors`) and the
 * started-over records (`startedOver`). Additive: absent when empty, the same
 * JSON on web, iOS and Android. The receiving device keeps the higher floor
 * and adds a started-over record it does not have.
 */
class StateBundleResetFactsTest {

    private companion object {
        const val ID_A = "0102030405060708090a0b0c0d0e0f10"
        const val ID_B = "1112131415161718191a1b1c1d1e1f20"
        val KEY_OLD: String = Base64.getEncoder().encodeToString(ByteArray(32) { 0xA1.toByte() })
        val KEY_NEW: String = Base64.getEncoder().encodeToString(ByteArray(32) { 0xB2.toByte() })
        const val BASE =
            "\"v\":4,\"contacts\":[],\"pins\":[],\"knownGroupIdsB64\":[],\"ownPredecessors\":[],\"groupNames\":[]," +
                "\"profiles\":[],\"pendingContacts\":[],\"channelSeeds\":[]"

        fun b64OfHex(hex: String): String =
            Base64.getEncoder().encodeToString(ByteArray(hex.length / 2) { hex.substring(it * 2, it * 2 + 2).toInt(16).toByte() })
    }

    private fun bundle(floors: List<StateBundle.CarriedSealFloor>, over: List<StateBundle.CarriedStartedOver>) =
        StateBundle.Bundle(
            contacts = emptyList(),
            pins = emptyList(),
            knownGroupIdsB64 = emptyList(),
            sealFloors = floors,
            startedOver = over,
        )

    @Test
    fun `both arrays round-trip and are absent when empty`() {
        val floors = listOf(StateBundle.CarriedSealFloor(ID_A, 7))
        val over = listOf(StateBundle.CarriedStartedOver(ID_A, KEY_OLD, 777))
        val text = StateBundle.encode(bundle(floors, over)).toString(Charsets.UTF_8)
        assertTrue(text.contains("\"sealFloors\":[{\"accountIdHex\":\"$ID_A\",\"seq\":7}]"))
        assertTrue(text.contains("\"startedOver\":[{\"accountIdHex\":\"$ID_A\",\"oldKeyB64\":\"$KEY_OLD\",\"atMs\":777}]"))
        val back = StateBundle.decode(text.toByteArray())!!
        assertEquals(floors, back.sealFloors)
        assertEquals(over, back.startedOver)
        val empty = StateBundle.encode(bundle(emptyList(), emptyList())).toString(Charsets.UTF_8)
        assertTrue("absent when empty", !empty.contains("sealFloors") && !empty.contains("startedOver"))
    }

    @Test
    fun `the shared JSON decodes`() {
        val json = "{$BASE,\"sealFloors\":[{\"accountIdHex\":\"$ID_B\",\"seq\":3}]," +
            "\"startedOver\":[{\"accountIdHex\":\"$ID_B\",\"oldKeyB64\":\"$KEY_OLD\",\"atMs\":0}]}"
        val back = StateBundle.decode(json.toByteArray())
        assertNotNull(back)
        assertEquals(listOf(StateBundle.CarriedSealFloor(ID_B, 3)), back!!.sealFloors)
        assertEquals(listOf(StateBundle.CarriedStartedOver(ID_B, KEY_OLD, 0)), back.startedOver)
    }

    @Test
    fun `a malformed entry refuses the bundle`() {
        fun refused(extra: String) = assertNull(extra, StateBundle.decode("{$BASE,$extra}".toByteArray()))
        refused("\"sealFloors\":[{\"accountIdHex\":\"zz\",\"seq\":1}]")
        refused("\"sealFloors\":[{\"accountIdHex\":\"$ID_A\",\"seq\":-1}]")
        refused("\"sealFloors\":[{\"accountIdHex\":\"$ID_A\",\"seq\":1},{\"accountIdHex\":\"$ID_A\",\"seq\":2}]")
        refused("\"sealFloors\":{}")
        refused("\"startedOver\":[{\"accountIdHex\":\"$ID_A\",\"oldKeyB64\":\"AAAA\",\"atMs\":1}]")
        refused("\"startedOver\":[{\"accountIdHex\":\"$ID_A\",\"oldKeyB64\":\"$KEY_OLD\",\"atMs\":-5}]")
        refused(
            "\"startedOver\":[{\"accountIdHex\":\"$ID_A\",\"oldKeyB64\":\"$KEY_OLD\",\"atMs\":1}," +
                "{\"accountIdHex\":\"$ID_A\",\"oldKeyB64\":\"$KEY_OLD\",\"atMs\":2}]",
        )
    }

    @Test
    fun `the sender carries every floor above zero and every closed key`() {
        val rows = mapOf(
            b64OfHex(ID_B) to ResetLedgerRow(sealSeqFloor = 4),
            b64OfHex(ID_A) to ResetLedgerRow(
                sealSeqFloor = 0,
                startedOver = StartedOver(KEY_OLD, KEY_NEW, 2, 555),
                startedOverKeysB64 = listOf(KEY_OLD),
            ),
        )
        val (floors, over) = StateBundle.carriedResetFacts(rows)
        assertEquals(listOf(StateBundle.CarriedSealFloor(ID_B, 4)), floors)
        assertEquals(listOf(StateBundle.CarriedStartedOver(ID_A, KEY_OLD, 555)), over)
    }

    @Test
    fun `the receiver keeps the higher floor and adds a started-over key it does not have`() {
        val ledger = InMemoryResetLedgerStore()
        val a = b64OfHex(ID_A)
        val b = b64OfHex(ID_B)
        ledger.save("me", a, ResetLedgerRow(sealSeqFloor = 9))
        ledger.save("me", b, ResetLedgerRow(sealSeqFloor = 1, startedOverKeysB64 = listOf(KEY_NEW)))
        val changed = StateBundle.applyResetFacts(
            ledger,
            "me",
            listOf(StateBundle.CarriedSealFloor(ID_A, 3), StateBundle.CarriedSealFloor(ID_B, 6)),
            listOf(StateBundle.CarriedStartedOver(ID_B, KEY_OLD, 1), StateBundle.CarriedStartedOver(ID_B, KEY_NEW, 1)),
        )
        assertEquals(1, changed)
        // A lower floor never lowers what is kept.
        assertEquals(9L, ledger.load("me", a)?.sealSeqFloor)
        val rowB = ledger.load("me", b)!!
        assertEquals(6L, rowB.sealSeqFloor)
        assertEquals(listOf(KEY_NEW, KEY_OLD), rowB.startedOverKeysB64)
        // Nothing is opened for the account (no new key is known from a bundle).
        assertNull(rowB.startedOver)
        // Applying it again changes nothing.
        assertEquals(
            0,
            StateBundle.applyResetFacts(ledger, "me", listOf(StateBundle.CarriedSealFloor(ID_B, 6)), listOf(StateBundle.CarriedStartedOver(ID_B, KEY_OLD, 1))),
        )
    }
}
