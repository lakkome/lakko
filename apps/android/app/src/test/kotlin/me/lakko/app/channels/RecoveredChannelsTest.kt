// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import me.lakko.app.account.AccountStoreGate
import me.lakko.app.auth.MarkerStorage
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/** ADR-0101 section 7 - a channel taken back with its recovery key keeps
 *  every reader the service listed at the recovery; the returned-invitation
 *  list. Mirror of `apps/web/src/channels/recovered-channels.test.ts`. */
class RecoveredChannelsTest {

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
    fun `a re-founding keeps a recovered channel's readers, and judges a lost one from the start`() {
        assertEquals(42L, refoundAdmittedThrough(RecoveredChannel(42L)))
        assertEquals(0L, refoundAdmittedThrough(null))
    }

    @Test
    fun `the mark is the highest listed row, and only ever rises`() {
        assertEquals(9L, highestSeq(listOf(3L, 9L, 1L)))
        assertEquals(5L, highestSeq(emptyList(), 5L))
        val store = RecoveredChannelsStore(MapStorage())
        store.mark("a", "c", 9L)
        store.mark("a", "c", 4L)
        assertEquals(mapOf("c" to RecoveredChannel(9L)), store.read("a"))
        store.clear("a", "c")
        assertTrue(store.read("a").isEmpty())
    }

    @Test
    fun `the type comes from the newest record, none while none is held`() {
        assertEquals(RecoveredVisibility.PUBLIC, recoveredVisibility(true))
        assertEquals(RecoveredVisibility.PRIVATE, recoveredVisibility(false))
        assertNull(recoveredVisibility(null))
    }

    @Test
    fun `returned invitations are shown until the account reads the channel, never dropped otherwise`() {
        val c1 = "11".repeat(16)
        val c2 = "22".repeat(16)
        val store = ReturnedInvitationsStore(MapStorage())
        store.keep("a", c2, "card2")
        store.keep("a", c1, "card1")
        store.keep("a", c1, "card1b")
        val held = store.read("a")
        assertEquals(listOf(ShownInvitation(c1, "card1b"), ShownInvitation(c2, "card2")), invitationsToShow(held, emptySet()))
        assertEquals(listOf(c2), invitationsToShow(held, setOf(c1)).map { it.channelIdHex })
        store.drop("a", listOf(c1))
        assertEquals(setOf(c2), store.read("a").keys)
        // A row that is not a channel id is never shown.
        assertTrue(ReturnedInvitationsStore(MapStorage().also { it.set(ReturnedInvitationsStore.KEY_PREFIX + "a", """{"zz":"x"}""") }).read("a").isEmpty())
    }
}
