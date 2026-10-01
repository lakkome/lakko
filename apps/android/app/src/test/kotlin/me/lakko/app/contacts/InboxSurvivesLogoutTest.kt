// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import java.util.Base64
import me.lakko.app.auth.AuthRepository
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * The stranger-inbox cursor and pending list are per account, so a log out
 * keeps them: deleting them there made every request the person had declined
 * come back after the next login (the cursor restarted at 0). Only forget
 * device drops them, and only the departing account's.
 */
@RunWith(RobolectricTestRunner::class)
class InboxSurvivesLogoutTest {

    private val accountA = ByteArray(16) { 0x0A }
    private val accountB = ByteArray(16) { 0x0B }
    private val aB64 = Base64.getEncoder().encodeToString(accountA)
    private val bB64 = Base64.getEncoder().encodeToString(accountB)

    private fun request(id: Long) = IncomingContactRequest(
        messageId = id,
        fromPubkey = ByteArray(32) { id.toByte() },
        fromAccountId = null,
        fromPseudonym = ByteArray(32),
        validAfterMs = 1_700_000_000_000uL,
        intro = null,
        createdAt = "2026-09-30T10:00:00Z",
    )

    @Before
    fun setUp() = AcceptedContactsStore.unbind()

    @After
    fun tearDown() = AcceptedContactsStore.unbind()

    @Test
    fun `log out keeps the inbox and forget device drops only that account's`() {
        val context = RuntimeEnvironment.getApplication()
        val store = SharedPreferencesContactsInboxStore(context)
        store.saveCursor(aB64, 42L)
        store.savePending(aB64, listOf(request(1)))
        store.saveCursor(bB64, 7L)
        val repo = ContactsRepository(
            authRepo = AuthRepository(RuntimeEnvironment.getApplication()),
            appContext = null,
            inboxStore = store,
        )

        repo.bindAccount(accountA)
        assertEquals(42L, repo.state.value.cursor)
        assertEquals(listOf(1L), repo.state.value.pending.map { it.messageId })

        // Log out.
        repo.clear()
        assertEquals(42L, store.load(aB64).cursor)
        assertEquals(listOf(1L), store.load(aB64).pending.map { it.messageId })

        // The next login of the same account resumes where it was.
        repo.bindAccount(accountA)
        assertEquals(42L, repo.state.value.cursor)
        assertEquals(listOf(1L), repo.state.value.pending.map { it.messageId })

        // Forget device: the session ends, then the one on-device wipe runs.
        repo.clear()
        me.lakko.app.account.AccountLocalWipe.wipeAccountLocalData(context, aB64)
        assertEquals(0L, store.load(aB64).cursor)
        assertTrue(store.load(aB64).pending.isEmpty())
        assertEquals(7L, store.load(bB64).cursor)
    }
}
