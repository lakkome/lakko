// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import kotlinx.coroutines.test.runTest
import me.lakko.app.auth.AuthRepository
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/** A failed accept leaves the request listed, and files no contact without a
 *  chat. */
@RunWith(RobolectricTestRunner::class)
class AcceptKeepsRequestTest {

    private val account = ByteArray(16) { 0x0D }
    private val accountB64 = java.util.Base64.getEncoder().encodeToString(account)

    private fun request(id: Long) = IncomingContactRequest(
        messageId = id,
        fromPubkey = ByteArray(32) { 0x31 },
        fromAccountId = ByteArray(16) { 0x32 },
        fromPseudonym = ByteArray(32),
        validAfterMs = 1_700_000_000_000uL,
        intro = null,
        createdAt = "2026-10-30T10:00:00Z",
    )

    @Before
    fun setUp() = AcceptedContactsStore.unbind()

    @After
    fun tearDown() = AcceptedContactsStore.unbind()

    @Test
    fun `an accept that cannot complete keeps the request and adds no contact`() = runTest {
        val context = RuntimeEnvironment.getApplication()
        val store = SharedPreferencesContactsInboxStore(context)
        store.savePending(accountB64, listOf(request(1)))
        val repo = ContactsRepository(authRepo = AuthRepository(context), appContext = null, inboxStore = store)
        repo.bindAccount(account)

        assertEquals(AcceptOutcome.FAILED, repo.acceptRequest(1))
        assertEquals(listOf(1L), repo.state.value.pending.map { it.messageId })
        assertTrue(repo.state.value.accepted.isEmpty())
        // Accept works again: the same answer, nothing used up.
        assertEquals(AcceptOutcome.FAILED, repo.acceptRequest(1))
        assertEquals(listOf(1L), repo.state.value.pending.map { it.messageId })
    }
}
