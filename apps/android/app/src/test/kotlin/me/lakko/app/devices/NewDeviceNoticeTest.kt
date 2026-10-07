// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import me.lakko.app.auth.MarkerStorage
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** `docs/ux/identity-recovery.md` 17.5: the new-device notice's acknowledge
 *  and suppress rules, and its persistence. */
class NewDeviceNoticeTest {

    private class MapStorage : MarkerStorage {
        val map = HashMap<String, String>()
        override fun get(key: String): String? = map[key]
        override fun set(key: String, value: String) {
            map[key] = value
        }
        override fun remove(key: String) {
            map.remove(key)
        }
    }

    private val self = "aa"
    private val seeded = NewDeviceNoticeState(seeded = true, acknowledged = setOf("aa", "bb"))

    @Test
    fun `the first roster a device sees is known - it joined, nobody was added`() {
        val (state, fresh) = observeRoster(NewDeviceNoticeState(), setOf("aa", "bb", "cc"), self, nowMs = 5L)
        assertTrue(state.seeded)
        assertEquals(setOf("aa", "bb", "cc"), state.acknowledged)
        assertTrue(state.pending.isEmpty())
        assertTrue(fresh.isEmpty())
        assertNull(newDeviceBanner(state))
    }

    @Test
    fun `a device that only knew itself treats the account it joins as known`() {
        // It founded a group of one before it was let in; the roster it then
        // sees is the account as it was when it joined.
        val alone = NewDeviceNoticeState(seeded = true, acknowledged = setOf("aa"))
        val (state, fresh) = observeRoster(alone, setOf("aa", "bb", "cc"), self, nowMs = 5L)
        assertTrue(fresh.isEmpty())
        assertNull(newDeviceBanner(state))
        assertEquals(setOf("aa", "bb", "cc"), state.acknowledged)
    }

    @Test
    fun `a device this one has not acknowledged raises the notice, dated when it was learned`() {
        val (state, fresh) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, nowMs = 1_700L)
        assertEquals(setOf("cc"), fresh)
        assertEquals(NewDeviceBanner(count = 1, latestAtMs = 1_700L), newDeviceBanner(state))
        assertTrue(isNewDevice(state, "cc"))
        assertFalse(isNewDevice(state, "bb"))
    }

    @Test
    fun `seeing the same roster again says nothing new and keeps the first date`() {
        val (once, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, nowMs = 1_700L)
        val (twice, fresh) = observeRoster(once, setOf("aa", "bb", "cc"), self, nowMs = 9_999L)
        assertTrue(fresh.isEmpty())
        assertEquals(once, twice)
    }

    @Test
    fun `two new devices are one banner with the most recent date`() {
        val (one, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, nowMs = 100L)
        val (two, fresh) = observeRoster(one, setOf("aa", "bb", "cc", "dd"), self, nowMs = 200L)
        assertEquals(setOf("dd"), fresh)
        assertEquals(NewDeviceBanner(count = 2, latestAtMs = 200L), newDeviceBanner(two))
    }

    @Test
    fun `a device never shows the notice about itself`() {
        // Its own key missing from the acknowledged set (a re-keyed leaf).
        val state = NewDeviceNoticeState(seeded = true, acknowledged = setOf("bb"))
        val (next, fresh) = observeRoster(state, setOf("aa", "bb"), selfKey = "aa", nowMs = 1L)
        assertTrue(fresh.isEmpty())
        assertNull(newDeviceBanner(next))
    }

    @Test
    fun `the device that let it in shows nothing`() {
        // The scan or the code route ran here: the roster is acknowledged
        // before the sync runtime reports it.
        val acknowledged = acknowledgeRoster(seeded, setOf("aa", "bb", "cc"))
        val (state, fresh) = observeRoster(acknowledged, setOf("aa", "bb", "cc"), self, nowMs = 1L)
        assertTrue(fresh.isEmpty())
        assertNull(newDeviceBanner(state))
    }

    @Test
    fun `letting a device in does not hide a notice about another one's`() {
        val (pending, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, nowMs = 1L)
        // This device then lets `dd` in; `cc` was already in the roster it
        // acknowledges, so it is acknowledged with it.
        val after = acknowledgeRoster(pending, setOf("aa", "bb", "cc", "dd"))
        assertNull(newDeviceBanner(after))
        assertTrue(after.acknowledged.containsAll(setOf("cc", "dd")))
    }

    @Test
    fun `dismiss ends the notice and the New labels with it, and it does not come back`() {
        val (pending, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, nowMs = 1L)
        val dismissed = dismissNotice(pending)
        assertNull(newDeviceBanner(dismissed))
        assertFalse(isNewDevice(dismissed, "cc"))
        val (again, fresh) = observeRoster(dismissed, setOf("aa", "bb", "cc"), self, nowMs = 2L)
        assertTrue(fresh.isEmpty())
        assertNull(newDeviceBanner(again))
    }

    @Test
    fun `a pending device that left the roster ends its notice`() {
        val (pending, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, nowMs = 1L)
        val (after, _) = observeRoster(pending, setOf("aa", "bb"), self, nowMs = 2L)
        assertNull(newDeviceBanner(after))
        // Removed elsewhere: no "removed" line on this device.
        assertFalse(after.removedNew)
    }

    @Test
    fun `removing a New device here replaces the banner with the change-password line`() {
        val (pending, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, nowMs = 1L)
        val removed = removedHere(pending, "cc", wasNew = true)
        assertNull(newDeviceBanner(removed))
        assertTrue(removed.removedNew)
        // The roster that follows the removal changes nothing more.
        val (after, _) = observeRoster(removed, setOf("aa", "bb"), self, nowMs = 2L)
        assertTrue(after.removedNew)
        assertFalse(dismissRemovedLine(after).removedNew)
    }

    @Test
    fun `removing a device that carried no New label says nothing`() {
        assertEquals(seeded, removedHere(seeded, "bb", wasNew = false))
    }

    @Test
    fun `the notice is kept across restarts, per account, until the wipe`() {
        val storage = MapStorage()
        val store = NewDeviceNoticeStore(storage)
        assertEquals(NewDeviceNoticeState(), store.load("acct-a"))
        val (pending, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, nowMs = 42L)
        store.save("acct-a", pending)
        // A new process reads the same row.
        assertEquals(pending, NewDeviceNoticeStore(storage).load("acct-a"))
        assertEquals(NewDeviceNoticeState(), store.load("acct-b"))
        store.deleteAccount("acct-a")
        assertEquals(NewDeviceNoticeState(), store.load("acct-a"))
    }

    @Test
    fun `an unreadable row reads as never seeded, so nothing undated is announced`() {
        val storage = MapStorage()
        storage.set("acct-a", "not json")
        val loaded = NewDeviceNoticeStore(storage).load("acct-a")
        assertFalse(loaded.seeded)
        val (state, fresh) = observeRoster(loaded, setOf("aa", "bb"), self, nowMs = 1L)
        assertTrue(fresh.isEmpty())
        assertNull(newDeviceBanner(state))
    }

    @Test
    fun `the device key is the lowercase hex of the leaf's signature key`() {
        assertEquals("00ff10", newDeviceKey(byteArrayOf(0, -1, 16)))
    }
}
