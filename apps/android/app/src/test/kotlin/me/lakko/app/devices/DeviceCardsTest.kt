// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.SharedPreferences
import java.time.LocalDateTime
import java.time.ZoneId
import java.util.Locale
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0080 device cards: the receiver's store rules, the send rule, the
 * Devices screen's order and row text, and the center's persistence and
 * throttle, against an in-memory preferences store.
 */
class DeviceCardsTest {

    private val zone = ZoneId.of("Europe/Amsterdam")
    private val now = LocalDateTime.of(2026, 10, 14, 15, 0).atZone(zone).toInstant().toEpochMilli()

    private fun card(
        updatedAtMs: Long = now - 1000,
        lastActiveMs: Long = now - 1000,
        city: String = "Almere",
        country: String = "NL",
        model: String = "Pixel 7a",
        appVersion: String = "1.4",
        os: String = "Android 15",
        platform: DeviceCardPlatform = DeviceCardPlatform.ANDROID,
    ) = StoredDeviceCard(platform, model, appVersion, os, city, country, updatedAtMs, lastActiveMs)

    @After
    fun unbind() {
        DeviceCardCenter.unbind()
    }

    @Test
    fun keeps_a_card_from_the_roster_and_only_a_strictly_newer_one() {
        assertEquals(mapOf("aa" to card()), DeviceCards.receive(emptyMap(), "aa", card(), listOf("aa")))
        assertNull(DeviceCards.receive(emptyMap(), "cc", card(), listOf("aa")))
        val stored = mapOf("aa" to card(updatedAtMs = 10))
        assertNull(DeviceCards.receive(stored, "aa", card(updatedAtMs = 10, model = "X"), listOf("aa")))
        assertNull(DeviceCards.receive(stored, "aa", card(updatedAtMs = 9), listOf("aa")))
        assertEquals("X", DeviceCards.receive(stored, "aa", card(updatedAtMs = 11, model = "X"), listOf("aa"))?.get("aa")?.model)
    }

    @Test
    fun drops_the_card_of_a_leaf_that_left() {
        val stored = mapOf("aa" to card(), "bb" to card())
        assertEquals(mapOf("aa" to card()), DeviceCards.prune(stored, listOf("aa")))
        assertNull(DeviceCards.prune(stored, listOf("aa", "bb")))
    }

    @Test
    fun sends_with_none_yet_on_a_field_change_and_after_30_minutes() {
        val fields = card()
        assertTrue(DeviceCards.shouldSend(null, fields, now))
        val last = card(updatedAtMs = now - 60_000)
        assertFalse(DeviceCards.shouldSend(last, fields, now))
        assertTrue(DeviceCards.shouldSend(last, fields.copy(city = "Utrecht"), now))
        assertFalse(DeviceCards.shouldSend(card(updatedAtMs = now - DeviceCards.REFRESH_MS + 1), fields, now))
        assertTrue(DeviceCards.shouldSend(card(updatedAtMs = now - DeviceCards.REFRESH_MS), fields, now))
        assertTrue(DeviceCards.shouldSend(card(updatedAtMs = now + 5_000), fields, now))
        assertEquals(150L, DeviceCards.growthDelayMs(0.5))
        assertTrue(DeviceCards.growthDelayMs(1.0) < 300L)
        assertEquals(0L, DeviceCards.growthDelayMs(Double.NaN))
    }

    @Test
    fun orders_this_device_first_then_newest_activity_then_cardless() {
        val rows = listOf(
            DeviceCards.RowInput("old", 1u, false, card(lastActiveMs = 100)),
            DeviceCards.RowInput("none", 2u, false, null),
            DeviceCards.RowInput("self", 3u, true, card()),
            DeviceCards.RowInput("new", 4u, false, card(lastActiveMs = 200)),
            DeviceCards.RowInput("none0", 0u, false, null),
        )
        val (self, others) = DeviceCards.order(rows)
        assertEquals("self", self?.keyHex)
        assertEquals(listOf("new", "old", "none0", "none"), others.map { it.keyHex })
    }

