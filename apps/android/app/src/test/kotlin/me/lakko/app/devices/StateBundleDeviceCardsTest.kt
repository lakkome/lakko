// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.SharedPreferences
import java.util.Base64
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * ADR-0080 amendment (2026-11-23): the pairing state bundle carries the
 * confirming device's device cards. The shared vector is byte for byte the
 * JSON text the web (`state-bundle-device-cards.test.ts`) and iOS
 * (`DeviceCardsTests.swift`) suites decode. The host JVM has no native
 * library, so the envelopes are mapped to their cards by a stand-in decoder
 * here; web and iOS decode them with the core codec and get the same cards.
 */
class StateBundleDeviceCardsTest {

    private companion object {
        const val KEY_1 = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE="
        const val KEY_2 = "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI="
        const val ENVELOPE_IOS =
            "bGFra28vZGV2aWNlLWNhcmQvdjEAAQAAAYvP5WgAAAABi8/la+gBCmlQaG9uZTE4LDQDMS4wCGlPUyAyNi40AAA="
        const val ENVELOPE_WEB =
            "bGFra28vZGV2aWNlLWNhcmQvdjEAAQAAAYvP5W/QAAABi8/lc7gFD0Nocm9tZSBvbiBtYWNPUwN3ZWIFbWFjT1MGQWxtZXJlAk5M"
        val SHARED_VECTOR: String =
            "{\"v\":4,\"contacts\":[],\"pins\":[],\"knownGroupIdsB64\":[],\"ownPredecessors\":[],\"groupNames\":[]," +
                "\"profiles\":[],\"pendingContacts\":[],\"channelSeeds\":[],\"deviceCards\":[" +
                "{\"leafKeyB64\":\"$KEY_1\",\"envelopeB64\":\"$ENVELOPE_IOS\"}," +
                "{\"leafKeyB64\":\"$KEY_2\",\"envelopeB64\":\"$ENVELOPE_WEB\"}]}"
        val HEX_1 = "01".repeat(32)
        val HEX_2 = "02".repeat(32)
        val HEX_OWN = "03".repeat(32)
        val EXPECTED_IOS = StoredDeviceCard(
            DeviceCardPlatform.IOS, "iPhone18,4", "1.0", "iOS 26.4", "", "", 1_700_000_000_000, 1_700_000_001_000,
        )
        val EXPECTED_WEB = StoredDeviceCard(
            DeviceCardPlatform.WEB, "Chrome on macOS", "web", "macOS", "Almere", "NL",
            1_700_000_002_000, 1_700_000_003_000,
        )
    }

    /** The core decoder's answer for the two vector envelopes. */
    private val decode: (ByteArray) -> StoredDeviceCard? = { bytes ->
        when (Base64.getEncoder().encodeToString(bytes)) {
            ENVELOPE_IOS -> EXPECTED_IOS
            ENVELOPE_WEB -> EXPECTED_WEB
            else -> null
        }
    }

    @After
    fun unbind() {
        DeviceCardCenter.unbind()
    }

    @Test
    fun the_shared_vector_decodes_to_the_two_entries() {
        val bundle = StateBundle.decode(SHARED_VECTOR.toByteArray())
        assertNotNull(bundle)
        bundle!!
        assertEquals(
            listOf(
                StateBundle.CarriedDeviceCard(KEY_1, ENVELOPE_IOS),
                StateBundle.CarriedDeviceCard(KEY_2, ENVELOPE_WEB),
            ),
            bundle.deviceCards,
        )
        assertEquals(
            listOf(HEX_1 to EXPECTED_IOS, HEX_2 to EXPECTED_WEB),
            StateBundle.decodedDeviceCards(bundle.deviceCards, decode),
        )
    }

