// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import me.lakko.app.auth.AuthRepository
import org.junit.After
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * `docs/ux/conversation-info.md` — removing a contact is local and leaves the
 * conversation. The one-to-one adoption rule must not bring the person back,
 * not at once and not after a relaunch (seen on web 2026-10-24).
 */
@RunWith(RobolectricTestRunner::class)
class RemovedContactStaysRemovedTest {

    private val account = ByteArray(16) { 0x0C }
    private val peer = ByteArray(32) { 0x21 }
    private val other = ByteArray(32) { 0x22 }

    @Before
    fun setUp() = AcceptedContactsStore.unbind()

    @After
    fun tearDown() = AcceptedContactsStore.unbind()

    private fun repo(): ContactsRepository {
        val context = RuntimeEnvironment.getApplication()
        return ContactsRepository(authRepo = AuthRepository(context), appContext = context).also {
            it.bindAccount(account)
        }
    }

    private fun ContactsRepository.adopt(key: ByteArray, group: String) =
        adoptOneToOnePeer(key, null, group, 1uL, "2026-10-24T00:00:00Z")

    @Test
    fun `a removed contact is not brought back by their one-to-one, also after a relaunch`() {
        val first = repo()
        assertTrue(first.adopt(peer, "beef"))
        val row = first.state.value.accepted.single { it.request.fromPubkey.contentEquals(peer) }
        first.removeContact(row.request.messageId)
        assertTrue(first.state.value.accepted.isEmpty())

        assertFalse(first.adopt(peer, "beef"))
        assertTrue(first.state.value.accepted.isEmpty())

        val relaunched = repo()
        assertFalse(relaunched.adopt(peer, "beef"))
        assertTrue(relaunched.state.value.accepted.isEmpty())
        // Another person's one-to-one is still adopted.
        assertTrue(relaunched.adopt(other, "cafe"))
    }
}
