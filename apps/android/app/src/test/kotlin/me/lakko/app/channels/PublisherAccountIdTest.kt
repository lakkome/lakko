// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import me.lakko.app.contacts.AcceptedContact
import me.lakko.app.contacts.AcceptedContactsStore
import me.lakko.app.contacts.IncomingContactRequest
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * ADR-0087 section 7 — a publisher whose request carried no account id (a
 * person with no display name has no profile, ADR-0048) is reached through
 * the owner's contact row, so the read key, the earlier seeds and the
 * admission seeds go to it. Seen live 2026-11-13 (base scenario 28): the
 * owner sent such a publisher nothing.
 */
@RunWith(RobolectricTestRunner::class)
class PublisherAccountIdTest {
    private lateinit var context: Context
    private val account = "b3duZXItYWNjb3VudC0wMDAwMA=="
    private val channelId = "ab".repeat(16)
    private val publisherKey = ByteArray(32) { 0xB2.toByte() }
    private val publisherKeyHex = "b2".repeat(32)
    private val publisherId = ByteArray(16) { 0x0C }

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
    }

    @After
    fun tearDown() {
        ChannelAdmissionSeedShare.forgetAccount(context, account)
        AcceptedContactsStore.delete(context, account)
    }

    private fun contact(key: ByteArray, id: ByteArray?) = AcceptedContact(
        request = IncomingContactRequest(
            messageId = 1L,
            fromPubkey = key,
            fromAccountId = id,
            fromPseudonym = ByteArray(32),
            validAfterMs = 1uL,
            intro = null,
            createdAt = "2026-11-13T00:00:00Z",
        ),
    )

    @Test
    fun a_publisher_with_no_account_id_in_the_request_is_reached_through_the_contact_row() {
        ChannelAdmissionSeedShare.rememberRequest(context, account, channelId, publisherKey, null)
        ChannelAdmissionSeedShare.notePublisher(context, account, channelId, publisherKey)
        // Not a contact yet: the publisher stays recorded, nobody to seal to.
        assertTrue(ChannelAdmissionSeedShare.fillPublisherAccountIds(context, account, channelId).isEmpty())

        AcceptedContactsStore.save(context, account, listOf(contact(publisherKey, publisherId)))
        val reachable = ChannelAdmissionSeedShare.fillPublisherAccountIds(context, account, channelId)
        assertEquals("0c".repeat(16), reachable[publisherKeyHex])
        // Kept: the next pass reads it without the contact store.
        AcceptedContactsStore.delete(context, account)
        assertEquals("0c".repeat(16), ChannelAdmissionSeedShare.fillPublisherAccountIds(context, account, channelId)[publisherKeyHex])
    }

    @Test
    fun an_account_id_the_request_carried_is_kept() {
        val carried = "11".repeat(16)
        assertEquals(carried, resolvePublisherAccountIdHex(carried, publisherKey) { listOf(contact(publisherKey, publisherId)) })
    }

    @Test
    fun someone_who_is_not_a_contact_stays_unknown() {
        val other = ByteArray(32) { 0x55 }
        assertEquals("", resolvePublisherAccountIdHex("", other) { listOf(contact(publisherKey, publisherId)) })
        assertEquals("", resolvePublisherAccountIdHex("", publisherKey) { listOf(contact(publisherKey, null)) })
    }
}
