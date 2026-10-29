// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import me.lakko.app.auth.MarkerStorage
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** `docs/ux/identity-recovery.md` 17.5 / 17.6: the new-device notice's
 *  known / pending / acknowledged rules, the Devices visit, the notification
 *  decision, and its persistence. */
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
    private val none = emptySet<String>()
    private val seeded = NewDeviceNoticeState(seeded = true, known = setOf("aa", "bb"))

    private fun pendingOn(state: NewDeviceNoticeState, acked: Set<String> = none) =
        effectivePending(state, acked).keys

    @Test
    fun `the first roster a device sees is known - it joined, nobody was added`() {
        val (state, fresh) = observeRoster(NewDeviceNoticeState(), setOf("aa", "bb", "cc"), self, none, nowMs = 5L)
        assertTrue(state.seeded)
        assertEquals(setOf("aa", "bb", "cc"), state.known)
        assertTrue(fresh.isEmpty())
        assertFalse(attentionVisible(state, none))
    }

    @Test
    fun `a device that only knew itself treats the account it joins as known`() {
        // It founded a group of one before it was let in; the roster it then
        // sees is the account as it was when it joined, even with
        // acknowledgements it already holds for other devices.
        val alone = NewDeviceNoticeState(seeded = true, known = setOf("aa"))
        val (state, fresh) = observeRoster(alone, setOf("aa", "bb", "cc"), self, setOf("zz"), nowMs = 5L)
        assertTrue(fresh.isEmpty())
        assertFalse(attentionVisible(state, setOf("zz")))
        assertEquals(setOf("aa", "bb", "cc"), state.known)
    }

    @Test
    fun `a new device is pending, dated when it was learned, and the badge is up`() {
        val (state, fresh) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, none, nowMs = 1_700L)
        assertEquals(setOf("cc"), fresh)
        assertEquals(mapOf("cc" to 1_700L), effectivePending(state, none))
        assertTrue(attentionVisible(state, none))
        assertEquals(NewDeviceExplanation(count = 1, latestAtMs = 1_700L), newDeviceExplanation(effectivePending(state, none)))
    }

    @Test
    fun `seeing the same roster again says nothing new and keeps the first date`() {
        val (once, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, none, nowMs = 1_700L)
        val (twice, fresh) = observeRoster(once, setOf("aa", "bb", "cc"), self, none, nowMs = 9_999L)
        assertTrue(fresh.isEmpty())
        assertEquals(once, twice)
        assertEquals(NoticeNotification.Keep, noticeNotification(fresh, effectivePending(twice, none)))
    }

    @Test
    fun `the notification is posted for a new device, whatever is in front, and counts every pending one`() {
        val (one, freshOne) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, none, nowMs = 100L)
        assertEquals(NoticeNotification.Post(1), noticeNotification(freshOne, effectivePending(one, none)))
        val (two, freshTwo) = observeRoster(one, setOf("aa", "bb", "cc", "dd"), self, none, nowMs = 200L)
        assertEquals(setOf("dd"), freshTwo)
        assertEquals(NoticeNotification.Post(2), noticeNotification(freshTwo, effectivePending(two, none)))
        assertEquals(NewDeviceExplanation(count = 2, latestAtMs = 200L), newDeviceExplanation(effectivePending(two, none)))
    }

    @Test
    fun `an acknowledged device is never pending`() {
        val (pending, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, none, nowMs = 1L)
        assertTrue(attentionVisible(pending, none))
        assertFalse(attentionVisible(pending, setOf("cc")))
        assertTrue(pendingOn(pending, setOf("cc")).isEmpty())
    }

    @Test
    fun `a device learned of after it was acknowledged elsewhere never shows anything`() {
        val (state, fresh) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, setOf("cc"), nowMs = 1L)
        assertTrue(fresh.isEmpty())
        assertTrue(state.pending.isEmpty())
        assertTrue("cc" in state.known)
        // And it stays known if the acknowledged set is not passed again.
        val (later, freshLater) = observeRoster(state, setOf("aa", "bb", "cc"), self, none, nowMs = 2L)
        assertTrue(freshLater.isEmpty())
        assertFalse(attentionVisible(later, none))
    }

    @Test
    fun `an acknowledgement received from another device clears pending and withdraws the notification`() {
        val (pending, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, none, nowMs = 1L)
        val after = applyAcknowledged(pending, setOf("cc"))
        assertTrue(after.pending.isEmpty())
        assertTrue("cc" in after.known)
        assertEquals(NoticeNotification.Withdraw, noticeNotification(emptySet(), effectivePending(after, setOf("cc"))))
    }

    @Test
    fun `an acknowledgement of one device leaves another pending and the notification up`() {
        val (one, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, none, nowMs = 1L)
        val (two, _) = observeRoster(one, setOf("aa", "bb", "cc", "dd"), self, none, nowMs = 2L)
        val after = applyAcknowledged(two, setOf("cc"))
        assertEquals(setOf("dd"), pendingOn(after))
        assertEquals(NoticeNotification.Keep, noticeNotification(emptySet(), effectivePending(after, setOf("cc"))))
    }

    @Test
    fun `opening Devices with pending yields the keys to acknowledge and a visit snapshot`() {
        val (one, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, none, nowMs = 10L)
        val (two, _) = observeRoster(one, setOf("aa", "bb", "cc", "dd"), self, none, nowMs = 20L)
        val seen = seeDevices(two, none, emptyMap())
        assertEquals(setOf("cc", "dd"), seen.toAcknowledge)
        assertEquals(mapOf("cc" to 10L, "dd" to 20L), seen.visit)
        // The badge and the notification go at once.
        assertFalse(attentionVisible(seen.state, none))
        assertEquals(NoticeNotification.Withdraw, noticeNotification(emptySet(), effectivePending(seen.state, none)))
        // The rows stay highlighted for this visit, with the explanation.
        assertEquals(setOf("cc", "dd"), highlightedDevices(seen.state, none, seen.visit).keys)
        assertEquals(NewDeviceExplanation(2, 20L), newDeviceExplanation(highlightedDevices(seen.state, none, seen.visit)))
        // Leaving the screen ends the visit: nothing is highlighted.
        assertTrue(highlightedDevices(seen.state, none, null).isEmpty())
    }

    @Test
    fun `this device's own acknowledgement arriving back keeps the visit's highlight`() {
        val (pending, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, none, nowMs = 1L)
        val seen = seeDevices(pending, none, emptyMap())
        val after = applyAcknowledged(seen.state, seen.toAcknowledge)
        assertEquals(setOf("cc"), highlightedDevices(after, seen.toAcknowledge, seen.visit).keys)
    }

    @Test
    fun `opening Devices with nothing pending acknowledges nothing`() {
        val seen = seeDevices(seeded, none, emptyMap())
        assertTrue(seen.toAcknowledge.isEmpty())
        assertTrue(seen.visit.isEmpty())
        assertEquals(seeded, seen.state)
        // A device acknowledged elsewhere is not acknowledged a second time.
        val (pending, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, none, nowMs = 1L)
        assertTrue(seeDevices(pending, setOf("cc"), emptyMap()).toAcknowledge.isEmpty())
    }

    @Test
    fun `opening Devices before the synced state is ready consumes nothing, then sees on ready`() {
        val (pending, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, none, nowMs = 7L)
        // Opened before the synced state is open: nothing is acknowledged,
        // nothing moves to known, the row stays highlighted and the badge up.
        val early = seeDevices(pending, none, emptyMap(), syncReady = false)
        assertTrue(early.toAcknowledge.isEmpty())
        assertEquals(pending, early.state)
        assertTrue(early.visit.isEmpty())
        assertTrue(attentionVisible(early.state, none))
        assertEquals(setOf("cc"), highlightedDevices(early.state, none, early.visit).keys)
        // The state becomes ready while the screen is still open.
        val ready = seeDevices(early.state, none, early.visit, syncReady = true)
        assertEquals(setOf("cc"), ready.toAcknowledge)
        assertEquals(mapOf("cc" to 7L), ready.visit)
        assertFalse(attentionVisible(ready.state, none))
        assertEquals(setOf("cc"), highlightedDevices(ready.state, none, ready.visit).keys)
    }

    @Test
    fun `a device arriving while Devices is open joins the visit`() {
        val (one, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, none, nowMs = 1L)
        val first = seeDevices(one, none, emptyMap())
        val (two, _) = observeRoster(first.state, setOf("aa", "bb", "cc", "dd"), self, none, nowMs = 2L)
        val second = seeDevices(two, none, first.visit)
        assertEquals(setOf("dd"), second.toAcknowledge)
        assertEquals(setOf("cc", "dd"), second.visit.keys)
    }

    @Test
    fun `a device never shows the notice about itself`() {
        // Its own key missing from the known set (a re-keyed leaf).
        val state = NewDeviceNoticeState(seeded = true, known = setOf("bb"))
        val (next, fresh) = observeRoster(state, setOf("aa", "bb"), selfKey = "aa", acknowledged = none, nowMs = 1L)
        assertTrue(fresh.isEmpty())
        assertFalse(attentionVisible(next, none))
    }

    @Test
    fun `the device that let it in shows nothing`() {
        // The scan or the code route ran here: the roster is known before
        // the sync runtime reports it.
        val letIn = acknowledgeRoster(seeded, setOf("aa", "bb", "cc"))
        val (state, fresh) = observeRoster(letIn, setOf("aa", "bb", "cc"), self, none, nowMs = 1L)
        assertTrue(fresh.isEmpty())
        assertFalse(attentionVisible(state, none))
    }

    @Test
    fun `letting a device in makes the roster known here`() {
        val (pending, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, none, nowMs = 1L)
        val after = acknowledgeRoster(pending, setOf("aa", "bb", "cc", "dd"))
        assertFalse(attentionVisible(after, none))
        assertTrue(after.known.containsAll(setOf("cc", "dd")))
    }

    @Test
    fun `a pending device that left the roster ends its notice`() {
        val (pending, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, none, nowMs = 1L)
        val (after, fresh) = observeRoster(pending, setOf("aa", "bb"), self, none, nowMs = 2L)
        assertFalse(attentionVisible(after, none))
        assertEquals(NoticeNotification.Withdraw, noticeNotification(fresh, effectivePending(after, none)))
        // Removed elsewhere: no "removed" line on this device.
        assertFalse(after.removedNew)
    }

    @Test
    fun `removing a New device here shows the change-password line`() {
        val (pending, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, none, nowMs = 1L)
        val removed = removedHere(pending, "cc", wasNew = true)
        assertFalse(attentionVisible(removed, none))
        assertTrue(removed.removedNew)
        // The roster that follows the removal changes nothing more.
        val (after, _) = observeRoster(removed, setOf("aa", "bb"), self, none, nowMs = 2L)
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
        val (pending, _) = observeRoster(seeded, setOf("aa", "bb", "cc"), self, none, nowMs = 42L)
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
        val (state, fresh) = observeRoster(loaded, setOf("aa", "bb"), self, none, nowMs = 1L)
        assertTrue(fresh.isEmpty())
        assertFalse(attentionVisible(state, none))
    }

    @Test
    fun `the device key is the lowercase hex of the leaf's signature key, and back`() {
        assertEquals("00ff10", newDeviceKey(byteArrayOf(0, -1, 16)))
        val key = ByteArray(32) { it.toByte() }
        assertTrue(key.contentEquals(newDeviceKeyBytes(newDeviceKey(key))))
        assertNull(newDeviceKeyBytes("00ff10"))
        assertNull(newDeviceKeyBytes("zz".repeat(32)))
    }
}