    @Test
    fun the_new_device_stores_the_cards_for_its_roster_only() {
        val bundle = StateBundle.decode(SHARED_VECTOR.toByteArray())!!
        val entries = StateBundle.decodedDeviceCards(bundle.deviceCards, decode)
        val prefs = MemoryPrefs()
        // Unbound (a restore route lands the session later): persisted.
        assertEquals(2, DeviceCardCenter.storeCarried(prefs, "acct", entries, listOf(HEX_1, HEX_2, HEX_OWN), HEX_OWN))
        DeviceCardCenter.bindStorage(prefs, "acct") { }
        assertEquals(mapOf(HEX_1 to EXPECTED_IOS, HEX_2 to EXPECTED_WEB), DeviceCardCenter.cards.value)
        DeviceCardCenter.unbind()
        // A key not in the roster is dropped, and never this device's own.
        assertEquals(1, DeviceCardCenter.storeCarried(prefs, "acct2", entries, listOf(HEX_1, HEX_2), HEX_2))
        DeviceCardCenter.bindStorage(prefs, "acct2") { }
        assertEquals(setOf(HEX_1), DeviceCardCenter.cards.value.keys)
        // Bound: a card that is not strictly newer changes nothing.
        assertEquals(0, DeviceCardCenter.storeCarried(prefs, "acct2", entries, listOf(HEX_1, HEX_2), HEX_2))
    }

    @Test
    fun only_a_strictly_newer_carried_card_replaces_a_held_one() {
        val newer = EXPECTED_IOS.copy(updatedAtMs = EXPECTED_IOS.updatedAtMs + 1, model = "newer")
        assertNull(DeviceCards.acceptCarried(mapOf(HEX_1 to newer), listOf(HEX_1 to EXPECTED_IOS), listOf(HEX_1), HEX_OWN))
        val seeded = EXPECTED_IOS.copy(updatedAtMs = 0)
        assertEquals(
            mapOf(HEX_1 to EXPECTED_IOS),
            DeviceCards.acceptCarried(mapOf(HEX_1 to seeded), listOf(HEX_1 to EXPECTED_IOS), listOf(HEX_1), HEX_OWN),
        )
    }

    @Test
    fun the_confirming_device_hands_over_its_own_card_and_the_roster_cards() {
        val prefs = MemoryPrefs()
        val original = DeviceCardCenter.composeEnvelope
        try {
            DeviceCardCenter.composeEnvelope = { card -> card.model.toByteArray().copyOf(44) }
            DeviceCardCenter.bindStorage(prefs, "acct") { }
            DeviceCardCenter.receive(HEX_1, EXPECTED_IOS, listOf(HEX_1, HEX_2, HEX_OWN))
            DeviceCardCenter.receive(HEX_2, EXPECTED_WEB, listOf(HEX_1, HEX_2, HEX_OWN))
            // The roster after the add no longer holds HEX_2: its card is not handed.
            val picked = DeviceCardCenter.cardsForStateBundle(prefs, "acct", HEX_OWN, listOf(HEX_1, HEX_OWN), 5L)
            assertEquals(listOf(HEX_OWN, HEX_1), picked.map { it.first })
            val carried = StateBundle.carriedDeviceCards(picked)
            assertEquals(listOf(KEY_1, Base64.getEncoder().encodeToString(ByteArray(32) { 3 })), carried.map { it.leafKeyB64 })
        } finally {
            DeviceCardCenter.composeEnvelope = original
        }
    }

