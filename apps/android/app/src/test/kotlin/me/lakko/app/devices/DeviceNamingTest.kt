// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.SharedPreferences
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import me.lakko.app.l10n.en
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * docs/ux/second-device.md section 2.1 and the "This device never waits for a
 * card" paragraph of section 3: the joining device's card in the pairing
 * payload, the signed-in device naming it on the confirm and in the result,
 * the payload card kept for the new leaf until the device's own card
 * arrives, this device's row built from its own fields, and a device that has
 * just joined sending its card at once.
 */
class DeviceNamingTest {

    private val now = 1_790_000_000_000L

    private fun card(
        model: String = "Pixel 7a",
        city: String = "Almere",
        country: String = "NL",
        updatedAtMs: Long = now - 1000,
    ) = StoredDeviceCard(DeviceCardPlatform.ANDROID, model, "1.4", "Android 15", city, country, updatedAtMs, now - 1000)

    private val words = DeviceCards.Words(
        device = "Device",
        online = "online",
        app = { if (it.isEmpty()) "Lakko" else "Lakko $it" },
        place = { c, k -> "$c, $k" },
        countryName = { if (it == "NL") "Netherlands" else it },
    )

    private lateinit var originalCompose: (StoredDeviceCard) -> ByteArray?
    private val composed: MutableList<StoredDeviceCard> = java.util.Collections.synchronizedList(mutableListOf())

    @Before
    fun hooks() {
        originalCompose = DeviceCardCenter.composeEnvelope
        DeviceCardCenter.composeEnvelope = { c -> composed += c; byteArrayOf(7) }
        DeviceCardCenter.antiCensorshipArmed = { false }
        DeviceCardCenter.platformFacts = { Triple("Pixel 7a", "Android 15", "1.4") }
        DeviceCardCenter.locate = { "Almere" to "NL" }
        DeviceCardCenter.foreground = true
        DeviceCardCenter.resetJoined()
    }

    @After
    fun restore() {
        DeviceCardCenter.unbind()
        DeviceCardCenter.resetJoined()
        DeviceCardCenter.composeEnvelope = originalCompose
    }

    private suspend fun until(condition: () -> Boolean) {
        withTimeout(5_000) { while (!condition()) delay(10) }
    }

    // ------------------------------------------------------------ item 1

    @Test
    fun the_payload_carries_the_card_this_device_would_send() = runBlocking {
        var sent = 0
        DeviceCardCenter.bindStorage(MemoryPrefs(), "acct") { sent += 1 }
        DeviceCardCenter.noteAction(now)
        until { sent == 1 }
        assertEquals("Almere", DeviceCardCenter.ownCard()?.city)

        composed.clear()
        val place = "Almere" to "NL"
        assertTrue(DeviceCardCenter.pairingCardEnvelope(place, now)?.contentEquals(byteArrayOf(7)) == true)
        assertEquals(DeviceCardCenter.currentFields(now), composed.single())
        assertEquals("Almere", composed.single().city)
        assertEquals("Pixel 7a", composed.single().model)

        // A null place is no place: the bound account's cached-city
        // fallback is decided by DeviceCardCenter.PairingPlace, not here.
        composed.clear()
        DeviceCardCenter.pairingCardEnvelope(null, now)
        assertEquals("", composed.single().city)
        assertEquals("", composed.single().country)

        // Armed anti-censorship: no place, as in the card itself.
        DeviceCardCenter.antiCensorshipArmed = { true }
        composed.clear()
        DeviceCardCenter.pairingCardEnvelope(place, now)
        assertEquals("", composed.single().city)
        assertEquals("", composed.single().country)
        DeviceCardCenter.antiCensorshipArmed = { false }

        // The city switch off: no place.
        DeviceCardCenter.setShowCity(false)
        until { sent == 2 }
        composed.clear()
        DeviceCardCenter.pairingCardEnvelope(place, now)
        assertEquals("", composed.single().city)
    }

