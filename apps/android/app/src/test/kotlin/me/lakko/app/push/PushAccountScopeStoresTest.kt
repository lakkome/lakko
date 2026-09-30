// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import androidx.core.app.NotificationCompat
import me.lakko.app.auth.AuthRepository
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf

/**
 * ADR-0042, amended 2026-09-30, on the stores the FCM service reads: the
 * reverse map is keyed by `(account, push_gid)` with account-scoped deletes,
 * the wake binding round-trips, and Log out / Forget device clear the binding
 * and this app's delivered notifications.
 */
@RunWith(RobolectricTestRunner::class)
class PushAccountScopeStoresTest {

    private lateinit var context: Context
    private val accountA = "QUFBQUFBQUFBQUFBQUFBQQ=="
    private val accountB = "QkJCQkJCQkJCQkJCQkJCQg=="
    private val pushGid = "cc".repeat(32)
    private val groupA = "aa".repeat(32)
    private val groupB = "bb".repeat(32)

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        PushWakeBindingStore(context).clear()
    }

    @Test
    fun `two accounts in one group keep separate route rows`() {
        val store = PushRouteStore(context)
        store.put(accountA, pushGid, groupA)
        store.put(accountB, pushGid.uppercase(), groupB)

        assertEquals(groupA, store.lookup(accountA, pushGid))
        assertEquals(groupB, store.lookup(accountB, pushGid))
        assertNull(store.lookup("QzM=", pushGid))
    }

    @Test
    fun `a route delete is scoped to its account`() {
        val store = PushRouteStore(context)
        store.put(accountA, pushGid, groupA)
        store.put(accountB, pushGid, groupB)

        store.remove(accountA, pushGid)
        assertNull(store.lookup(accountA, pushGid))
        assertEquals("the other account's row survives", groupB, store.lookup(accountB, pushGid))

        store.put(accountA, pushGid, groupA)
        store.clearForAccount(accountB)
        assertNull(store.lookup(accountB, pushGid))
        assertEquals(groupA, store.lookup(accountA, pushGid))
    }

    @Test
    fun `the wake binding round-trips and clears`() {
        val store = PushWakeBindingStore(context)
        assertNull(store.load())
        val pseudonym = ByteArray(32) { 0x0A }
        store.bind(pseudonym, accountA)
        val loaded = store.load()
        assertEquals(PushWakeBinding("0a".repeat(32), accountA), loaded)
        assertEquals(accountA, wakeAccountFor("0a".repeat(32), loaded))

        store.clear()
        assertNull(store.load())
    }

    @Test
    fun `sign-out clears the binding and the delivered notifications`() {
        bindAndNotify()
        PushSignOut.clearFirst(context)
        assertNothingLeft()
    }

    @Test
    fun `Log out clears the binding and the delivered notifications`() {
        val auth = AuthRepository(context)
        bindAndNotify()
        auth.logout()
        assertNothingLeft()
    }

    @Test
    fun `Forget device clears the binding and the delivered notifications`() {
        val auth = AuthRepository(context)
        bindAndNotify()
        auth.forgetDevice()
        assertNothingLeft()
    }

    private fun bindAndNotify() {
        PushWakeBindingStore(context).bind(ByteArray(32) { 0x0A }, accountA)
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel("test", "test", NotificationManager.IMPORTANCE_HIGH),
        )
        manager.notify(
            7,
            NotificationCompat.Builder(context, "test")
                .setSmallIcon(android.R.drawable.sym_def_app_icon)
                .setContentText("a message for account A")
                .build(),
        )
        assertNotNull(PushWakeBindingStore(context).load())
        assertEquals(1, shadowOf(manager).allNotifications.size)
    }

    private fun assertNothingLeft() {
        assertNull("the wake binding is gone", PushWakeBindingStore(context).load())
        val manager = context.getSystemService(NotificationManager::class.java)
        assertTrue("no delivered notification survives", shadowOf(manager).allNotifications.isEmpty())
    }
}