    @Test
    fun the_codec_writes_no_empty_field_and_refuses_a_bad_list() {
        val empty = StateBundle.Bundle(emptyList(), emptyList(), emptyList())
        val text = StateBundle.encode(empty).toString(Charsets.UTF_8)
        assertFalse(text.contains("deviceCards"))
        assertEquals(emptyList<StateBundle.CarriedDeviceCard>(), StateBundle.decode(StateBundle.encode(empty))!!.deviceCards)

        val keys = (0 until 10).map { i -> "%02x".format(10 - i).repeat(32) }
        val envelope = Base64.getDecoder().decode(ENVELOPE_WEB)
        val carried = StateBundle.carriedDeviceCards((keys + keys[0]).map { it to envelope })
        assertEquals(StateBundle.MAX_DEVICE_CARDS, carried.size)
        assertEquals(Base64.getEncoder().encodeToString(ByteArray(32) { 1 }), carried.first().leafKeyB64)
        val roundTrip = StateBundle.decode(StateBundle.encode(empty.copy(deviceCards = carried)))!!
        assertEquals(carried, roundTrip.deviceCards)

        fun withCards(cards: String): ByteArray =
            (SHARED_VECTOR.substringBefore("\"deviceCards\":") + "\"deviceCards\":" + cards + "}").toByteArray()
        assertNotNull(StateBundle.decode(withCards("[]")))
        assertNull(StateBundle.decode(withCards("\"x\"")))
        assertNull(StateBundle.decode(withCards("[{\"leafKeyB64\":\"AQ==\",\"envelopeB64\":\"$ENVELOPE_IOS\"}]")))
        assertNull(StateBundle.decode(withCards("[{\"leafKeyB64\":\"$KEY_1\",\"envelopeB64\":\"AAAA\"}]")))
        assertNull(
            StateBundle.decode(
                withCards(
                    "[{\"leafKeyB64\":\"$KEY_1\",\"envelopeB64\":\"$ENVELOPE_IOS\"}," +
                        "{\"leafKeyB64\":\"$KEY_1\",\"envelopeB64\":\"$ENVELOPE_WEB\"}]",
                ),
            ),
        )
        val nine = (1..9).joinToString(",") { i ->
            "{\"leafKeyB64\":\"${Base64.getEncoder().encodeToString(ByteArray(32) { i.toByte() })}\"," +
                "\"envelopeB64\":\"$ENVELOPE_WEB\"}"
        }
        assertNull(StateBundle.decode(withCards("[$nine]")))
    }

    /** A minimal in-memory [SharedPreferences]. */
    private class MemoryPrefs : SharedPreferences {
        val map = HashMap<String, Any?>()
        override fun getAll(): MutableMap<String, *> = HashMap(map)
        override fun getString(key: String, defValue: String?): String? = map[key] as? String ?: defValue
        override fun getStringSet(key: String, defValues: MutableSet<String>?): MutableSet<String>? = defValues
        override fun getInt(key: String, defValue: Int): Int = map[key] as? Int ?: defValue
        override fun getLong(key: String, defValue: Long): Long = map[key] as? Long ?: defValue
        override fun getFloat(key: String, defValue: Float): Float = map[key] as? Float ?: defValue
        override fun getBoolean(key: String, defValue: Boolean): Boolean = map[key] as? Boolean ?: defValue
        override fun contains(key: String): Boolean = key in map
        override fun registerOnSharedPreferenceChangeListener(l: SharedPreferences.OnSharedPreferenceChangeListener) {}
        override fun unregisterOnSharedPreferenceChangeListener(l: SharedPreferences.OnSharedPreferenceChangeListener) {}
        override fun edit(): SharedPreferences.Editor = object : SharedPreferences.Editor {
            val pending = HashMap<String, Any?>()
            val removed = HashSet<String>()
            override fun putString(key: String, value: String?) = apply { pending[key] = value }
            override fun putStringSet(key: String, values: MutableSet<String>?) = apply { pending[key] = values }
            override fun putInt(key: String, value: Int) = apply { pending[key] = value }
            override fun putLong(key: String, value: Long) = apply { pending[key] = value }
            override fun putFloat(key: String, value: Float) = apply { pending[key] = value }
            override fun putBoolean(key: String, value: Boolean) = apply { pending[key] = value }
            override fun remove(key: String) = apply { removed += key }
            override fun clear() = apply { removed += map.keys }
            override fun commit(): Boolean {
                apply()
                return true
            }
            override fun apply() {
                removed.forEach { map.remove(it) }
                map.putAll(pending)
            }
        }
    }
}
