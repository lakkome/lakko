// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import me.lakko.app.chat.ParsedGroupInvite
import me.lakko.app.chat.parseGroupInviteSlug
import me.lakko.app.channels.CoPublisher
import me.lakko.app.contacts.LakkoLink
import me.lakko.app.contacts.lakkoLinkAt
import me.lakko.app.discovery.InviteLinkArgument
import me.lakko.app.discovery.InviteLinkLengths
import me.lakko.app.discovery.composeSealedInviteUrl
import me.lakko.app.discovery.inviteLinkSecretOf
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertNotNull
import org.junit.Test

/**
 * A publisher invitation's `Accept` acts on the same link the card opened.
 *
 * Seen on a live walk on web: the card opened the link and `Accept` then
 * reported the link as invalid. On Android the card rebuilds the link from
 * its ledger id and secret, and `Accept` passes it to the co-publisher join;
 * this pins that the join reads back exactly that ledger id and secret
 * (the parse steps of `CoPublisher.requestToCoPublish`, which needs the
 * native core to run whole).
 */
class ChannelInviteAcceptLinkTest {
    private val lengths = InviteLinkLengths(blobIdLen = 6, blobIdUrlLen = 8, linkSecretLen = 12, linkSecretUrlLen = 16)

    @Test
    fun `accept hands the join the card's ledger id and secret`() {
        val ledgerId = byteArrayOf(1, 2, 3, 4, 5, 6)
        val secret = ByteArray(12) { (it * 7 + 3).toByte() }
        for (origin in listOf("https://test.lakko.me", "https://lakko.me")) {
            val url = composeSealedInviteUrl(origin, "/g/", ledgerId, secret, lengths)
            val link = lakkoLinkAt(url, origin) as? LakkoLink.Group
            assertNotNull("card link parses on $origin", link)

            val joinUrl = coPublisherJoinUrl(link!!)
            val idx = joinUrl.lastIndexOf(CoPublisher.URL_PREFIX)
            val argument = InviteLinkArgument.decode(joinUrl.substring(idx + CoPublisher.URL_PREFIX.length).trim())
            val readSecret = inviteLinkSecretOf(argument.fragment, lengths)
            assertNotNull("secret survives on $origin", readSecret)
            assertArrayEquals(secret, readSecret)
            val pointer = parseGroupInviteSlug(argument.slug.trimEnd('/'), lengths) as ParsedGroupInvite.Pointer
            assertArrayEquals(ledgerId, pointer.blobId)
        }
    }
}