    private val words = DeviceCards.Words(
        device = "Device",
        online = "online",
        app = { if (it.isEmpty()) "Lakko" else "Lakko $it" },
        place = { c, k -> "$c, $k" },
        countryName = { if (it == "NL") "Netherlands" else it },
    )

    private fun view(row: DeviceCards.RowInput) =
        DeviceCards.view(row, words, now, zone, use24h = true, locale = Locale.UK)

    @Test
    fun a_row_reads_like_an_active_session() {
        val v = view(DeviceCards.RowInput("aa", 0u, false, card()))
        assertEquals("Pixel 7a", v.title)
        assertEquals("Lakko 1.4 · Android 15", v.detail)
        assertEquals("Almere, Netherlands", v.place)
        assertEquals("14:59", v.activity)
        assertEquals(DeviceCards.IconKind.PHONE, v.icon)
        assertEquals("online", view(DeviceCards.RowInput("aa", 0u, true, card())).activity)
        assertEquals("Sun", view(DeviceCards.RowInput("aa", 0u, false, card(lastActiveMs = now - 3 * 86_400_000L))).activity)
        assertEquals("Netherlands", view(DeviceCards.RowInput("aa", 0u, false, card(city = ""))).place)
        assertEquals("", view(DeviceCards.RowInput("aa", 0u, false, card(city = "", country = ""))).place)
        val none = view(DeviceCards.RowInput("aa", 0u, false, null))
        assertEquals(DeviceCards.RowView("Device", "", "", "", DeviceCards.IconKind.UNKNOWN), none)
        val web = view(DeviceCards.RowInput("aa", 0u, false, card(platform = DeviceCardPlatform.WEB, appVersion = "web", os = "")))
        assertEquals("Lakko web", web.detail)
        assertEquals(DeviceCards.IconKind.BROWSER, web.icon)
    }

    @Test
    fun names_an_android_device_without_repeating_its_maker() {
        assertEquals("Google Pixel 7a", DeviceCards.androidModelName("Google", "Pixel 7a"))
        assertEquals("Samsung SM-S918B", DeviceCards.androidModelName("samsung", "SM-S918B"))
        assertEquals("OnePlus 12", DeviceCards.androidModelName("OnePlus", "OnePlus 12"))
        assertEquals("Pixel", DeviceCards.androidModelName("", "Pixel"))
    }

    @Test
    fun the_center_keeps_cards_per_account_and_throttles_its_own() = runBlocking {
        val prefs = MemoryPrefs()
        val sent = mutableListOf<ByteArray>()
        DeviceCardCenter.foreground = true
        DeviceCardCenter.antiCensorshipArmed = { false }
        DeviceCardCenter.locate = { "Almere" to "NL" }
        DeviceCardCenter.platformFacts = { Triple("Pixel 7a", "Android 15", "1.4") }
        DeviceCardCenter.bindStorage(prefs, "acct") { sent += it }
        DeviceCardCenter.receive("aa", card(), listOf("aa", "bb"))
        DeviceCardCenter.receive("cc", card(), listOf("aa", "bb"))
        assertEquals(setOf("aa"), DeviceCardCenter.cards.value.keys)

        // Composing the own card needs the native core; the throttle is what
        // is asserted here: one decision per 30 minutes.
        DeviceCardCenter.noteAction(now)
        withTimeout(5_000) { while (DeviceCardCenter.ownCard() == null) delay(10) }
        val first = DeviceCardCenter.ownCard()
        assertEquals("Almere", first?.city)
        assertEquals(now, first?.lastActiveMs)
        DeviceCardCenter.noteAction(now + 60_000)
        delay(100)
        assertEquals(first, DeviceCardCenter.ownCard())

        // Turning the city off is a field change: a fresh card at once.
        DeviceCardCenter.setShowCity(false)
        withTimeout(5_000) { while (DeviceCardCenter.ownCard()?.city != "") delay(10) }

        // Rebinding reads back what was stored; a leaf that left is dropped.
        DeviceCardCenter.unbind()
        DeviceCardCenter.bindStorage(prefs, "acct") { }
        assertEquals(setOf("aa"), DeviceCardCenter.cards.value.keys)
        assertFalse(DeviceCardCenter.showCity.value)
        DeviceCardCenter.prune(listOf("bb"))
        assertTrue(DeviceCardCenter.cards.value.isEmpty())
        // Another account sees nothing of this one.
        DeviceCardCenter.unbind()
        DeviceCardCenter.bindStorage(prefs, "other") { }
        assertTrue(DeviceCardCenter.cards.value.isEmpty())
        assertNull(DeviceCardCenter.ownCard())
    }

