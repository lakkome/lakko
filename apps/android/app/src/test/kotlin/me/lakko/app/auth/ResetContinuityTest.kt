// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import me.lakko.app.account.AccountStoreGate
import me.lakko.app.transparency.LogVerifyStatus
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/** ADR-0101 sections 1 to 5 - the reset side's window and previous key.
 *  Mirror of `apps/web/src/auth/reset-continuity.test.ts`. */
class ResetContinuityTest {

    private class MapStorage : MarkerStorage {
        val rows = HashMap<String, String>()
        override fun get(key: String): String? = rows[key]
        override fun set(key: String, value: String) {
            rows[key] = value
        }
        override fun remove(key: String) {
            rows.remove(key)
        }
    }

    @Before
    fun setUp() = AccountStoreGate.resetForTest()

    @After
    fun tearDown() = AccountStoreGate.resetForTest()

    @Test
    fun `the window is thirty days counted from the attestation's signed_at`() {
        assertEquals(30L * 24 * 60 * 60 * 1000, RESET_CONTINUITY_WINDOW_MS)
        assertEquals(5L + RESET_CONTINUITY_WINDOW_MS, resetContinuityExpiryMs(5L))
    }

    @Test
    fun `the mailbox is read strictly before the expiry, and never without a record`() {
        assertTrue(continuityMailboxReadable(10L, 9L))
        assertFalse(continuityMailboxReadable(10L, 10L))
        assertFalse(continuityMailboxReadable(null, 0L))
    }

    @Test
    fun `the record round-trips, and a damaged row reads as absent`() {
        val s = MapStorage()
        val store = ResetContinuityStore(s)
        val rec = ResetContinuity(expiresAtMs = 42L, previousKeyB64 = "K", keyGeneration = "3", announced = false)
        store.save("a", rec)
        assertEquals(rec, store.load("a"))
        assertTrue(announcementOwed(rec, 41L))
        assertFalse(announcementOwed(rec.withAnnounced(), 41L))
        assertFalse(announcementOwed(rec, 42L))
        assertFalse(announcementOwed(null, 0L))
        s.set(ResetContinuityStore.KEY_PREFIX + "c", """{"v":1,"expiresAtMs":42,"keyGeneration":"x"}""")
        assertNull(store.load("c"))
        s.set(ResetContinuityStore.KEY_PREFIX + "b", "{oops")
        assertNull(store.load("b"))
        store.clear("a")
        assertNull(store.load("a"))
    }

    @Test
    fun `the previous own key is the served key when the log confirms it, or no log is pinned`() {
        assertEquals("OLD", previousKeyStanding("OLD", LogVerifyStatus.CONFIRMED, "NEW"))
        assertEquals("OLD", previousKeyStanding("OLD", LogVerifyStatus.OFF, "NEW"))
    }

    @Test
    fun `the previous own key is none on any other verdict, with nothing served, or when it is the new key`() {
        for (status in listOf(LogVerifyStatus.ABSENT, LogVerifyStatus.MISMATCH, LogVerifyStatus.ROLLBACK, LogVerifyStatus.UNREACHABLE, null)) {
            assertNull(previousKeyStanding("OLD", status, "NEW"))
        }
        assertNull(previousKeyStanding(null, LogVerifyStatus.CONFIRMED, "NEW"))
        assertNull(previousKeyStanding("NEW", LogVerifyStatus.CONFIRMED, "NEW"))
    }

    @Test
    fun `forget device removes every row of the account and keeps another account's`() {
        val plain = MapStorage()
        val secret = MapStorage()
        for (p in ResetReturnStorage.PLAIN_PREFIXES) {
            plain.set(p + "a", "x")
            plain.set(p + "b", "y")
        }
        for (p in ResetReturnStorage.SECRET_PREFIXES) {
            secret.set(p + "a", "x")
            secret.set(p + "b", "y")
        }
        ResetReturnStorage.wipe(plain, secret, "a")
        assertTrue(plain.rows.keys.none { it.endsWith(":a") })
        assertTrue(secret.rows.keys.none { it.endsWith(":a") })
        assertEquals(ResetReturnStorage.PLAIN_PREFIXES.size, plain.rows.size)
        assertEquals(ResetReturnStorage.SECRET_PREFIXES.size, secret.rows.size)
    }

    @Test
    fun `the continuity cursor reads 0 when absent or damaged and round-trips`() {
        val s = MapStorage()
        assertEquals(0L, ResetReturnStorage.readContinuityCursor(s, "a"))
        ResetReturnStorage.writeContinuityCursor(s, "a", 17L)
        assertEquals(17L, ResetReturnStorage.readContinuityCursor(s, "a"))
        s.set(ResetReturnStorage.CONTINUITY_CURSOR_PREFIX + "a", "-3")
        assertEquals(0L, ResetReturnStorage.readContinuityCursor(s, "a"))
    }
}
