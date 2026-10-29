// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.SharedPreferences
import kotlinx.coroutines.delay
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * docs/ux/second-device.md section 2.1, "The place in the payload card": one
 * city request when the pairing screen opens, at most
 * [DeviceCardCenter.PAIRING_PLACE_WAIT_MS] of waiting before a code is built,
 * none with the anti-censorship connection armed or the switch off, and an
 * answer held for the pairing only when no account is bound.
 */
@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class PairingPlaceTest {

    private var locateCalls = 0
    private var answerAfterMs = 500L
    private val composed: MutableList<StoredDeviceCard> = java.util.Collections.synchronizedList(mutableListOf())
    private lateinit var originalCompose: (StoredDeviceCard) -> ByteArray?
    private lateinit var originalLocate: suspend () -> Pair<String, String>?
    private lateinit var originalArmed: () -> Boolean

    @Before
    fun hooks() {
        DeviceCardCenter.unbind()
        originalCompose = DeviceCardCenter.composeEnvelope
        originalLocate = DeviceCardCenter.locate
        originalArmed = DeviceCardCenter.antiCensorshipArmed
        DeviceCardCenter.composeEnvelope = { c -> composed += c; byteArrayOf(7) }
        DeviceCardCenter.antiCensorshipArmed = { false }
        DeviceCardCenter.platformFacts = { Triple("Pixel 7a", "Android 15", "1.4") }
        DeviceCardCenter.locate = {
            locateCalls += 1
            delay(answerAfterMs)
            "Almere" to "NL"
        }
        DeviceCardCenter.foreground = false
    }

    @After
    fun restore() {
        DeviceCardCenter.unbind()
        DeviceCardCenter.composeEnvelope = originalCompose
        DeviceCardCenter.locate = originalLocate
        DeviceCardCenter.antiCensorshipArmed = originalArmed
    }

    private fun TestScope.start(): DeviceCardCenter.PairingPlace =
        DeviceCardCenter.startPairingPlace(scope = backgroundScope, clock = { testScheduler.currentTime })

    private fun lastCard(place: Pair<String, String>?): StoredDeviceCard {
        composed.clear()
        DeviceCardCenter.pairingCardEnvelope(place, NOW)
        return composed.single()
    }

    @Test
    fun the_bound_is_one_and_a_half_seconds() {
        assertEquals(1_500L, DeviceCardCenter.PAIRING_PLACE_WAIT_MS)
    }

    @Test
    fun an_answer_within_the_bound_is_in_the_qr_card() = runTest {
        answerAfterMs = 500
        val place = start()
        assertTrue(place.requested)
        val settled = place.settled()
        assertEquals("Almere" to "NL", settled)
        assertEquals(500L, testScheduler.currentTime)
        assertEquals(1, locateCalls)
        val card = lastCard(settled)
        assertEquals("Almere", card.city)
        assertEquals("NL", card.country)
    }

    @Test
    fun no_answer_by_the_bound_gives_no_place_and_the_wait_ends_at_the_bound() = runTest {
        answerAfterMs = 4_000
        val place = start()
        val settled = place.settled()
        assertNull(settled)
        assertEquals(DeviceCardCenter.PAIRING_PLACE_WAIT_MS, testScheduler.currentTime)
        val card = lastCard(settled)
        assertEquals("", card.city)
        assertEquals("", card.country)
        assertEquals(1, locateCalls)
    }

    @Test
    fun the_code_route_seals_with_an_answer_that_arrived_after_the_bound() = runTest {
        answerAfterMs = 4_000
        val place = start()
        // The code is typed long after the screen opened.
        advanceTimeBy(30_000)
        runCurrent()
        assertEquals("Almere" to "NL", place.settled())
        assertEquals(30_000L, testScheduler.currentTime)
        assertEquals(1, locateCalls)
    }

    @Test
    fun the_code_route_waits_no_longer_than_the_bound_from_the_screen_opening() = runTest {
        answerAfterMs = 4_000
        val place = start()
        advanceTimeBy(1_000)
        assertNull(place.settled())
        assertEquals(DeviceCardCenter.PAIRING_PLACE_WAIT_MS, testScheduler.currentTime)
    }

    @Test
    fun armed_anti_censorship_asks_nothing_and_puts_no_place() = runTest {
        DeviceCardCenter.antiCensorshipArmed = { true }
        val place = start()
        assertFalse(place.requested)
        assertNull(place.settled())
        assertEquals(0L, testScheduler.currentTime)
        assertEquals(0, locateCalls)
        // Even a place handed in is kept out.
        assertEquals("", lastCard("Almere" to "NL").city)
    }

    @Test
    fun the_switch_off_asks_nothing_and_puts_no_place() = runTest {
        DeviceCardCenter.bindStorage(MemoryPrefs(), "acct") { }
        DeviceCardCenter.setShowCity(false)
        val place = start()
        assertFalse(place.requested)
        assertNull(place.settled())
        assertEquals(0, locateCalls)
        assertEquals("", lastCard("Almere" to "NL").city)
    }

    @Test
    fun an_unbound_device_asks_with_the_default_switch_and_keeps_the_answer_to_the_pairing() = runTest {
        answerAfterMs = 100
        val place = start()
        assertEquals("Almere" to "NL", place.settled())
        assertEquals(1, locateCalls)
        // Held for this pairing only: the center's own fields have no city.
        assertEquals("", DeviceCardCenter.currentFields(NOW).city)
    }

    @Test
    fun a_bound_device_with_a_fresh_city_asks_nothing() = runTest {
        val prefs = MemoryPrefs()
        prefs.map[DeviceCards.cityKey("acct")] =
            """{"city":"Utrecht","country":"NL","fetchedAtMs":1000000}"""
        DeviceCardCenter.bindStorage(prefs, "acct") { }
        val place = DeviceCardCenter.startPairingPlace(scope = backgroundScope, clock = { 1_060_000L + testScheduler.currentTime })
        assertFalse(place.requested)
        assertEquals("Utrecht" to "NL", place.settled())
        assertEquals(0, locateCalls)
        assertEquals(0L, testScheduler.currentTime)
    }

    @Test
    fun a_bound_device_with_a_stale_city_asks_once_and_keeps_the_answer_in_its_cache() = runTest {
        val prefs = MemoryPrefs()
        prefs.map[DeviceCards.cityKey("acct")] =
            """{"city":"Utrecht","country":"NL","fetchedAtMs":0}"""
        DeviceCardCenter.bindStorage(prefs, "acct") { }
        answerAfterMs = 200
        val place = DeviceCardCenter.startPairingPlace(scope = backgroundScope, clock = { 1_000_000_000L + testScheduler.currentTime })
        assertTrue(place.requested)
        assertEquals("Almere" to "NL", place.settled())
        assertEquals(1, locateCalls)
        assertEquals("Almere", DeviceCardCenter.currentFields(NOW).city)
        assertTrue((prefs.map[DeviceCards.cityKey("acct")] as String).contains("Almere"))
    }

    @Test
    fun a_bound_device_with_a_stale_city_and_no_answer_by_the_bound_keeps_its_cached_city() = runTest {
        val prefs = MemoryPrefs()
        prefs.map[DeviceCards.cityKey("acct")] =
            """{"city":"Utrecht","country":"NL","fetchedAtMs":0}"""
        DeviceCardCenter.bindStorage(prefs, "acct") { }
        answerAfterMs = 4_000
        val place = DeviceCardCenter.startPairingPlace(scope = backgroundScope, clock = { 1_000_000_000L + testScheduler.currentTime })
        assertTrue(place.requested)
        val settled = place.settled()
        assertEquals("Utrecht" to "NL", settled)
        assertEquals(DeviceCardCenter.PAIRING_PLACE_WAIT_MS, testScheduler.currentTime)
        assertEquals("Utrecht", lastCard(settled).city)
        assertEquals(1, locateCalls)
        // The code route, sealed after the answer arrived, takes the answer.
        advanceTimeBy(10_000)
        runCurrent()
        assertEquals("Almere" to "NL", place.settled())
    }

    @Test
    fun a_bound_device_with_a_stale_city_and_a_failed_locate_keeps_its_cached_city() = runTest {
        val prefs = MemoryPrefs()
        prefs.map[DeviceCards.cityKey("acct")] =
            """{"city":"Utrecht","country":"NL","fetchedAtMs":0}"""
        DeviceCardCenter.bindStorage(prefs, "acct") { }
        DeviceCardCenter.locate = {
            locateCalls += 1
            null
        }
        val place = DeviceCardCenter.startPairingPlace(scope = backgroundScope, clock = { 1_000_000_000L + testScheduler.currentTime })
        assertEquals("Utrecht" to "NL", place.settled())
        assertEquals(1, locateCalls)
    }

    @Test
    fun a_bound_device_with_no_cached_city_and_no_answer_by_the_bound_gives_no_place() = runTest {
        DeviceCardCenter.bindStorage(MemoryPrefs(), "acct") { }
        answerAfterMs = 4_000
        val place = start()
        assertNull(place.settled())
        assertEquals("", lastCard(null).city)
        assertEquals(1, locateCalls)
    }

    @Test
    fun a_bound_device_with_a_stale_city_and_armed_anti_censorship_puts_no_place() = runTest {
        val prefs = MemoryPrefs()
        prefs.map[DeviceCards.cityKey("acct")] =
            """{"city":"Utrecht","country":"NL","fetchedAtMs":0}"""
        DeviceCardCenter.bindStorage(prefs, "acct") { }
        DeviceCardCenter.antiCensorshipArmed = { true }
        val place = DeviceCardCenter.startPairingPlace(scope = backgroundScope, clock = { 1_000_000_000L + testScheduler.currentTime })
        assertFalse(place.requested)
        assertNull(place.settled())
        assertEquals(0, locateCalls)
    }

    @Test
    fun a_failed_locate_gives_no_place() = runTest {
        DeviceCardCenter.locate = {
            locateCalls += 1
            null
        }
        val place = start()
        assertNull(place.settled())
        assertEquals(1, locateCalls)
    }

    private companion object {
        const val NOW = 1_790_000_000_000L
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
