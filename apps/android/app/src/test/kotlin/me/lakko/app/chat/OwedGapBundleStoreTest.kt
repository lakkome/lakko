// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import me.lakko.app.account.AccountLocalWipe
import me.lakko.app.account.AccountStoreGate
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * ADR-0091 phase 2 section 7 — the gap bundles a re-admitter owes are kept on
 * disk: a restart before the replacement merges still sends them, a sign-out
 * keeps them, the group's wipe and Forget device remove them.
 */
@RunWith(RobolectricTestRunner::class)
class OwedGapBundleStoreTest {
    private lateinit var context: Context
    private val account = "AAECAwQFBgcICQoLDA0ODw=="
    private val other = "EBESExQVFhcYGRobHB0eHw=="

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        AccountStoreGate.resetForTest()
    }

    @After
    fun tearDown() = AccountStoreGate.resetForTest()

    private fun owed(group: String, leaf: Byte, epoch: ULong = 6uL) =
        OwedGapBundle(group, byteArrayOf(leaf, 2, 3), epochLeft = epoch, gapFromMs = 100L, owedSinceMs = 200L)

    @Test
    fun `a bundle round-trips through a new store instance`() {
        assertTrue(SharedPrefsOwedGapBundleStore(context).put(account, owedGapBundleKey("ab", "01"), owed("ab", 1, ULong.MAX_VALUE)))
        val back = SharedPrefsOwedGapBundleStore(context).load(account)
        val row = back.getValue("ab:01")
        assertEquals("ab", row.groupIdHex)
        assertArrayEquals(byteArrayOf(1, 2, 3), row.leafSignatureKey)
        assertEquals(ULong.MAX_VALUE, row.epochLeft)
        assertEquals(100L, row.gapFromMs)
        assertEquals(200L, row.owedSinceMs)
        assertTrue("another account sees none", SharedPrefsOwedGapBundleStore(context).load(other).isEmpty())
    }

    @Test
    fun `take hands a bundle out once`() {
        val store = SharedPrefsOwedGapBundleStore(context)
        store.put(account, "ab:01", owed("ab", 1))
        assertEquals(6uL, store.take(account, "ab:01")!!.epochLeft)
        assertNull(store.take(account, "ab:01"))
        assertTrue(store.load(account).isEmpty())
    }

    @Test
    fun `the group's wipe removes only its bundles`() {
        val store = SharedPrefsOwedGapBundleStore(context)
        store.put(account, "ab:01", owed("ab", 1))
        store.put(account, "ab:02", owed("ab", 2))
        store.put(account, "cd:01", owed("cd", 1))
        store.removeGroup(account, "AB")
        assertEquals(setOf("cd:01"), store.load(account).keys)
    }

    @Test
    fun `a sealed account takes no write`() {
        val store = SharedPrefsOwedGapBundleStore(context)
        AccountStoreGate.seal(account)
        assertFalse(store.put(account, "ab:01", owed("ab", 1)))
        assertTrue(store.load(account).isEmpty())
    }

    @Test
    fun `forget device removes the account's bundles and leaves another's`() {
        val store = SharedPrefsOwedGapBundleStore(context)
        store.put(account, "ab:01", owed("ab", 1))
        store.put(other, "ab:01", owed("ab", 1))
        val step = AccountLocalWipe.steps.single { it.name == "reentry-owed-bundles" }
        AccountLocalWipe.runSteps(context, account, listOf(step))
        assertTrue(store.load(account).isEmpty())
        assertEquals(setOf("ab:01"), store.load(other).keys)
        assertTrue(AccountLocalWipe.steps.any { it.name == "reentry-gap-looks" })
    }
}