    @Test
    fun a_card_that_cannot_be_composed_gives_no_card_never_a_failure() {
        DeviceCardCenter.composeEnvelope = { null }
        assertNull(DeviceCardCenter.pairingCardEnvelope("Almere" to "NL", now))
        DeviceCardCenter.composeEnvelope = { error("no native core") }
        assertNull(DeviceCardCenter.pairingCardEnvelope("Almere" to "NL", now))
        DeviceCardCenter.platformFacts = { error("no build facts") }
        assertNull(DeviceCardCenter.pairingCardEnvelope("Almere" to "NL", now))
    }

    @Test
    fun an_unbound_joining_device_puts_no_place_in_its_payload_card_without_an_answer() {
        DeviceCardCenter.pairingCardEnvelope(null, now)
        assertEquals("", composed.single().city)
        assertEquals("Pixel 7a", composed.single().model)
    }

    // ------------------------------------------------------------ item 2

    @Test
    fun the_confirm_names_the_device_and_shows_its_row_lines() {
        assertEquals("Link “Pixel 7a”?", DeviceCards.confirmTitle(card()).en)
        assertEquals(listOf("Lakko 1.4 · Android 15", "Almere, Netherlands"), DeviceCards.pairingLines(card(), words))
        assertEquals(listOf("Lakko 1.4 · Android 15"), DeviceCards.pairingLines(card(city = "", country = ""), words))
    }

    @Test
    fun the_confirm_without_a_card_keeps_todays_title_and_no_lines() {
        assertEquals("Link this device?", DeviceCards.confirmTitle(null).en)
        assertEquals(emptyList<String>(), DeviceCards.pairingLines(null, words))
        // A card with no name names nothing.
        assertEquals("Link this device?", DeviceCards.confirmTitle(card(model = "  ")).en)
    }

    @Test
    fun the_scan_route_returns_to_the_list_with_no_result_line() {
        // UX second-device.md section 2.1 (decided 2026-10-29): a device
        // added by a scan goes straight back to the Devices list; only a
        // hand-over without keys keeps its own text on screen.
        val notice = ScanLinkedNotice()
        notice.linked("acct-a", keysSent = true)
        assertNull(notice.keysNotSentFor.value)
        assertFalse(notice.showsKeysNotSent("acct-a"))
    }

    @Test
    fun a_scan_without_keys_leaves_the_warning_under_the_list_until_add_is_pressed() {
        // UX second-device.md section 2.1: only the added line goes; the
        // keys-not-sent warning stays under the Devices list, for that
        // account only, and Add a device clears it.
        val notice = ScanLinkedNotice()
        notice.linked("acct-a", keysSent = false)
        assertTrue(notice.showsKeysNotSent("acct-a"))
        assertFalse(notice.showsKeysNotSent("acct-b"))
        notice.clear()
        assertFalse(notice.showsKeysNotSent("acct-a"))
        // A later scan that sent the keys replaces an earlier warning.
        notice.linked("acct-a", keysSent = false)
        notice.linked("acct-a", keysSent = true)
        assertFalse(notice.showsKeysNotSent("acct-a"))
    }

    // ------------------------------------------------------------ item 3

    @Test
    fun the_payload_card_is_kept_at_the_lowest_stamp_unless_one_is_stored() {
        val kept = DeviceCards.pairingCard(emptyMap(), "aa", card())
        assertEquals(0L, kept?.get("aa")?.updatedAtMs)
        assertEquals("Pixel 7a", kept?.get("aa")?.model)
        assertNull(DeviceCards.pairingCard(emptyMap(), "aa", null))
        assertNull(DeviceCards.pairingCard(mapOf("aa" to card(model = "Real")), "aa", card()))
    }

    @Test
    fun the_new_devices_own_card_replaces_the_payload_card() {
        val prefs = MemoryPrefs()
        DeviceCardCenter.bindStorage(prefs, "acct") { }
        DeviceCardCenter.storePairingCard("acct", "aa", card(model = "Chrome on macOS"))
        assertEquals("Chrome on macOS", DeviceCardCenter.cards.value["aa"]?.model)
        // Kept across a restart.
        DeviceCardCenter.unbind()
        DeviceCardCenter.bindStorage(prefs, "acct") { }
        assertEquals("Chrome on macOS", DeviceCardCenter.cards.value["aa"]?.model)
        // The device's own card, at any real stamp, wins.
        DeviceCardCenter.receive("aa", card(model = "Chrome on macOS", city = "Utrecht", updatedAtMs = 1), listOf("aa"))
        assertEquals("Utrecht", DeviceCardCenter.cards.value["aa"]?.city)
        // A later pairing card never overwrites a stored card.
        DeviceCardCenter.storePairingCard("acct", "aa", card(model = "Other"))
        assertEquals("Chrome on macOS", DeviceCardCenter.cards.value["aa"]?.model)
        assertEquals("Utrecht", DeviceCardCenter.cards.value["aa"]?.city)
    }