    /** Bind the center with a stand-in composer (the JVM suite has no native
     *  library) and [send] as the DeviceGroup send path; returns the prefs. */
    private fun bindForSending(send: suspend (ByteArray) -> Unit): MemoryPrefs {
        val prefs = MemoryPrefs()
        DeviceCardCenter.foreground = true
        DeviceCardCenter.antiCensorshipArmed = { false }
        DeviceCardCenter.locate = { null }
        DeviceCardCenter.platformFacts = { Triple("Pixel 7a", "Android 15", "1.4") }
        DeviceCardCenter.composeEnvelope = { byteArrayOf(1, 2, 3) }
        DeviceCardCenter.bindStorage(prefs, "acct", send)
        return prefs
    }

    private suspend fun until(condition: () -> Boolean) {
        withTimeout(5_000) { while (!condition()) delay(10) }
    }

    @Test
    fun a_failed_first_publish_stays_owed_and_goes_on_the_next_poll_pass() = runBlocking {
        val original = DeviceCardCenter.composeEnvelope
        try {
            var fail = true
            var sent = 0
            val prefs = bindForSending {
                if (fail) error("network") else sent += 1
            }
            DeviceCardCenter.noteAction(now)
            until { DeviceCardCenter.isOwed() }
            assertEquals(0, sent)
            // The owed flag is persisted: a restart keeps it owed.
            DeviceCardCenter.unbind()
            DeviceCardCenter.bindStorage(prefs, "acct") { if (fail) error("network") else sent += 1 }
            assertTrue(DeviceCardCenter.isOwed())
            // The next poll pass sends it.
            fail = false
            DeviceCardCenter.sendOwedIfAny()
            until { !DeviceCardCenter.isOwed() }
            assertEquals(1, sent)
            // Nothing is owed any more: a further pass sends nothing.
            DeviceCardCenter.sendOwedIfAny()
            delay(100)
            assertEquals(1, sent)
        } finally {
            DeviceCardCenter.composeEnvelope = original
        }
    }

    @Test
    fun a_growth_resend_that_comes_due_in_the_background_goes_at_the_next_foreground() = runBlocking {
        val original = DeviceCardCenter.composeEnvelope
        try {
            var sent = 0
            bindForSending { sent += 1 }
            DeviceCardCenter.noteAction(now)
            until { sent == 1 }
            DeviceCardCenter.foreground = false
            DeviceCardCenter.scheduleResend(random = 0.0)
            until { DeviceCardCenter.isOwed() }
            // Not from the background: neither the timer nor a poll pass sends.
            DeviceCardCenter.sendOwedIfAny()
            delay(100)
            assertEquals(1, sent)
            // The next foreground does.
            DeviceCardCenter.foreground = true
            until { sent == 2 }
            until { !DeviceCardCenter.isOwed() }
        } finally {
            DeviceCardCenter.composeEnvelope = original
        }
    }

    @Test
    fun a_local_add_hands_the_own_card_over_at_once() = runBlocking {
        val original = DeviceCardCenter.composeEnvelope
        try {
            var sent = 0
            bindForSending { sent += 1 }
            // No own card yet: nothing to hand over.
            DeviceCardCenter.sendHandOver()
            delay(100)
            assertEquals(0, sent)
            DeviceCardCenter.noteAction(now)
            until { sent == 1 }
            DeviceCardCenter.sendHandOver()
            until { sent == 2 }
        } finally {
            DeviceCardCenter.composeEnvelope = original
        }
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
