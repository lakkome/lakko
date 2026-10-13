// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.account

import android.content.Context
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.PersistedSession
import me.lakko.app.auth.SessionStore
import me.lakko.app.chat.KnownGroupsStore
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * A test that signs an account out does not leave it sealed for the next
 * test. The three methods are the same test: each finds the account open,
 * writes one of its stores and then signs it out, so whichever order they run
 * in, the second and the third prove the reset (no `@Before` or `@After` here
 * does it; it is [me.lakko.app.LakkoTestApplication]).
 */
@RunWith(RobolectricTestRunner::class)
class StoreGateIsolationTest {

    private val account = "Dg4ODg4ODg4ODg4ODg4ODg=="

    private fun writeThenSignOut() {
        val context: Context = RuntimeEnvironment.getApplication()
        assertFalse("sealed by an earlier test", AccountStoreGate.isSealed(account))
        val known = KnownGroupsStore(context)
        val group = "ab".repeat(32)
        known.add(account, group)
        assertEquals(setOf(group), known.load(account))

        val sessions = SessionStore(context)
        sessions.save(PersistedSession(accountIdB64 = account, username = "alice"))
        AuthRepository(context, sessionStore = sessions).logout()
        assertTrue(AccountStoreGate.isSealed(account))
    }

    @Test
    fun `first - the account is open, and signing out seals it`() = writeThenSignOut()

    @Test
    fun `second - the account is open, and signing out seals it`() = writeThenSignOut()

    @Test
    fun `third - the account is open, and signing out seals it`() = writeThenSignOut()
}