    @Test
    fun a_payload_card_for_another_account_is_not_stored() {
        DeviceCardCenter.bindStorage(MemoryPrefs(), "acct") { }
        DeviceCardCenter.storePairingCard("other", "aa", card())
        assertTrue(DeviceCardCenter.cards.value.isEmpty())
        DeviceCardCenter.storePairingCard("acct", "aa", null)
        assertTrue(DeviceCardCenter.cards.value.isEmpty())
    }

    // ------------------------------------------------------------ item 4

    @Test
    fun this_devices_row_is_built_from_its_own_fields() {
        val own = DeviceCardCenter.currentFields(now)
        // Its key holds no received card (its own card never comes back).
        val cards = mapOf("bb" to card(model = "iPhone 16 Pro"))
        val self = DeviceCards.cardFor(isSelf = true, keyHex = "aa", cards = cards, ownFields = own)
        assertEquals(own, self)
        val row = DeviceCards.view(
            DeviceCards.RowInput("aa", 0u, true, self),
            words,
            now,
            java.time.ZoneId.of("UTC"),
            use24h = true,
            locale = java.util.Locale.UK,
        )
        assertEquals("Pixel 7a", row.title)
        assertEquals("Lakko 1.4 · Android 15", row.detail)
        assertEquals("online", row.activity)
        // Another device still reads its received card.
        assertEquals("iPhone 16 Pro", DeviceCards.cardFor(false, "bb", cards, own)?.model)
        assertNull(DeviceCards.cardFor(false, "cc", cards, own))
    }

    // ------------------------------------------------------------ item 5

    @Test
    fun a_device_that_joins_before_the_bind_sends_its_card_when_bound() = runBlocking {
        var sent = 0
        DeviceCardCenter.noteJoined("acct")
        assertTrue(DeviceCardCenter.isJoinedOwed())
        // Not the person's next action and no growth wait: the bind sends.
        DeviceCardCenter.foreground = false
        DeviceCardCenter.bindStorage(MemoryPrefs(), "acct") { sent += 1 }
        until { sent == 1 }
        assertFalse(DeviceCardCenter.isJoinedOwed())
        assertEquals("Pixel 7a", DeviceCardCenter.ownCard()?.model)
        assertFalse(DeviceCardCenter.isOwed())
    }

    @Test
    fun a_device_that_joins_while_bound_sends_its_card_at_once() = runBlocking {
        var sent = 0
        DeviceCardCenter.bindStorage(MemoryPrefs(), "acct") { sent += 1 }
        DeviceCardCenter.noteJoined("acct")
        until { sent == 1 }
        assertFalse(DeviceCardCenter.isJoinedOwed())
    }

    @Test
    fun a_join_of_another_account_waits_for_that_accounts_bind() = runBlocking {
        var sent = 0
        DeviceCardCenter.bindStorage(MemoryPrefs(), "other") { sent += 1 }
        DeviceCardCenter.noteJoined("acct")
        delay(100)
        assertEquals(0, sent)
        assertTrue(DeviceCardCenter.isJoinedOwed())
    }

    @Test
    fun a_failed_join_send_stays_owed_for_the_next_pass() = runBlocking {
        var fail = true
        var sent = 0
        DeviceCardCenter.bindStorage(MemoryPrefs(), "acct") { if (fail) error("network") else sent += 1 }
        DeviceCardCenter.noteJoined("acct")
        until { DeviceCardCenter.isOwed() }
        fail = false
        DeviceCardCenter.sendOwedIfAny()
        until { sent == 1 }
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
